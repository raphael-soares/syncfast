//! Finding and running rclone. Every call uses explicit arguments, no shell
//! and a null stdin. The output of `rclone config dump` holds secrets: it is
//! parsed in memory and never logged, stored or put in an error.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use serde::Deserialize;
use tokio::process::{Child, Command};

use crate::flatpak;
use crate::paths::Paths;

const BINARY_NAME: &str = "rclone";
const BINARY_OVERRIDE_VAR: &str = "SYNCFAST_RCLONE";
const CONFIG_VAR: &str = "RCLONE_CONFIG";
const DRIVE_TYPE: &str = "drive";
/// rclone's exit codes for a folder and for a file that is not there.
const NOT_FOUND: i32 = 3;
const FILE_NOT_FOUND: i32 = 4;
/// How long the short calls (`listremotes`, `about`, `lsjson`...) may take
/// before they count as failed. Rounds and sign-ins have no such limit.
pub(crate) const SHORT_CALL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum RcloneError {
    #[error("rclone not found; install rclone or reinstall syncfast units")]
    RcloneNotFound,
    #[error("cannot run rclone: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("rclone {command} failed: {detail}")]
    Failed {
        command: &'static str,
        detail: String,
    },
    #[error("unexpected output from rclone {0}")]
    BadOutput(&'static str),
}

/// A resolved rclone binary and, optionally, the config file it must use.
/// Tests and the dev mode pass the config here instead of through process
/// environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rclone {
    bin: PathBuf,
    config: Option<PathBuf>,
}

impl Rclone {
    #[must_use]
    pub fn new(bin: PathBuf) -> Self {
        Self { bin, config: None }
    }

    /// Runs rclone with this `RCLONE_CONFIG`, whatever the environment says.
    #[must_use]
    pub fn with_config(mut self, config: PathBuf) -> Self {
        self.config = Some(config);
        self
    }

    /// `SYNCFAST_RCLONE` if set, otherwise `rclone` on `PATH`. The rest of
    /// the environment, `RCLONE_CONFIG` included, is inherited. Inside the
    /// Flatpak sandbox, where the config folder is remapped, an unset
    /// `RCLONE_CONFIG` becomes the host's `~/.config/rclone/rclone.conf`
    /// (ADR 0006).
    ///
    /// # Errors
    /// [`RcloneError::RcloneNotFound`].
    pub fn from_env() -> Result<Self, RcloneError> {
        let bin = resolve_binary(
            std::env::var_os(BINARY_OVERRIDE_VAR).as_deref(),
            std::env::var_os("PATH").as_deref(),
        )?;
        let rclone = Self::new(bin);
        let configured = std::env::var_os(CONFIG_VAR).is_some_and(|value| !value.is_empty());
        let config = Paths::from_env()
            .ok()
            .and_then(|paths| sandbox_config(flatpak::app_id().as_deref(), configured, &paths));
        Ok(match config {
            Some(config) => rclone.with_config(config),
            None => rclone,
        })
    }

    #[must_use]
    pub fn bin(&self) -> &Path {
        &self.bin
    }

    /// A command for `rclone <args>` with a null stdin. The caller decides
    /// where stdout and stderr go and spawns it.
    pub fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(&self.bin);
        command.args(args).stdin(Stdio::null()).kill_on_drop(true);
        if let Some(config) = &self.config {
            command.env("RCLONE_CONFIG", config);
        }
        command
    }

    async fn output(
        &self,
        name: &'static str,
        args: Vec<OsString>,
        keep_stderr: bool,
    ) -> Result<Vec<u8>, RcloneError> {
        self.output_within(SHORT_CALL_TIMEOUT, name, args, keep_stderr)
            .await
    }

    /// The child is killed when the wait runs out: `kill_on_drop`.
    async fn output_within(
        &self,
        limit: Duration,
        name: &'static str,
        args: Vec<OsString>,
        keep_stderr: bool,
    ) -> Result<Vec<u8>, RcloneError> {
        let call = self
            .command(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        let output = match tokio::time::timeout(limit, call).await {
            Ok(done) => done.map_err(RcloneError::Spawn)?,
            Err(_) => {
                return Err(RcloneError::Failed {
                    command: name,
                    detail: format!("no answer within {} seconds", limit.as_secs()),
                });
            }
        };
        if output.status.success() {
            return Ok(output.stdout);
        }
        let detail = if keep_stderr {
            last_line(&output.stderr).unwrap_or_else(|| exit_detail(output.status))
        } else {
            exit_detail(output.status)
        };
        Err(RcloneError::Failed {
            command: name,
            detail,
        })
    }

    /// Removes the remote `name` from the rclone config.
    ///
    /// # Errors
    /// rclone cannot run or fails.
    pub async fn delete_remote(&self, name: &str) -> Result<(), RcloneError> {
        self.output(
            "config delete",
            ["config", "delete", name].map(OsString::from).to_vec(),
            true,
        )
        .await?;
        Ok(())
    }

    /// Opens the remote with `about`, which needs a working sign-in. The
    /// detail of a failure is rclone's last stderr line, never a token.
    ///
    /// # Errors
    /// rclone cannot run, or the remote refuses.
    pub async fn check_remote(&self, name: &str) -> Result<(), RcloneError> {
        self.output(
            "about",
            ["about".to_owned(), format!("{name}:"), "--json".to_owned()]
                .map(OsString::from)
                .to_vec(),
            true,
        )
        .await?;
        Ok(())
    }

    /// Remote names, without the trailing `:`.
    ///
    /// # Errors
    /// rclone cannot run or fails.
    pub async fn listremotes(&self) -> Result<Vec<String>, RcloneError> {
        let stdout = self
            .output("listremotes", vec!["listremotes".into()], true)
            .await?;
        Ok(String::from_utf8_lossy(&stdout)
            .lines()
            .map(str::trim)
            .filter_map(|line| line.strip_suffix(':'))
            .map(str::to_owned)
            .collect())
    }

    /// Remote names with their rclone type (`drive`, `local`...), in the
    /// order rclone lists them.
    ///
    /// # Errors
    /// rclone cannot run or fails.
    pub async fn remote_types(&self) -> Result<Vec<(String, String)>, RcloneError> {
        let stdout = self
            .output(
                "listremotes",
                vec!["listremotes".into(), "--long".into()],
                true,
            )
            .await?;
        Ok(String::from_utf8_lossy(&stdout)
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(name, kind)| (name.trim().to_owned(), kind.trim().to_owned()))
            .filter(|(name, _)| !name.is_empty())
            .collect())
    }

    /// The `client_id` and `client_secret` saved for the remote `name`, so
    /// that a sign-in again can start from them. `None` when the remote has
    /// no such entry. Like the other dump readers, it never logs the dump.
    ///
    /// # Errors
    /// rclone cannot run or fails, or prints something that is not the
    /// expected JSON.
    pub async fn drive_client_credentials(
        &self,
        name: &str,
    ) -> Result<Option<(String, String)>, RcloneError> {
        let stdout = self
            .output("config dump", vec!["config".into(), "dump".into()], false)
            .await?;
        let mut remotes: BTreeMap<String, DumpedRemote> =
            serde_json::from_slice(&stdout).map_err(|_| RcloneError::BadOutput("config dump"))?;
        Ok(remotes.remove(name).map(|remote| {
            (
                remote.client_id.unwrap_or_default(),
                remote.client_secret.unwrap_or_default(),
            )
        }))
    }

    /// Creates the folder `remote_path` (`gdrive:Notes/2026`) and the
    /// folders above it that are missing.
    ///
    /// # Errors
    /// rclone cannot run or fails.
    pub async fn mkdir(&self, remote_path: &str) -> Result<(), RcloneError> {
        self.output(
            "mkdir",
            ["mkdir", "--", remote_path].map(OsString::from).to_vec(),
            true,
        )
        .await?;
        Ok(())
    }

    /// Removes the folder only when it is empty; rclone refuses otherwise.
    ///
    /// # Errors
    /// rclone cannot run or fails (for example the folder is not empty).
    pub async fn rmdir(&self, path: &str) -> Result<(), RcloneError> {
        self.output(
            "rmdir",
            ["rmdir", "--", path].map(OsString::from).to_vec(),
            true,
        )
        .await?;
        Ok(())
    }

    /// Names of `drive` remotes whose `client_id` is absent or empty, so they
    /// use rclone's shared one. Sorted.
    ///
    /// # Errors
    /// rclone cannot run or fails, or prints something that is not the
    /// expected JSON. The dump itself is never part of the error.
    pub async fn drive_remotes_without_own_client_id(&self) -> Result<Vec<String>, RcloneError> {
        let stdout = self
            .output("config dump", vec!["config".into(), "dump".into()], false)
            .await?;
        let remotes: BTreeMap<String, DumpedRemote> =
            serde_json::from_slice(&stdout).map_err(|_| RcloneError::BadOutput("config dump"))?;
        Ok(remotes
            .into_iter()
            .filter(|(_, remote)| {
                remote.kind.as_deref() == Some(DRIVE_TYPE)
                    && remote.client_id.as_deref().is_none_or(str::is_empty)
            })
            .map(|(name, _)| name)
            .collect())
    }

    /// Starts `rclone config create`, which opens the browser for the OAuth
    /// consent and returns when it finishes. stdout and stderr are piped;
    /// dropping the child kills it.
    ///
    /// # Errors
    /// rclone cannot be started.
    pub fn create_drive_remote(
        &self,
        name: &str,
        client_id: &str,
        client_secret: &str,
    ) -> Result<Child, RcloneError> {
        self.config_child([
            "config".to_owned(),
            "create".to_owned(),
            name.to_owned(),
            DRIVE_TYPE.to_owned(),
            format!("client_id={client_id}"),
            format!("client_secret={client_secret}"),
            "scope=drive".to_owned(),
            "config_is_local=true".to_owned(),
        ])
    }

    /// Starts `rclone config update` for the credentials of `name`, same
    /// contract as [`Rclone::create_drive_remote`].
    ///
    /// # Errors
    /// rclone cannot be started.
    pub fn update_drive_credentials(
        &self,
        name: &str,
        client_id: &str,
        client_secret: &str,
    ) -> Result<Child, RcloneError> {
        self.config_child([
            "config".to_owned(),
            "update".to_owned(),
            name.to_owned(),
            format!("client_id={client_id}"),
            format!("client_secret={client_secret}"),
        ])
    }

    fn config_child(&self, args: impl IntoIterator<Item = String>) -> Result<Child, RcloneError> {
        self.command(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(RcloneError::Spawn)
    }

    /// Names of the folders directly inside `remote_path` (`gdrive:Notes`).
    ///
    /// # Errors
    /// rclone cannot run or fails (for example the folder does not exist).
    pub async fn list_dirs(&self, remote_path: &str) -> Result<Vec<String>, RcloneError> {
        let stdout = self
            .output(
                "lsjson",
                [
                    "lsjson",
                    "--dirs-only",
                    "--no-modtime",
                    "--no-mimetype",
                    "--",
                    remote_path,
                ]
                .map(OsString::from)
                .to_vec(),
                true,
            )
            .await?;
        let entries: Vec<ListedEntry> =
            serde_json::from_slice(&stdout).map_err(|_| RcloneError::BadOutput("lsjson"))?;
        Ok(entries.into_iter().map(|entry| entry.name).collect())
    }

    /// Paths of every file under `path`, relative to it, honouring the rules
    /// in `filters` (a `--filters-file` style file) when given.
    ///
    /// # Errors
    /// rclone cannot run or fails.
    pub async fn list_files(
        &self,
        path: &str,
        filters: Option<&Path>,
    ) -> Result<Vec<String>, RcloneError> {
        let mut args: Vec<OsString> = ["lsf", "-R", "--files-only"].map(OsString::from).to_vec();
        if let Some(filters) = filters {
            args.extend(["--filter-from".into(), filters.as_os_str().to_owned()]);
        }
        args.extend(["--".into(), path.into()]);
        let stdout = self.output("lsf", args, true).await?;
        Ok(String::from_utf8_lossy(&stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// Every file and folder under `path` (`gdrive:Notes` or a local
    /// folder), with name, size and modification time. Never the content.
    ///
    /// # Errors
    /// rclone cannot run or fails (for example the folder does not exist).
    pub async fn list_tree(&self, path: &str) -> Result<Vec<TreeEntry>, RcloneError> {
        let stdout = self
            .output(
                "lsjson",
                ["lsjson", "-R", "--no-mimetype", "--", path]
                    .map(OsString::from)
                    .to_vec(),
                true,
            )
            .await?;
        serde_json::from_slice(&stdout).map_err(|_| RcloneError::BadOutput("lsjson"))
    }

    /// What `path` is (a file or a folder, with its size), or `None` when
    /// nothing is there. Never the content.
    ///
    /// # Errors
    /// rclone cannot run, or fails for a reason other than a missing path.
    pub async fn stat(&self, path: &str) -> Result<Option<TreeEntry>, RcloneError> {
        let output = self
            .command(["lsjson", "--stat", "--no-mimetype", "--", path])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(RcloneError::Spawn)?;
        if output.status.success() {
            return serde_json::from_slice(&output.stdout)
                .map(Some)
                .map_err(|_| RcloneError::BadOutput("lsjson"));
        }
        if matches!(output.status.code(), Some(NOT_FOUND | FILE_NOT_FOUND)) {
            return Ok(None);
        }
        Err(RcloneError::Failed {
            command: "lsjson",
            detail: last_line(&output.stderr).unwrap_or_else(|| exit_detail(output.status)),
        })
    }

    /// `rclone <command> <flags> -- <source> <destination>`, where
    /// `command` is `copy`, `copyto`, `move` or `moveto`.
    ///
    /// # Errors
    /// rclone cannot run or fails.
    pub async fn transfer(
        &self,
        command: &'static str,
        flags: &[&str],
        source: &str,
        destination: &str,
    ) -> Result<(), RcloneError> {
        let mut args: Vec<OsString> = vec![command.into()];
        args.extend(flags.iter().map(OsString::from));
        args.extend(["--".into(), source.into(), destination.into()]);
        self.output(command, args, true).await.map(drop)
    }
}

#[derive(Deserialize)]
struct DumpedRemote {
    #[serde(rename = "type")]
    kind: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
}

/// One line of `rclone lsjson -R`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TreeEntry {
    /// Relative to the listed folder, with `/` between the parts.
    #[serde(rename = "Path")]
    pub path: String,
    /// `-1` when the backend does not know it (Google Docs).
    #[serde(rename = "Size", default)]
    pub size: i64,
    #[serde(rename = "ModTime", default)]
    pub modified: Option<String>,
    #[serde(rename = "IsDir", default)]
    pub is_dir: bool,
}

#[derive(Deserialize)]
struct ListedEntry {
    #[serde(rename = "Name")]
    name: String,
}

fn last_line(stderr: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(str::to_owned)
}

fn exit_detail(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "killed by a signal".to_owned(),
    }
}

/// The host's rclone config, for a sandbox whose `RCLONE_CONFIG` is unset.
fn sandbox_config(flatpak_id: Option<&str>, configured: bool, paths: &Paths) -> Option<PathBuf> {
    (flatpak_id.is_some() && !configured)
        .then(|| paths.config_home.join("rclone").join("rclone.conf"))
}

fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// `override_bin` (the value of `SYNCFAST_RCLONE`) wins when set and not
/// empty; it must exist. Otherwise the first executable `rclone` on
/// `path_var` is used.
fn resolve_binary(
    override_bin: Option<&OsStr>,
    path_var: Option<&OsStr>,
) -> Result<PathBuf, RcloneError> {
    if let Some(bin) = override_bin.filter(|bin| !bin.is_empty()) {
        let bin = PathBuf::from(bin);
        return if is_executable_file(&bin) {
            Ok(bin)
        } else {
            Err(RcloneError::RcloneNotFound)
        };
    }
    path_var
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join(BINARY_NAME))
        .find(|candidate| is_executable_file(candidate))
        .ok_or(RcloneError::RcloneNotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Paths {
        Paths::new(
            "/home/ada".into(),
            "/home/ada/.config".into(),
            "/s".into(),
            "/d".into(),
            "/r".into(),
        )
    }

    #[test]
    fn inside_flatpak_an_unset_rclone_config_is_the_host_one() {
        let config = sandbox_config(Some("io.example.App"), false, &paths());

        assert_eq!(
            config.as_deref(),
            Some(Path::new("/home/ada/.config/rclone/rclone.conf"))
        );
    }

    #[test]
    fn an_explicit_rclone_config_or_no_sandbox_leaves_the_environment_alone() {
        assert_eq!(sandbox_config(Some("io.example.App"), true, &paths()), None);
        assert_eq!(sandbox_config(None, false, &paths()), None);
    }

    fn executable(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn the_override_is_used_even_when_rclone_is_on_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let custom = executable(dir.path(), "my-rclone");
        let other = tempfile::tempdir().unwrap();
        executable(other.path(), "rclone");

        let bin = resolve_binary(Some(custom.as_os_str()), Some(other.path().as_os_str())).unwrap();

        assert_eq!(bin, custom);
    }

    #[test]
    fn an_override_that_does_not_exist_is_not_found_even_with_rclone_on_the_path() {
        let dir = tempfile::tempdir().unwrap();
        executable(dir.path(), "rclone");

        let result = resolve_binary(
            Some(dir.path().join("missing").as_os_str()),
            Some(dir.path().as_os_str()),
        );

        assert!(matches!(result, Err(RcloneError::RcloneNotFound)));
    }

    #[test]
    fn an_empty_override_falls_back_to_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let on_path = executable(dir.path(), "rclone");

        let bin = resolve_binary(Some(OsStr::new("")), Some(dir.path().as_os_str())).unwrap();

        assert_eq!(bin, on_path);
    }

    #[test]
    fn the_first_executable_rclone_on_the_path_wins_and_non_executables_are_skipped() {
        let first = tempfile::tempdir().unwrap();
        std::fs::write(first.path().join("rclone"), "not executable").unwrap();
        let second = tempfile::tempdir().unwrap();
        let wanted = executable(second.path(), "rclone");
        let third = tempfile::tempdir().unwrap();
        executable(third.path(), "rclone");
        let path = std::env::join_paths([first.path(), second.path(), third.path()]).unwrap();

        let bin = resolve_binary(None, Some(path.as_os_str())).unwrap();

        assert_eq!(bin, wanted);
    }

    #[test]
    fn no_rclone_anywhere_fails_with_the_install_hint() {
        let dir = tempfile::tempdir().unwrap();

        let error = resolve_binary(None, Some(dir.path().as_os_str())).unwrap_err();

        assert_eq!(
            error.to_string(),
            "rclone not found; install rclone or reinstall syncfast units"
        );
    }

    #[test]
    fn an_unset_path_is_not_found() {
        assert!(matches!(
            resolve_binary(None, None),
            Err(RcloneError::RcloneNotFound)
        ));
    }

    /// The real rclone with a throwaway config, or `None` (with a warning)
    /// when rclone is not installed.
    #[allow(clippy::print_stderr)]
    fn rclone_with(config: &str) -> Option<(Rclone, tempfile::TempDir)> {
        let Ok(rclone) = Rclone::from_env() else {
            eprintln!("skipping: rclone is not installed");
            return None;
        };
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("rclone.conf");
        std::fs::write(&config_path, config).unwrap();
        Some((rclone.with_config(config_path), dir))
    }

    const LOCAL_REMOTE: &str = "[testremote]\ntype = local\n";

    #[tokio::test]
    async fn listremotes_returns_the_names_in_the_given_config_without_the_colon() {
        let Some((rclone, _dir)) = rclone_with("[a]\ntype = local\n\n[b]\ntype = local\n") else {
            return;
        };

        let remotes = rclone.listremotes().await.unwrap();

        assert_eq!(remotes, ["a", "b"]);
    }

    #[tokio::test]
    async fn listremotes_of_an_empty_config_is_empty() {
        let Some((rclone, _dir)) = rclone_with("") else {
            return;
        };

        assert_eq!(rclone.listremotes().await.unwrap(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn list_dirs_returns_only_the_folders_directly_inside() {
        let Some((rclone, dir)) = rclone_with(LOCAL_REMOTE) else {
            return;
        };
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("alpha/deep")).unwrap();
        std::fs::create_dir_all(root.join("beta")).unwrap();
        std::fs::write(root.join("file.txt"), "x").unwrap();

        let dirs = rclone
            .list_dirs(&format!("testremote:{}", root.display()))
            .await
            .unwrap();

        assert_eq!(dirs, ["alpha", "beta"]);
    }

    #[tokio::test]
    async fn list_dirs_of_a_missing_folder_fails_with_the_rclone_message() {
        let Some((rclone, dir)) = rclone_with(LOCAL_REMOTE) else {
            return;
        };

        let error = rclone
            .list_dirs(&format!(
                "testremote:{}",
                dir.path().join("absent").display()
            ))
            .await
            .unwrap_err();

        assert!(
            matches!(&error, RcloneError::Failed { command: "lsjson", detail } if detail.contains("not found")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn list_files_returns_every_file_recursively_and_skips_folders() {
        let Some((rclone, dir)) = rclone_with(LOCAL_REMOTE) else {
            return;
        };
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "x").unwrap();
        std::fs::write(root.join("sub/b.log"), "x").unwrap();

        let mut files = rclone
            .list_files(root.to_str().unwrap(), None)
            .await
            .unwrap();
        files.sort();

        assert_eq!(files, ["a.txt", "sub/b.log"]);
    }

    #[tokio::test]
    async fn list_files_with_a_filters_file_leaves_out_the_excluded_files() {
        let Some((rclone, dir)) = rclone_with(LOCAL_REMOTE) else {
            return;
        };
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "x").unwrap();
        std::fs::write(root.join("sub/b.log"), "x").unwrap();
        let filters = dir.path().join("filters.txt");
        std::fs::write(&filters, "# Generated by syncfast. Do not edit.\n- *.log\n").unwrap();

        let files = rclone
            .list_files(root.to_str().unwrap(), Some(&filters))
            .await
            .unwrap();

        assert_eq!(files, ["a.txt"]);
    }

    #[tokio::test]
    async fn remote_types_pairs_each_name_with_its_rclone_type() {
        let Some((rclone, _dir)) = rclone_with("[a]\ntype = local\n\n[b]\ntype = drive\n") else {
            return;
        };

        let remotes = rclone.remote_types().await.unwrap();

        assert_eq!(
            remotes,
            [
                ("a".to_owned(), "local".to_owned()),
                ("b".to_owned(), "drive".to_owned())
            ]
        );
    }

    #[tokio::test]
    async fn mkdir_creates_the_folder_and_its_missing_parents() {
        let Some((rclone, dir)) = rclone_with(LOCAL_REMOTE) else {
            return;
        };
        let target = dir.path().join("root/new/deeper");

        rclone
            .mkdir(&format!("testremote:{}", target.display()))
            .await
            .unwrap();

        assert!(target.is_dir());
    }

    #[tokio::test]
    async fn drive_client_credentials_returns_what_is_saved_and_none_for_an_unknown_remote() {
        let config = "\
[own]
type = drive
client_id = 123.apps.googleusercontent.com
client_secret = hunter2

[shared]
type = drive
";
        let Some((rclone, _dir)) = rclone_with(config) else {
            return;
        };

        let own = rclone.drive_client_credentials("own").await.unwrap();
        let shared = rclone.drive_client_credentials("shared").await.unwrap();
        let unknown = rclone.drive_client_credentials("absent").await.unwrap();

        assert_eq!(
            own,
            Some((
                "123.apps.googleusercontent.com".to_owned(),
                "hunter2".to_owned()
            ))
        );
        assert_eq!(shared, Some((String::new(), String::new())));
        assert_eq!(unknown, None);
    }

    #[tokio::test]
    async fn drive_remotes_without_own_client_id_lists_only_drive_remotes_without_one() {
        let config = "\
[shared]
type = drive

[blank]
type = drive
client_id =

[own]
type = drive
client_id = 123.apps.googleusercontent.com
client_secret = hunter2

[elsewhere]
type = local
";
        let Some((rclone, _dir)) = rclone_with(config) else {
            return;
        };

        let remotes = rclone.drive_remotes_without_own_client_id().await.unwrap();

        assert_eq!(remotes, ["blank", "shared"]);
    }

    #[tokio::test]
    async fn a_binary_that_cannot_be_started_is_a_spawn_error() {
        let rclone = Rclone::new(PathBuf::from("/nonexistent/rclone"));

        let error = rclone
            .drive_remotes_without_own_client_id()
            .await
            .unwrap_err();

        assert!(matches!(error, RcloneError::Spawn(_)));
    }

    fn fake_rclone(dir: &Path, script: &str) -> Rclone {
        let path = dir.join("fake-rclone");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Rclone::new(path)
    }

    #[tokio::test]
    async fn a_failing_config_dump_never_puts_its_output_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = fake_rclone(
            dir.path(),
            "echo 'client_secret=TOPSECRET'; echo 'TOPSECRET' >&2; exit 3",
        );

        let error = rclone
            .drive_remotes_without_own_client_id()
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "rclone config dump failed: exit code 3");
    }

    #[tokio::test]
    async fn an_unparseable_config_dump_never_puts_its_output_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = fake_rclone(
            dir.path(),
            "echo '{\"a\": {\"client_id\": 5, \"x\": \"TOPSECRET\"}}'",
        );

        let error = rclone
            .drive_remotes_without_own_client_id()
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "unexpected output from rclone config dump"
        );
    }

    #[tokio::test]
    async fn create_drive_remote_passes_the_credentials_as_key_value_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-rclone");
        std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let rclone = Rclone::new(script);

        let child = rclone
            .create_drive_remote("mine", "the-id", "the-secret")
            .unwrap();
        let output = child.wait_with_output().await.unwrap();

        let args: Vec<_> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            args,
            [
                "config",
                "create",
                "mine",
                "drive",
                "client_id=the-id",
                "client_secret=the-secret",
                "scope=drive",
                "config_is_local=true",
            ]
        );
    }

    #[tokio::test]
    async fn update_drive_credentials_passes_the_credentials_as_key_value_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-rclone");
        std::fs::write(&script, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let rclone = Rclone::new(script);

        let child = rclone
            .update_drive_credentials("mine", "the-id", "the-secret")
            .unwrap();
        let output = child.wait_with_output().await.unwrap();

        let args: Vec<_> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            args,
            [
                "config",
                "update",
                "mine",
                "client_id=the-id",
                "client_secret=the-secret",
            ]
        );
    }

    #[tokio::test]
    async fn delete_remote_removes_only_that_remote() {
        let Some((rclone, _dir)) = rclone_with("[a]\ntype = local\n\n[b]\ntype = local\n") else {
            return;
        };

        rclone.delete_remote("a").await.unwrap();

        assert_eq!(rclone.listremotes().await.unwrap(), ["b"]);
    }

    #[tokio::test]
    async fn check_remote_is_ok_for_a_local_remote_and_fails_for_an_unknown_one() {
        let Some((rclone, _dir)) = rclone_with(LOCAL_REMOTE) else {
            return;
        };

        assert!(rclone.check_remote("testremote").await.is_ok());
        let error = rclone.check_remote("nothere").await.unwrap_err();
        assert!(matches!(
            error,
            RcloneError::Failed {
                command: "about",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn check_remote_and_delete_remote_name_the_remote_in_the_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let rclone = fake_rclone(dir.path(), "printf '%s\\n' \"$@\" >&2; exit 1");

        let about = rclone.check_remote("mine").await.unwrap_err();
        let delete = rclone.delete_remote("mine").await.unwrap_err();

        // The detail is the last stderr line, so the last argument.
        assert!(matches!(&about, RcloneError::Failed { detail, .. } if detail == "--json"));
        assert!(matches!(&delete, RcloneError::Failed { detail, .. } if detail == "mine"));
    }

    #[tokio::test]
    async fn a_short_call_that_does_not_answer_fails_and_the_child_is_killed() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let rclone = fake_rclone(
            dir.path(),
            &format!("echo $$ > '{}'; exec sleep 30", pid_file.display()),
        );

        let error = rclone
            .output_within(
                Duration::from_secs(1),
                "listremotes",
                vec!["listremotes".into()],
                true,
            )
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "rclone listremotes failed: no answer within 1 seconds"
        );
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        let proc = PathBuf::from(format!("/proc/{}", pid.trim()));
        for _ in 0..40 {
            if !proc.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!proc.exists(), "the child {} is still running", pid.trim());
    }

    #[test]
    fn short_calls_wait_sixty_seconds() {
        assert_eq!(SHORT_CALL_TIMEOUT, Duration::from_secs(60));
    }
}
