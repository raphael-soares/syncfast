//! Running restic (spec phase 3, step 4). Every call uses explicit
//! arguments and no shell. The backup key goes in through a pipe and is
//! never in argv, the environment or a log.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::{Child, Command};

use crate::backup::destination::Destination;
use crate::backup::key::Key;
use crate::backup::snapshots::{SnapshotView, parse_snapshots};
use crate::behavior::Bandwidth;
use crate::bisync::{Fault, fault_of};
use crate::flatpak;
use crate::paths::Paths;
use crate::rclone::{Rclone, is_executable_file};
use crate::run::{Interrupts, forward_interrupt};

/// The restic the Flatpak ships (`scripts/pin-restic.sh` keeps it equal to
/// the manifest).
pub const RESTIC_VERSION: &str = "0.19.1";
/// The oldest restic dev mode accepts: it needs `--limit-upload` and the
/// JSON output syncfast reads.
pub const RESTIC_MIN_VERSION: &str = "0.17.1";

const BINARY_NAME: &str = "restic";
const BINARY_OVERRIDE_VAR: &str = "SYNCFAST_RESTIC";
const EXIT_NO_REPOSITORY: i32 = 10;
const EXIT_LOCKED: i32 = 11;
const EXIT_WRONG_PASSWORD: i32 = 12;
const RCLONE_ARGS: &str =
    "rclone.args=serve restic --stdio --b2-hard-delete --drive-use-trash=false";
const SCRUBBED_VARS: [&str; 5] = [
    "RESTIC_PASSWORD",
    "RESTIC_PASSWORD_FILE",
    "RESTIC_PASSWORD_COMMAND",
    "RESTIC_REPOSITORY",
    "RESTIC_REPOSITORY_FILE",
];

#[derive(Debug, thiserror::Error)]
pub enum ResticError {
    #[error("restic not found; reinstall syncfast")]
    NotFound,
    #[error("cannot run restic: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("restic {found} is older than {RESTIC_MIN_VERSION}, which syncfast needs")]
    TooOld { found: String },
    #[error("cannot read the name of this machine: {0}")]
    Host(String),
    #[error("No backup was found in that folder.")]
    NoRepository,
    #[error("That key does not open this backup.")]
    WrongKey,
    #[error("The backup repository is locked: {message}")]
    Locked { message: String },
    #[error("restic failed: {message}")]
    Failed {
        message: String,
        /// The account problem rclone reported inside restic's stderr.
        fault: Option<Fault>,
    },
    #[error("restic gave output syncfast cannot read ({0})")]
    BadOutput(&'static str),
}

/// Which repository a call opens, from where, and how fast it may talk to
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    pub destination: Destination,
    /// The backup's state folder: restic runs with it as its working
    /// directory.
    pub work_dir: PathBuf,
    pub limit_upload: Bandwidth,
    pub limit_download: Bandwidth,
}

impl Repository {
    #[must_use]
    pub fn new(destination: Destination, work_dir: PathBuf) -> Self {
        Self {
            destination,
            work_dir,
            limit_upload: Bandwidth::Off,
            limit_download: Bandwidth::Off,
        }
    }

    #[must_use]
    pub fn with_limits(mut self, upload: Bandwidth, download: Bandwidth) -> Self {
        self.limit_upload = upload;
        self.limit_download = download;
        self
    }
}

/// What `restic init --json` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitResult {
    pub id: String,
    pub repository: String,
}

/// What a restic call that succeeded wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Restic {
    bin: PathBuf,
    rclone: Rclone,
    cache_dir: PathBuf,
}

/// The name of this machine, read once: the `--host` of `backup` and
/// `forget`.
///
/// # Errors
/// The machine has no name.
pub fn backup_host() -> Result<&'static str, ResticError> {
    static HOST: OnceLock<String> = OnceLock::new();
    if let Some(host) = HOST.get() {
        return Ok(host);
    }
    let name = rustix::system::uname()
        .nodename()
        .to_string_lossy()
        .into_owned();
    let host = clean_host(&name)?;
    Ok(HOST.get_or_init(|| host))
}

fn clean_host(name: &str) -> Result<String, ResticError> {
    let name = name.trim();
    if name.is_empty() {
        Err(ResticError::Host("it is empty".to_owned()))
    } else {
        Ok(name.to_owned())
    }
}

const INIT_ARGS: &[&str] = &["init", "--json"];
const VERIFY_ARGS: &[&str] = &["cat", "config"];
const SNAPSHOTS_ARGS: &[&str] = &["snapshots", "--json"];
/// Never `--remove-all`, which would break a run that is alive.
const UNLOCK_ARGS: &[&str] = &["unlock"];

fn takes_host(subcommand: &str) -> bool {
    matches!(subcommand, "backup" | "forget")
}

/// KiB per second, the unit of restic's `--limit-upload`.
fn limit_flag(flag: &'static str, limit: Bandwidth) -> Option<[OsString; 2]> {
    match limit {
        Bandwidth::Off => None,
        Bandwidth::KiBPerSec(kib) => Some([flag.into(), kib.to_string().into()]),
    }
}

/// The whole argument list of one restic call: the repository, the way
/// restic reaches rclone, the key on stdin, the limits, the subcommand and
/// `--host` where it applies. Never the key.
fn build_args(
    rclone_bin: &Path,
    repository: &Repository,
    host: &str,
    subcommand: &[&str],
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-r".into(), repository.destination.repo_arg().into()];
    if matches!(repository.destination, Destination::Account { .. }) {
        let mut program = OsString::from("rclone.program=");
        program.push(rclone_bin);
        args.extend(["-o".into(), program, "-o".into(), RCLONE_ARGS.into()]);
    }
    args.extend(["--password-file".into(), "/dev/stdin".into()]);
    args.extend(
        limit_flag("--limit-upload", repository.limit_upload)
            .into_iter()
            .flatten(),
    );
    args.extend(
        limit_flag("--limit-download", repository.limit_download)
            .into_iter()
            .flatten(),
    );
    args.extend(subcommand.iter().map(OsString::from));
    if subcommand.first().is_some_and(|first| takes_host(first)) {
        args.extend(["--host".into(), host.into()]);
    }
    args
}

fn resolve_binary(
    override_bin: Option<&OsStr>,
    path_var: Option<&OsStr>,
) -> Result<PathBuf, ResticError> {
    if let Some(bin) = override_bin.filter(|bin| !bin.is_empty()) {
        let bin = PathBuf::from(bin);
        return if is_executable_file(&bin) {
            Ok(bin)
        } else {
            Err(ResticError::NotFound)
        };
    }
    path_var
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join(BINARY_NAME))
        .find(|candidate| is_executable_file(candidate))
        .ok_or(ResticError::NotFound)
}

fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.trim().split('.');
    let mut next = || parts.next()?.parse().ok();
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

/// `Err` when `found` is older than [`RESTIC_MIN_VERSION`]; `Ok(true)` when
/// it is not the pinned [`RESTIC_VERSION`].
fn check_version(found: &str) -> Result<bool, ResticError> {
    let (Some(found_parts), Some(min)) = (parse_version(found), parse_version(RESTIC_MIN_VERSION))
    else {
        return Err(ResticError::BadOutput("restic version"));
    };
    if found_parts < min {
        return Err(ResticError::TooOld {
            found: found.to_owned(),
        });
    }
    Ok(found != RESTIC_VERSION)
}

#[derive(Deserialize)]
struct ExitError {
    message_type: String,
    #[serde(default)]
    message: String,
}

/// The message restic ended with: the `exit_error` line of `--json` calls,
/// else the last line of stderr.
fn failure_message(stderr: &str) -> String {
    stderr
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .find_map(|line| {
            serde_json::from_str::<ExitError>(line)
                .ok()
                .filter(|error| error.message_type == "exit_error")
                .map(|error| error.message)
        })
        .or_else(|| {
            stderr
                .lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}

/// Turns a restic exit into a typed error. Exit codes 10, 11 and 12 are
/// restic's own; other failures keep the message and the rclone problem in
/// its stderr, if any.
fn classify_failure(code: Option<i32>, stderr: &[u8]) -> ResticError {
    let stderr = String::from_utf8_lossy(stderr);
    let message = failure_message(&stderr);
    match code {
        Some(EXIT_NO_REPOSITORY) => ResticError::NoRepository,
        Some(EXIT_WRONG_PASSWORD) => ResticError::WrongKey,
        Some(EXIT_LOCKED) => ResticError::Locked {
            message: first_line(&message).to_owned(),
        },
        _ => {
            let fault = fault_of(&stderr).filter(|fault| {
                matches!(
                    fault,
                    Fault::Auth
                        | Fault::Account
                        | Fault::Quota
                        | Fault::Permission
                        | Fault::RemoteMissing
                )
            });
            let message = if message.is_empty() {
                match code {
                    Some(code) => format!("exit code {code}"),
                    None => "killed by a signal".to_owned(),
                }
            } else {
                message
            };
            ResticError::Failed { message, fault }
        }
    }
}

async fn read_all(stream: Option<impl AsyncRead + Unpin>) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(mut stream) = stream
        && let Err(err) = stream.read_to_end(&mut bytes).await
    {
        log::warn!("cannot read restic's output: {err}");
    }
    bytes
}

async fn wait_forwarding(
    child: &mut Child,
    mut interrupts: Option<Interrupts>,
) -> std::io::Result<ExitStatus> {
    loop {
        tokio::select! {
            status = child.wait() => return status,
            () = async {
                match interrupts.as_mut() {
                    Some(interrupts) => interrupts.next().await,
                    None => std::future::pending().await,
                }
            } => forward_interrupt(child, "restic"),
        }
    }
}

impl Restic {
    #[must_use]
    pub fn new(bin: PathBuf, rclone: Rclone, paths: &Paths) -> Self {
        Self {
            bin,
            rclone,
            cache_dir: paths.restic_cache_dir(),
        }
    }

    /// `SYNCFAST_RESTIC` if set, otherwise `restic` on `PATH`. Outside the
    /// Flatpak the version is checked: older than [`RESTIC_MIN_VERSION`] is
    /// refused and a different one only logs a warning.
    ///
    /// # Errors
    /// [`ResticError::NotFound`], or a restic that is too old or cannot
    /// say its version.
    pub async fn from_env(rclone: Rclone, paths: &Paths) -> Result<Self, ResticError> {
        let bin = resolve_binary(
            std::env::var_os(BINARY_OVERRIDE_VAR).as_deref(),
            std::env::var_os("PATH").as_deref(),
        )?;
        let restic = Self::new(bin, rclone, paths);
        if flatpak::app_id().is_none() {
            restic.ensure_supported().await?;
        }
        Ok(restic)
    }

    #[must_use]
    pub fn rclone(&self) -> &Rclone {
        &self.rclone
    }

    async fn ensure_supported(&self) -> Result<(), ResticError> {
        #[derive(Deserialize)]
        struct Version {
            version: String,
        }
        let mut command = self.base_command();
        command
            .args(["version", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = command.output().await.map_err(ResticError::Spawn)?;
        if !output.status.success() {
            return Err(classify_failure(output.status.code(), &output.stderr));
        }
        let version: Version = serde_json::from_slice(&output.stdout)
            .map_err(|_| ResticError::BadOutput("restic version"))?;
        if check_version(&version.version)? {
            log::warn!(
                "restic {} is not the {RESTIC_VERSION} syncfast is tested with",
                version.version
            );
        }
        Ok(())
    }

    /// The process every call starts from: no password in its
    /// environment, restic's cache in syncfast's state and rclone on the
    /// config syncfast uses.
    fn base_command(&self) -> Command {
        let mut command = Command::new(&self.bin);
        command.kill_on_drop(true);
        for var in SCRUBBED_VARS {
            command.env_remove(var);
        }
        command
            .env("RESTIC_CACHE_DIR", &self.cache_dir)
            .env("RESTIC_PROGRESS_FPS", "1");
        if let Some(config) = self.rclone.config() {
            command.env("RCLONE_CONFIG", config);
        }
        command
    }

    fn command(&self, repository: &Repository, host: &str, subcommand: &[&str]) -> Command {
        let mut command = self.base_command();
        command
            .args(build_args(self.rclone.bin(), repository, host, subcommand))
            .current_dir(&repository.work_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// Starts restic with `key` on its stdin and the pipes open, for a
    /// caller that reads the output as it comes (the backup round).
    ///
    /// # Errors
    /// The machine has no name, or restic cannot be started.
    pub async fn spawn(
        &self,
        repository: &Repository,
        key: &Key,
        subcommand: &[&str],
    ) -> Result<Child, ResticError> {
        let host = if subcommand.first().is_some_and(|first| takes_host(first)) {
            backup_host()?
        } else {
            ""
        };
        tokio::fs::create_dir_all(&repository.work_dir)
            .await
            .map_err(ResticError::Spawn)?;
        let mut child = self
            .command(repository, host, subcommand)
            .spawn()
            .map_err(ResticError::Spawn)?;
        if let Some(mut stdin) = child.stdin.take() {
            match stdin.write_all(key.password().as_bytes()).await {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => {}
                Err(err) => return Err(ResticError::Spawn(err)),
            }
        }
        Ok(child)
    }

    /// Runs `subcommand` and waits. With `forward_signals`, SIGINT and
    /// SIGTERM of this process go to restic as SIGINT, and the call waits
    /// for it to release its lock and exit.
    ///
    /// # Errors
    /// restic cannot be started, or it exits with a failure.
    pub async fn run(
        &self,
        repository: &Repository,
        key: &Key,
        subcommand: &[&str],
        forward_signals: bool,
    ) -> Result<Output, ResticError> {
        let interrupts = if forward_signals {
            Some(Interrupts::install().map_err(ResticError::Spawn)?)
        } else {
            None
        };
        let mut child = self.spawn(repository, key, subcommand).await?;
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        let (status, stdout, stderr) = tokio::join!(
            wait_forwarding(&mut child, interrupts),
            read_all(stdout),
            read_all(stderr)
        );
        let status = status.map_err(ResticError::Spawn)?;
        if status.success() {
            Ok(Output { stdout, stderr })
        } else {
            Err(classify_failure(status.code(), &stderr))
        }
    }

    /// `restic init --json`.
    ///
    /// # Errors
    /// restic fails or prints something else.
    pub async fn init(
        &self,
        repository: &Repository,
        key: &Key,
    ) -> Result<InitResult, ResticError> {
        let output = self.run(repository, key, INIT_ARGS, false).await?;
        serde_json::from_slice(&output.stdout).map_err(|_| ResticError::BadOutput("init"))
    }

    /// `restic cat config`: the repository exists and `key` opens it.
    ///
    /// # Errors
    /// [`ResticError::NoRepository`], [`ResticError::WrongKey`] or any other
    /// failure.
    pub async fn verify(&self, repository: &Repository, key: &Key) -> Result<(), ResticError> {
        self.run(repository, key, VERIFY_ARGS, false)
            .await
            .map(|_| ())
    }

    /// `restic snapshots --json`: the snapshots of every host, newest
    /// first.
    ///
    /// # Errors
    /// restic fails or prints something else.
    pub async fn snapshots(
        &self,
        repository: &Repository,
        key: &Key,
    ) -> Result<Vec<SnapshotView>, ResticError> {
        let output = self.run(repository, key, SNAPSHOTS_ARGS, false).await?;
        parse_snapshots(&output.stdout).map_err(|_| ResticError::BadOutput("snapshots"))
    }

    /// `restic unlock`: removes locks of runs that are gone. Never
    /// `--remove-all`, which would break a run that is alive.
    ///
    /// # Errors
    /// restic fails.
    pub async fn unlock(&self, repository: &Repository, key: &Key) -> Result<(), ResticError> {
        self.run(repository, key, UNLOCK_ARGS, false)
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    const SENTINEL: &str = "SENTINELSENTINELSENTINELSENTINELSENTINELSENTINELSENT";

    fn fixture(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/restic-0.19.1")
            .join(name);
        std::fs::read(path).unwrap()
    }

    fn account() -> Repository {
        Repository::new(
            Destination::parse("gdrive:Backups/laptop").unwrap(),
            "/state/backups/docs".into(),
        )
    }

    fn local() -> Repository {
        Repository::new(
            Destination::parse("/mnt/usb/repo").unwrap(),
            "/state".into(),
        )
    }

    fn text(args: &[OsString]) -> Vec<&str> {
        args.iter().map(|arg| arg.to_str().unwrap()).collect()
    }

    fn restic() -> Restic {
        let paths = Paths::new(
            "/home/ada".into(),
            "/home/ada/.config".into(),
            "/home/ada/.local/state".into(),
            "/home/ada/.local/share".into(),
            "/run/user/1000".into(),
        );
        Restic::new(
            "/usr/bin/restic".into(),
            Rclone::new("/usr/bin/rclone".into()).with_config("/cfg/rclone.conf".into()),
            &paths,
        )
    }

    #[test]
    fn an_account_destination_goes_through_the_pinned_rclone() {
        let args = build_args(
            Path::new("/usr/bin/rclone"),
            &account(),
            "h",
            &["cat", "config"],
        );

        assert_eq!(
            text(&args),
            [
                "-r",
                "rclone:gdrive:Backups/laptop",
                "-o",
                "rclone.program=/usr/bin/rclone",
                "-o",
                "rclone.args=serve restic --stdio --b2-hard-delete --drive-use-trash=false",
                "--password-file",
                "/dev/stdin",
                "cat",
                "config",
            ]
        );
    }

    #[test]
    fn a_local_destination_needs_no_rclone() {
        let args = build_args(Path::new("/usr/bin/rclone"), &local(), "h", &["unlock"]);

        assert_eq!(
            text(&args),
            [
                "-r",
                "/mnt/usb/repo",
                "--password-file",
                "/dev/stdin",
                "unlock"
            ]
        );
    }

    #[test]
    fn limits_are_in_kib_per_second_and_off_omits_the_flag() {
        let repo = |up, down| local().with_limits(up, down);
        let rate = |text: &str| Bandwidth::parse(text).unwrap();
        let args_for = |repository: &Repository| {
            build_args(Path::new("r"), repository, "h", &["check"])
                .iter()
                .map(|arg| arg.to_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };

        let tail = ["--password-file", "/dev/stdin"].map(str::to_owned);
        let flags = |args: &[String]| args[2 + tail.len()..args.len() - 1].to_vec();

        assert!(flags(&args_for(&repo(Bandwidth::Off, Bandwidth::Off))).is_empty());
        assert_eq!(
            flags(&args_for(&repo(rate("500K"), Bandwidth::Off))),
            ["--limit-upload", "500"]
        );
        assert_eq!(
            flags(&args_for(&repo(Bandwidth::Off, rate("2M")))),
            ["--limit-download", "2048"]
        );
        assert_eq!(
            flags(&args_for(&repo(rate("1G"), rate("3K")))),
            ["--limit-upload", "1048576", "--limit-download", "3"]
        );
    }

    #[test]
    fn host_is_added_to_backup_and_forget_only() {
        for (subcommand, expected) in [
            (vec!["backup", "--json"], true),
            (vec!["forget", "--prune"], true),
            (vec!["snapshots", "--json"], false),
            (vec!["check"], false),
            (vec!["restore", "latest"], false),
            (vec!["unlock"], false),
            (vec!["init", "--json"], false),
            (vec!["cat", "config"], false),
            (vec!["ls", "--json"], false),
        ] {
            let args = build_args(Path::new("r"), &local(), "laptop", &subcommand);
            let args = text(&args);

            assert_eq!(args.contains(&"--host"), expected, "{subcommand:?}");
            if expected {
                assert_eq!(args[args.len() - 2..], ["--host", "laptop"]);
            }
        }
    }

    #[test]
    fn init_verify_snapshots_and_unlock_have_their_exact_argv() {
        let account_head = [
            "-r",
            "rclone:gdrive:Backups/laptop",
            "-o",
            "rclone.program=/usr/bin/rclone",
            "-o",
            "rclone.args=serve restic --stdio --b2-hard-delete --drive-use-trash=false",
            "--password-file",
            "/dev/stdin",
        ];

        for (subcommand, tail) in [
            (INIT_ARGS, &["init", "--json"][..]),
            (VERIFY_ARGS, &["cat", "config"]),
            (SNAPSHOTS_ARGS, &["snapshots", "--json"]),
            (UNLOCK_ARGS, &["unlock"]),
        ] {
            let args = build_args(
                Path::new("/usr/bin/rclone"),
                &account(),
                "laptop",
                subcommand,
            );

            assert_eq!(text(&args), [&account_head[..], tail].concat());
        }
    }

    #[test]
    fn limits_reach_every_call_in_kib_per_second() {
        let rate = |text: &str| Bandwidth::parse(text).unwrap();
        let head = ["-r", "/mnt/usb/repo", "--password-file", "/dev/stdin"];

        for (upload, download, flags) in [
            (Bandwidth::Off, Bandwidth::Off, vec![]),
            (rate("500K"), Bandwidth::Off, vec!["--limit-upload", "500"]),
            (Bandwidth::Off, rate("2M"), vec!["--limit-download", "2048"]),
            (
                rate("1G"),
                rate("3K"),
                vec!["--limit-upload", "1048576", "--limit-download", "3"],
            ),
        ] {
            let repository = local().with_limits(upload, download);
            for (subcommand, tail) in [
                (INIT_ARGS, &["init", "--json"][..]),
                (VERIFY_ARGS, &["cat", "config"]),
                (SNAPSHOTS_ARGS, &["snapshots", "--json"]),
                (UNLOCK_ARGS, &["unlock"]),
            ] {
                let args = build_args(Path::new("r"), &repository, "laptop", subcommand);

                assert_eq!(text(&args), [&head[..], &flags, tail].concat());
            }
        }
    }

    #[test]
    fn no_call_but_backup_and_forget_carries_the_host() {
        for subcommand in [INIT_ARGS, VERIFY_ARGS, SNAPSHOTS_ARGS, UNLOCK_ARGS] {
            let args = build_args(Path::new("r"), &local(), "laptop", subcommand);

            assert!(!text(&args).contains(&"--host"), "{subcommand:?}");
        }
    }

    #[test]
    fn a_lock_is_reported_without_saying_who_holds_it() {
        let err = ResticError::Locked {
            message: "repository is already locked".to_owned(),
        };

        assert_eq!(
            err.to_string(),
            "The backup repository is locked: repository is already locked"
        );
    }

    #[test]
    fn the_key_is_in_no_argument_and_no_variable_of_the_process() {
        let key = Key::parse(SENTINEL).unwrap();
        let command = restic().command(&account(), "h", &["backup", "--json", "/home/ada/docs"]);
        let std = command.as_std();

        let everything = std
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .chain(std.get_envs().flat_map(|(name, value)| {
                [
                    name.to_string_lossy().into_owned(),
                    value
                        .map(|v| v.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ]
            }))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(!everything.contains(key.password()));
    }

    #[test]
    fn the_environment_is_scrubbed_and_pointed_at_syncfast_state() {
        let command = restic().command(&local(), "h", &["unlock"]);
        let envs: std::collections::BTreeMap<_, _> = command
            .as_std()
            .get_envs()
            .map(|(name, value)| (name.to_owned(), value.map(OsStr::to_owned)))
            .collect();

        for var in SCRUBBED_VARS {
            assert_eq!(envs.get(OsStr::new(var)), Some(&None), "{var}");
        }
        assert_eq!(
            envs.get(OsStr::new("RESTIC_CACHE_DIR")),
            Some(&Some(OsString::from(
                "/home/ada/.local/state/syncfast/restic-cache"
            )))
        );
        assert_eq!(
            envs.get(OsStr::new("RESTIC_PROGRESS_FPS")),
            Some(&Some(OsString::from("1")))
        );
        assert_eq!(
            envs.get(OsStr::new("RCLONE_CONFIG")),
            Some(&Some(OsString::from("/cfg/rclone.conf")))
        );
        assert_eq!(
            command.as_std().get_current_dir(),
            Some(Path::new("/state"))
        );
    }

    #[test]
    fn the_exit_error_of_a_wrong_key_is_typed() {
        let err = classify_failure(Some(12), &fixture("wrong_password.err"));

        assert!(matches!(err, ResticError::WrongKey), "{err:?}");
        assert_eq!(err.to_string(), "That key does not open this backup.");
    }

    #[test]
    fn the_exit_error_of_a_missing_repository_is_typed() {
        let err = classify_failure(Some(10), &fixture("no_repo.err"));

        assert!(matches!(err, ResticError::NoRepository), "{err:?}");
        assert_eq!(err.to_string(), "No backup was found in that folder.");
    }

    #[test]
    fn the_exit_error_of_a_lock_keeps_the_first_line_of_its_message() {
        let err = classify_failure(Some(11), &fixture("locked.err"));

        let ResticError::Locked { message } = err else {
            panic!("expected Locked, got {err:?}");
        };
        assert!(
            message.starts_with(
                "unable to create lock in backend: repository is already locked by PID"
            ),
            "{message}"
        );
        assert!(!message.contains('\n'));
    }

    #[test]
    fn another_failure_keeps_restic_message_and_no_fault() {
        let stderr = br#"{"message_type":"exit_error","code":1,"message":"Fatal: something odd"}"#;

        let err = classify_failure(Some(1), stderr);

        let ResticError::Failed { message, fault } = err else {
            panic!("expected Failed, got {err:?}");
        };
        assert_eq!(message, "Fatal: something odd");
        assert_eq!(fault, None);
    }

    #[test]
    fn a_plain_stderr_gives_its_last_line() {
        let err = classify_failure(Some(1), b"warning\nFatal: nope\n\n");

        assert!(matches!(&err, ResticError::Failed { message, .. } if message == "Fatal: nope"));
    }

    #[test]
    fn an_empty_stderr_names_the_exit_code_or_the_signal() {
        let by_code = classify_failure(Some(3), b"");
        let by_signal = classify_failure(None, b"");

        assert!(
            matches!(&by_code, ResticError::Failed { message, .. } if message == "exit code 3")
        );
        assert!(
            matches!(&by_signal, ResticError::Failed { message, .. } if message == "killed by a signal")
        );
    }

    #[test]
    fn rclone_text_in_stderr_gives_the_account_fault() {
        let stderr = b"2026/01/01 ERROR : couldn't login: oauth2: token expired and refresh token is not set\n";

        let err = classify_failure(Some(1), stderr);

        assert!(
            matches!(
                err,
                ResticError::Failed {
                    fault: Some(Fault::Auth),
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn a_fault_that_is_not_about_the_account_is_dropped() {
        let stderr = b"directory is empty: nothing to see\n";

        let err = classify_failure(Some(1), stderr);

        assert!(
            matches!(err, ResticError::Failed { fault: None, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn the_init_fixture_parses() {
        let result: InitResult = serde_json::from_slice(&fixture("init.out")).unwrap();

        assert_eq!(result.id.len(), 64);
        assert!(result.repository.starts_with("rclone:testremote:"));
    }

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert_eq!(parse_version("0.19.1"), Some((0, 19, 1)));
        assert_eq!(parse_version("0.9.10"), Some((0, 9, 10)));
        assert_eq!(parse_version("0.19"), None);
        assert_eq!(parse_version("0.19.1.2"), None);
        assert_eq!(parse_version("beta"), None);
    }

    #[test]
    fn a_version_below_the_minimum_is_refused() {
        assert!(matches!(
            check_version("0.17.0"),
            Err(ResticError::TooOld { .. })
        ));
        assert!(matches!(
            check_version("0.9.9"),
            Err(ResticError::TooOld { .. })
        ));
        assert!(matches!(
            check_version("nonsense"),
            Err(ResticError::BadOutput(_))
        ));
    }

    #[test]
    fn the_minimum_the_pinned_and_a_newer_version_are_accepted_and_only_a_different_one_warns() {
        assert!(!check_version(RESTIC_VERSION).unwrap());
        assert!(check_version(RESTIC_MIN_VERSION).unwrap());
        assert!(check_version("0.20.0").unwrap());
        assert!(check_version("1.0.0").unwrap());
    }

    fn executable(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn the_override_wins_and_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let custom = executable(dir.path(), "my-restic");
        let other = tempfile::tempdir().unwrap();
        executable(other.path(), "restic");

        assert_eq!(
            resolve_binary(Some(custom.as_os_str()), Some(other.path().as_os_str())).unwrap(),
            custom
        );
        assert!(matches!(
            resolve_binary(
                Some(dir.path().join("missing").as_os_str()),
                Some(other.path().as_os_str())
            ),
            Err(ResticError::NotFound)
        ));
    }

    #[test]
    fn without_an_override_the_first_executable_restic_on_the_path_is_used() {
        let first = tempfile::tempdir().unwrap();
        std::fs::write(first.path().join("restic"), "not executable").unwrap();
        let second = tempfile::tempdir().unwrap();
        let wanted = executable(second.path(), "restic");
        let path = std::env::join_paths([first.path(), second.path()]).unwrap();

        assert_eq!(resolve_binary(None, Some(&path)).unwrap(), wanted);
        assert_eq!(
            resolve_binary(Some(OsStr::new("")), Some(&path)).unwrap(),
            wanted
        );
        assert!(matches!(
            resolve_binary(None, None),
            Err(ResticError::NotFound)
        ));
    }

    #[test]
    fn not_found_says_to_reinstall() {
        assert_eq!(
            ResticError::NotFound.to_string(),
            "restic not found; reinstall syncfast"
        );
    }

    #[test]
    fn a_machine_without_a_name_is_an_error_and_a_name_is_trimmed() {
        assert!(matches!(clean_host("  \n"), Err(ResticError::Host(_))));
        assert_eq!(clean_host("laptop\n").unwrap(), "laptop");
    }

    #[test]
    fn the_host_of_this_machine_is_read_once_and_not_empty() {
        let first = backup_host().unwrap();

        assert!(!first.is_empty());
        assert_eq!(backup_host().unwrap(), first);
    }
}
