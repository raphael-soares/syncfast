//! A throwaway home for the real `syncfast` binary: its own XDG folders, an
//! rclone config with one `local` remote, no systemd and no desktop bus.

#![allow(dead_code, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub struct Bed {
    pub _dir: tempfile::TempDir,
    pub root: PathBuf,
}

/// The real rclone is needed; without it the test is skipped.
#[allow(clippy::print_stderr)]
pub fn bed() -> Option<Bed> {
    if Command::new("rclone").arg("version").output().is_err() {
        eprintln!("skipping: rclone is not installed");
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("rclone.conf"), "[testremote]\ntype = local\n").unwrap();
    for folder in ["home", "config", "state", "data", "run", "local", "remote"] {
        fs::create_dir_all(root.join(folder)).unwrap();
    }
    Some(Bed { _dir: dir, root })
}

pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|err| panic!("not JSON ({err}): {}", self.stdout))
    }
}

impl Bed {
    pub fn local(&self) -> PathBuf {
        self.root.join("local")
    }

    pub fn remote(&self) -> PathBuf {
        self.root.join("remote")
    }

    pub fn remote_arg(&self) -> String {
        format!("testremote:{}", self.remote().display())
    }

    /// The `syncfast` command with this bed's environment.
    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_syncfast"));
        command
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("RCLONE_CONFIG", self.root.join("rclone.conf"))
            .env("SYNCFAST_SYSTEMD", "off")
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
            .env("DBUS_SYSTEM_BUS_ADDRESS", "unix:path=/nonexistent")
            .env_remove("SYNCFAST_RCLONE");
        command
    }

    pub fn run(&self, args: &[&str]) -> Run {
        let output: Output = self.command().args(args).output().unwrap();
        Run {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// `add` the `docs` sync on `local/` and `remote/`.
    pub fn add_docs(&self) -> Run {
        let local = self.local();
        let remote = self.remote_arg();
        self.run(&[
            "add",
            "--json",
            "--name",
            "Docs",
            "--local",
            local.to_str().unwrap(),
            "--remote",
            &remote,
        ])
    }

    /// `docs` synced once with four files on both sides.
    pub fn synced_docs(&self) {
        for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            write(&self.local().join(name), &format!("content of {name}"));
        }
        assert_eq!(self.add_docs().code, 0);
        let shown = self.run(&["review", "show", "docs", "--json"]);
        let hash = shown.json()["filters_hash"].as_str().unwrap().to_owned();
        let confirmed = self.run(&[
            "review",
            "confirm",
            "docs",
            "--filters-hash",
            &hash,
            "--json",
        ]);
        assert_eq!(confirmed.code, 0, "{}", confirmed.stdout);
        assert_eq!(confirmed.json()["sync"]["status"], "UpToDate");
    }
}

pub fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}
