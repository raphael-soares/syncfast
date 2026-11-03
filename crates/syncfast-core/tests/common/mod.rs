//! Helpers shared by the apply and watch integration tests.

#![allow(dead_code, clippy::unwrap_used)]

use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use syncfast_core::paths::Paths;
use syncfast_core::rclone::Rclone;

pub struct Bed {
    pub _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub paths: Paths,
    pub rclone: Rclone,
}

impl Bed {
    /// An explicit `Paths` under a fresh tempdir and an rclone stand-in
    /// that lists one account, `testremote`.
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let paths = Paths::new(
            root.join("home"),
            root.join("config"),
            root.join("state"),
            root.join("data"),
            root.join("run"),
        );
        let rclone = Rclone::new(script(&root, "rclone", "echo testremote:"));
        Self {
            _dir: dir,
            root,
            paths,
            rclone,
        }
    }

    pub fn local(&self, id: &str) -> PathBuf {
        self.root.join(format!("local-{id}"))
    }

    pub fn remote(&self, id: &str) -> PathBuf {
        self.root.join(format!("remote-{id}"))
    }

    /// `(id, enabled)` pairs, each with its own local and remote folder.
    pub fn write_config(&self, pairs: &[(&str, bool)]) {
        let mut text = String::from("version = 1\n");
        for (id, enabled) in pairs {
            let local = self.local(id);
            fs::create_dir_all(&local).unwrap();
            write!(
                text,
                "\n[[pair]]\nid = \"{id}\"\nname = \"{id}\"\nlocal = \"{}\"\nremote = \"testremote:{}\"\nenabled = {enabled}\n",
                local.display(),
                self.remote(id).display(),
            )
            .unwrap();
        }
        self.write_config_text(&text);
    }

    pub fn write_config_text(&self, text: &str) {
        let file = self.paths.config_file();
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
}

/// An executable shell script `name` in `dir` with `body`.
pub fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}
