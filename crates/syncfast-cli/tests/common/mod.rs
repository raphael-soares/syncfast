//! A throwaway home for the real `syncfast` binary: its own XDG folders, an
//! rclone config with one `local` remote, no systemd and no desktop bus.

#![allow(dead_code, clippy::unwrap_used, clippy::panic)]

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// No command of a test lasts this long; a hung restic or rclone is killed.
pub const RUN_DEADLINE: Duration = Duration::from_secs(150);

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

/// `bed()` for the backup tests: restic is needed too.
#[allow(clippy::print_stderr)]
pub fn backup_bed() -> Option<Bed> {
    let bed = bed()?;
    if Command::new("restic").arg("version").output().is_err() {
        eprintln!("skipping: restic is not installed");
        return None;
    }
    Some(bed)
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
            .env("SYNCFAST_KEYSTORE", "plain")
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent")
            .env("DBUS_SYSTEM_BUS_ADDRESS", "unix:path=/nonexistent")
            .env_remove("SYNCFAST_RCLONE");
        command
    }

    pub fn run(&self, args: &[&str]) -> Run {
        self.run_in(&self.root, args)
    }

    /// `run` from the working folder `cwd`.
    pub fn run_in(&self, cwd: &Path, args: &[&str]) -> Run {
        let mut child = self
            .command()
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let drain = |mut pipe: Box<dyn std::io::Read + Send>| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes);
                String::from_utf8_lossy(&bytes).into_owned()
            })
        };
        let stdout = drain(Box::new(child.stdout.take().unwrap()));
        let stderr = drain(Box::new(child.stderr.take().unwrap()));
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                started.elapsed() < RUN_DEADLINE,
                "syncfast {args:?} ran past the deadline"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        Run {
            code: status.code().unwrap_or(-1),
            stdout: stdout.join().unwrap(),
            stderr: stderr.join().unwrap(),
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
