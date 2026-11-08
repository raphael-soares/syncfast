//! The restic wrapper and the repository functions against a real restic
//! and rclone, on `type = local` remotes and a folder.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use syncfast_core::backup::destination::{
    Destination, RepositoryError, create_repository, open_repository,
};
use syncfast_core::backup::key::Key;
use syncfast_core::paths::Paths;
use syncfast_core::rclone::{Rclone, RcloneError};
use syncfast_core::restic::{Repository, Restic, ResticError};

struct Bed {
    _dir: tempfile::TempDir,
    root: PathBuf,
    restic: Restic,
}

#[allow(clippy::print_stderr)]
async fn bed() -> Option<Bed> {
    let found = match Rclone::from_env() {
        Ok(found) => found,
        Err(RcloneError::RcloneNotFound) => {
            eprintln!("skipping: rclone is not installed");
            return None;
        }
        Err(err) => panic!("rclone is installed but unusable: {err}"),
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let conf = root.join("rclone.conf");
    fs::write(&conf, "[testremote]\ntype = local\n").unwrap();
    let paths = Paths::new(
        root.join("home"),
        root.join("config"),
        root.join("state"),
        root.join("data"),
        root.join("run"),
    );
    let rclone = Rclone::new(found.bin().to_owned()).with_config(conf);
    let restic = match Restic::from_env(rclone, &paths).await {
        Ok(restic) => restic,
        Err(ResticError::NotFound) => {
            eprintln!("skipping: restic is not installed");
            return None;
        }
        Err(err) => panic!("restic is installed but unusable: {err}"),
    };
    Some(Bed {
        _dir: dir,
        root,
        restic,
    })
}

impl Bed {
    fn repository(&self, destination: Destination) -> Repository {
        Repository::new(destination, self.root.join("work"))
    }

    fn account(&self, name: &str) -> Repository {
        self.repository(Destination::Account {
            name: "testremote".to_owned(),
            path: self.root.join(name).to_string_lossy().into_owned(),
        })
    }

    fn folder(&self, name: &str) -> Repository {
        self.repository(Destination::Local(self.root.join(name)))
    }
}

const DEADLINE: Duration = Duration::from_secs(90);

macro_rules! deadline_tests {
    ($(async fn $name:ident() $body:block)*) => {
        $(
            #[tokio::test]
            async fn $name() {
                tokio::time::timeout(DEADLINE, async $body)
                    .await
                    .expect("restic or rclone hung past the deadline");
            }
        )*
    };
}

deadline_tests! {
async fn a_repository_is_created_on_an_account_and_opens_with_its_key() {
    let Some(bed) = bed().await else { return };
    let key = Key::generate().unwrap();
    let repo = bed.account("repo");

    let made = create_repository(&bed.restic, &repo, &key).await.unwrap();

    assert_eq!(made.id.len(), 64);
    open_repository(&bed.restic, &repo, &key).await.unwrap();
    assert!(bed.root.join("repo/config").is_file());
    assert!(bed.restic.snapshots(&repo, &key).await.unwrap().is_empty());
}

async fn a_repository_is_created_in_a_local_folder() {
    let Some(bed) = bed().await else { return };
    let key = Key::generate().unwrap();
    let repo = bed.folder("disk");

    create_repository(&bed.restic, &repo, &key).await.unwrap();

    open_repository(&bed.restic, &repo, &key).await.unwrap();
    assert!(bed.root.join("disk/config").is_file());
}

async fn another_key_does_not_open_the_repository() {
    let Some(bed) = bed().await else { return };
    let repo = bed.account("repo");
    create_repository(&bed.restic, &repo, &Key::generate().unwrap())
        .await
        .unwrap();

    let err = open_repository(&bed.restic, &repo, &Key::generate().unwrap())
        .await
        .unwrap_err();

    assert!(matches!(err, ResticError::WrongKey), "{err}");
    assert_eq!(err.to_string(), "That key does not open this backup.");
}

async fn a_folder_without_a_repository_is_reported_as_such() {
    let Some(bed) = bed().await else { return };
    fs::create_dir_all(bed.root.join("empty")).unwrap();

    for repo in [
        bed.account("empty"),
        bed.account("absent"),
        bed.folder("empty"),
    ] {
        let err = open_repository(&bed.restic, &repo, &Key::generate().unwrap())
            .await
            .unwrap_err();

        assert!(matches!(err, ResticError::NoRepository), "{err}");
    }
}

async fn a_folder_with_files_never_becomes_a_repository() {
    let Some(bed) = bed().await else { return };
    let key = Key::generate().unwrap();
    for name in ["account-folder", "local-folder"] {
        fs::create_dir_all(bed.root.join(name)).unwrap();
        fs::write(bed.root.join(name).join("photo.jpg"), b"x").unwrap();
    }

    for repo in [bed.account("account-folder"), bed.folder("local-folder")] {
        let err = create_repository(&bed.restic, &repo, &key)
            .await
            .unwrap_err();

        assert!(matches!(err, RepositoryError::NotEmpty), "{err}");
    }
    assert!(!bed.root.join("account-folder/config").exists());
    assert!(!bed.root.join("local-folder/config").exists());
}

async fn an_empty_existing_folder_can_become_a_repository() {
    let Some(bed) = bed().await else { return };
    fs::create_dir_all(bed.root.join("ready")).unwrap();

    create_repository(
        &bed.restic,
        &bed.account("ready"),
        &Key::generate().unwrap(),
    )
    .await
    .unwrap();

    assert!(bed.root.join("ready/config").is_file());
}

async fn a_backup_shows_up_in_the_snapshots_with_this_machines_name() {
    let Some(bed) = bed().await else { return };
    let key = Key::generate().unwrap();
    let repo = bed.account("repo");
    create_repository(&bed.restic, &repo, &key).await.unwrap();
    let source = bed.root.join("docs");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("a.txt"), b"hello").unwrap();
    let source_arg = source.to_string_lossy().into_owned();

    bed.restic
        .run(&repo, &key, &["backup", "--json", &source_arg], false)
        .await
        .unwrap();
    let snapshots = bed.restic.snapshots(&repo, &key).await.unwrap();

    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].paths, [source_arg]);
    assert_eq!(
        snapshots[0].host,
        syncfast_core::restic::backup_host().unwrap()
    );
    assert_eq!(snapshots[0].files, Some(1));
}

async fn unlock_succeeds_on_a_repository_without_locks() {
    let Some(bed) = bed().await else { return };
    let key = Key::generate().unwrap();
    let repo = bed.account("repo");
    create_repository(&bed.restic, &repo, &key).await.unwrap();

    bed.restic.unlock(&repo, &key).await.unwrap();
}

async fn limits_do_not_stop_a_repository_from_opening() {
    let Some(bed) = bed().await else { return };
    let key = Key::generate().unwrap();
    let plain = bed.account("repo");
    create_repository(&bed.restic, &plain, &key).await.unwrap();
    let limited = plain.with_limits(
        syncfast_core::behavior::Bandwidth::KiBPerSec(1000),
        syncfast_core::behavior::Bandwidth::KiBPerSec(1000),
    );

    open_repository(&bed.restic, &limited, &key).await.unwrap();
}
}
