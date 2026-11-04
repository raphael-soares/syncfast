//! The operations an agent uses, against the real rclone with remotes of
//! type `local`. Skipped, with a warning, when rclone is not installed.

#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jiff::Zoned;
use syncfast_core::network::{BoxFuture, Network};
use syncfast_core::notify::{Notice, Notifier};
use syncfast_core::ops::{
    ExploreOptions, Keep, NewSync, OpError, Ops, TransferOptions, TransferStarter, run_transfer,
};
use syncfast_core::paths::Paths;
use syncfast_core::rclone::Rclone;
use syncfast_core::run::{RunEnv, run};
use syncfast_core::state::try_lock_pair;
use syncfast_core::units::{Systemctl, SystemdMode};
use syncfast_core::watch::Trigger;

const WAIT: Duration = Duration::from_secs(60);

struct Online;

impl Network for Online {
    fn is_online(&self) -> BoxFuture<'_, bool> {
        Box::pin(async { true })
    }
}

struct Silent;

impl Notifier for Silent {
    fn notify(&self, _notice: &Notice) {}
}

struct NoSystemctl;

impl Systemctl for NoSystemctl {
    fn run<'a>(&'a self, _args: &'a [&'a str]) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// Starts a round as a task of this process, what systemd or a child
/// process does for the real trigger.
struct InProcess {
    paths: Paths,
    rclone: Rclone,
}

impl Trigger for InProcess {
    fn start<'a>(&'a self, pair_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        let (paths, rclone, id) = (self.paths.clone(), self.rclone.clone(), pair_id.to_owned());
        Box::pin(async move {
            tokio::spawn(async move {
                let clock = Zoned::now;
                let env = RunEnv {
                    paths: &paths,
                    rclone: &rclone,
                    network: &Online,
                    notifier: &Silent,
                    clock: &clock,
                    forward_signals: false,
                };
                let _ = run(&env, &id).await;
            });
            Ok(())
        })
    }
}

/// Runs a transfer as a task of this process, what the detached
/// `transfer-run` does for the real starter.
struct InProcessTransfers {
    paths: Paths,
    rclone: Rclone,
}

impl TransferStarter for InProcessTransfers {
    fn start<'a>(&'a self, transfer_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        let (paths, rclone, id) = (
            self.paths.clone(),
            self.rclone.clone(),
            transfer_id.to_owned(),
        );
        Box::pin(async move {
            tokio::spawn(async move {
                let _ = run_transfer(&paths, &rclone, &id).await;
            });
            Ok(())
        })
    }
}

struct Bed {
    _dir: tempfile::TempDir,
    root: PathBuf,
    ops: Ops,
}

#[allow(clippy::print_stderr)]
fn bed_with(rclone_conf: &str) -> Option<Bed> {
    let Ok(found) = Rclone::from_env() else {
        eprintln!("skipping: rclone is not installed");
        return None;
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let conf = root.join("rclone.conf");
    fs::write(&conf, rclone_conf).unwrap();
    let paths = Paths::new(
        root.join("home"),
        root.join("config"),
        root.join("state"),
        root.join("data"),
        root.join("run"),
    );
    fs::create_dir_all(&paths.home).unwrap();
    let rclone = Rclone::new(found.bin().to_owned()).with_config(conf);
    let ops = Ops {
        trigger: Arc::new(InProcess {
            paths: paths.clone(),
            rclone: rclone.clone(),
        }),
        transfers: Arc::new(InProcessTransfers {
            paths: paths.clone(),
            rclone: rclone.clone(),
        }),
        paths,
        rclone,
        network: Box::new(Online),
        notifier: Box::new(Silent),
        systemctl: Arc::new(NoSystemctl),
        systemd: SystemdMode::Off,
        bin: PathBuf::from("/bin/true"),
        flatpak_id: None,
    };
    Some(Bed {
        _dir: dir,
        root,
        ops,
    })
}

fn bed() -> Option<Bed> {
    bed_with("[testremote]\ntype = local\n")
}

impl Bed {
    fn local(&self) -> PathBuf {
        self.root.join("local")
    }

    fn remote(&self) -> PathBuf {
        self.root.join("remote")
    }

    fn new_sync(&self) -> NewSync {
        fs::create_dir_all(self.local()).unwrap();
        NewSync {
            name: "Docs".into(),
            local: self.local().display().to_string(),
            remote: format!("testremote:{}", self.remote().display()),
            create_remote: true,
            ..NewSync::default()
        }
    }

    /// A sync that finished its first round with `a.txt`, `b.txt`, `c.txt`
    /// and `d.txt` on both sides.
    async fn synced_docs(&self) {
        fs::create_dir_all(self.local()).unwrap();
        for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            fs::write(self.local().join(name), format!("content of {name}")).unwrap();
        }
        self.confirmed_first_round().await;
    }

    async fn confirmed_first_round(&self) {
        let added = self.ops.add(self.new_sync()).await.unwrap();
        let review = self.ops.show_review(&added.sync.id).await.unwrap();
        let confirmed = self
            .ops
            .confirm_review(&added.sync.id, &review.filters_hash, WAIT)
            .await
            .unwrap();
        assert_eq!(confirmed.sync.status, "UpToDate");
    }
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[tokio::test]
async fn adding_a_sync_saves_it_held_for_its_first_review_and_copies_nothing() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("a.txt"), "hello");

    let added = bed.ops.add(bed.new_sync()).await.unwrap();

    assert_eq!(added.sync.id, "docs");
    assert_eq!(added.sync.status, "NeedsReview");
    assert_eq!(
        added.sync.gate.as_ref().map(|gate| gate.kind.as_str()),
        Some("needs_review_first_run")
    );
    assert!(!bed.remote().join("a.txt").exists());
    let config = fs::read_to_string(bed.ops.paths.config_file()).unwrap();
    assert!(config.contains("id = \"docs\""), "{config}");
}

#[tokio::test]
async fn adding_a_sync_on_an_unknown_account_says_what_the_config_says() {
    let Some(bed) = bed() else { return };
    let mut new = bed.new_sync();
    new.remote = "nowhere:Docs".into();

    let err = bed.ops.add(new).await.unwrap_err();

    assert_eq!(
        err.to_string(),
        "account \"nowhere\" is not set up in rclone"
    );
    assert_eq!(err.exit_code(), 2);
    assert!(!bed.ops.paths.config_file().exists());
}

#[tokio::test]
async fn a_missing_local_folder_is_created_only_when_asked() {
    let Some(bed) = bed() else { return };
    let mut new = bed.new_sync();
    new.local = bed.root.join("later").display().to_string();

    let refused = bed.ops.add(new.clone()).await.unwrap_err();
    new.create_local = true;
    let created = bed.ops.add(new).await.unwrap();

    assert!(refused.to_string().contains("does not exist"), "{refused}");
    assert!(bed.root.join("later").is_dir());
    assert_eq!(created.sync.id, "docs");
}

#[tokio::test]
async fn a_review_without_deletions_confirmed_by_the_agent_syncs_both_sides() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("a.txt"), "from here");
    write(&bed.remote().join("b.txt"), "from there");
    let added = bed.ops.add(bed.new_sync()).await.unwrap();

    let review = bed.ops.show_review(&added.sync.id).await.unwrap();
    let confirmed = bed
        .ops
        .confirm_review(&added.sync.id, &review.filters_hash, WAIT)
        .await
        .unwrap();

    assert!(review.agent_can_confirm);
    assert_eq!((review.counts.upload, review.counts.download), (1, 1));
    assert!(confirmed.finished);
    assert_eq!(confirmed.sync.status, "UpToDate");
    assert_eq!(
        fs::read_to_string(bed.remote().join("a.txt")).unwrap(),
        "from here"
    );
    assert_eq!(
        fs::read_to_string(bed.local().join("b.txt")).unwrap(),
        "from there"
    );
}

#[tokio::test]
async fn a_review_with_deletions_is_refused_with_exit_four_and_nothing_changes() {
    let Some(bed) = bed() else { return };
    bed.synced_docs().await;
    for name in ["b.txt", "c.txt", "d.txt"] {
        fs::remove_file(bed.local().join(name)).unwrap();
    }
    bed.ops.sync_now(Some("docs"), WAIT).await.unwrap();
    let review = bed.ops.show_review("docs").await.unwrap();

    let err = bed
        .ops
        .confirm_review("docs", &review.filters_hash, WAIT)
        .await
        .unwrap_err();

    assert!(review.deletes_files && !review.agent_can_confirm);
    assert!(matches!(err, OpError::DeletesNeedUser));
    assert_eq!(err.exit_code(), 4);
    assert_eq!(
        err.to_string(),
        "This review deletes files. Confirm it in syncfast."
    );
    assert!(!bed.ops.paths.confirm_file("docs").exists());
    let status = bed.ops.show("docs").await.unwrap();
    assert_eq!(status.sync.status, "NeedsReview");
    for name in ["b.txt", "c.txt", "d.txt"] {
        assert!(bed.remote().join(name).exists(), "{name} left the remote");
    }
}

#[tokio::test]
async fn a_hash_that_is_not_the_one_shown_is_refused_with_exit_five() {
    let Some(bed) = bed() else { return };
    bed.ops.add(bed.new_sync()).await.unwrap();
    bed.ops.show_review("docs").await.unwrap();

    let err = bed
        .ops
        .confirm_review("docs", "0000", WAIT)
        .await
        .unwrap_err();

    assert!(matches!(err, OpError::ReviewNotShown(_)), "{err}");
    assert_eq!(err.exit_code(), 5);
    assert!(!bed.ops.paths.confirm_file("docs").exists());
}

#[tokio::test]
async fn confirming_without_showing_the_review_first_is_refused() {
    let Some(bed) = bed() else { return };
    bed.ops.add(bed.new_sync()).await.unwrap();

    let err = bed
        .ops
        .confirm_review("docs", "anything", WAIT)
        .await
        .unwrap_err();

    assert_eq!(err.exit_code(), 5);
    assert!(err.to_string().contains("show the review first"), "{err}");
}

#[tokio::test]
async fn a_review_shown_before_the_filters_changed_is_refused() {
    let Some(bed) = bed() else { return };
    bed.ops.add(bed.new_sync()).await.unwrap();
    let old = bed.ops.show_review("docs").await.unwrap();
    bed.ops
        .set_filters("docs", None, Some(vec!["- *.bak".into()]))
        .await
        .unwrap();

    let err = bed
        .ops
        .confirm_review("docs", &old.filters_hash, WAIT)
        .await
        .unwrap_err();

    assert_eq!(err.exit_code(), 5);
    assert!(err.to_string().contains("filters changed"), "{err}");
    assert!(!bed.ops.paths.confirm_file("docs").exists());
}

#[tokio::test]
async fn a_review_is_confirmed_once() {
    let Some(bed) = bed() else { return };
    bed.confirmed_first_round().await;
    let hash = bed.ops.show("docs").await.unwrap().filters_hash;

    let err = bed
        .ops
        .confirm_review("docs", &hash, WAIT)
        .await
        .unwrap_err();

    assert!(matches!(err, OpError::Invalid(_)), "{err}");
}

#[tokio::test]
async fn saving_filters_on_a_synced_sync_holds_it_for_the_review_of_the_new_filters() {
    let Some(bed) = bed() else { return };
    bed.synced_docs().await;

    let saved = bed
        .ops
        .set_filters("docs", Some(vec![]), Some(vec!["- *.bak".into()]))
        .await
        .unwrap();

    assert!(saved.review_needed);
    assert_eq!(saved.sync.status, "NeedsReview");
    assert_eq!(
        saved.sync.gate.as_ref().map(|gate| gate.kind.as_str()),
        Some("needs_review_filters_changed")
    );
    assert_eq!(saved.sync.filters, ["- *.bak"]);
    assert!(saved.sync.presets.is_empty());
}

#[tokio::test]
async fn saving_filters_on_a_sync_held_for_mass_deletion_holds_it_for_the_new_review_instead() {
    let Some(bed) = bed() else { return };
    bed.synced_docs().await;
    for name in ["b.txt", "c.txt", "d.txt"] {
        fs::remove_file(bed.local().join(name)).unwrap();
    }
    let held = bed.ops.sync_now(Some("docs"), WAIT).await.unwrap();

    let saved = bed
        .ops
        .set_filters("docs", None, Some(vec!["- *.bak".into()]))
        .await
        .unwrap();

    let kind = |view: &syncfast_core::ops::PairView| {
        view.gate.as_ref().map(|gate| gate.kind.as_str().to_owned())
    };
    assert_eq!(
        kind(&held.rounds[0].sync).as_deref(),
        Some("too_many_deletes")
    );
    assert_eq!(
        kind(&saved.sync).as_deref(),
        Some("needs_review_filters_changed")
    );
}

#[tokio::test]
async fn a_rule_without_a_sign_is_refused_with_the_config_message() {
    let Some(bed) = bed() else { return };
    bed.ops.add(bed.new_sync()).await.unwrap();

    let err = bed
        .ops
        .set_filters("docs", None, Some(vec!["*.bak".into()]))
        .await
        .unwrap_err();

    assert_eq!(
        err.to_string(),
        "filter line 1 must start with \"+ \" or \"- \""
    );
}

#[tokio::test]
async fn testing_filters_counts_the_excluded_files_without_saving_anything() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("keep.txt"), "k");
    write(&bed.local().join("old/a.bak"), "a");
    write(&bed.local().join("old/b.bak"), "b");
    write(&bed.local().join("c.bak"), "c");

    let test = bed
        .ops
        .test_filters(
            None,
            Some(&bed.local().display().to_string()),
            Some(vec![]),
            Some(vec!["- *.bak".into()]),
        )
        .await
        .unwrap();

    assert_eq!(test.summary, "3 of 4 files excluded");
    assert_eq!(test.folders.len(), 1);
    assert_eq!(
        (
            test.folders[0].folder.as_str(),
            test.folders[0].excluded_files
        ),
        ("old", 2)
    );
    assert!(!bed.ops.paths.config_file().exists());
}

#[tokio::test]
async fn exploring_a_folder_gives_exact_counts_sizes_and_extensions_and_no_content() {
    let Some(bed) = bed() else { return };
    let root = bed.root.join("tree");
    write(&root.join("notes/a.md"), "SECRET-ALPHA");
    write(&root.join("notes/b.md"), "SECRET-BRAVO!");
    write(&root.join("notes/deep/c.png"), &"x".repeat(100));
    write(&root.join("top.txt"), "t");
    fs::create_dir_all(root.join("empty")).unwrap();

    let found = bed
        .ops
        .explore(&root.display().to_string(), ExploreOptions::default())
        .await
        .unwrap();

    assert_eq!((found.files, found.folders), (4, 3));
    assert_eq!(found.bytes, 12 + 13 + 100 + 1);
    let notes = found.tree.iter().find(|stat| stat.path == "notes").unwrap();
    assert_eq!((notes.files, notes.direct_files, notes.bytes), (3, 2, 125));
    assert_eq!(notes.subfolders, 1);
    assert_eq!(notes.top_extensions[0].extension, "md");
    assert_eq!(notes.top_extensions[0].files, 2);
    assert!(found.tree.iter().any(|stat| stat.path == "empty"));
    let json = serde_json::to_string(&found).unwrap();
    assert!(!json.contains("SECRET"), "file content leaked: {json}");
}

#[tokio::test]
async fn exploring_a_remote_path_reads_it_through_rclone() {
    let Some(bed) = bed() else { return };
    write(&bed.remote().join("docs/a.txt"), "abc");

    let found = bed
        .ops
        .explore(
            &format!("testremote:{}", bed.remote().display()),
            ExploreOptions::default(),
        )
        .await
        .unwrap();

    assert_eq!((found.files, found.bytes), (1, 3));
    assert_eq!(found.location, syncfast_core::ops::Location::Remote);
}

#[tokio::test]
async fn accounts_list_names_and_types_and_never_a_secret() {
    let conf = "[mine]\ntype = drive\nclient_id = my-id\nclient_secret = SUPER-SECRET\ntoken = {\"access_token\":\"TOKEN-VALUE\"}\n\n[shared]\ntype = drive\n\n[disk]\ntype = local\n";
    let Some(bed) = bed_with(conf) else { return };

    let accounts = bed.ops.accounts().await.unwrap();

    let named = |name: &str| {
        accounts
            .iter()
            .find(|account| account.name == name)
            .unwrap()
    };
    assert_eq!(named("mine").kind, "drive");
    assert!(!named("mine").shared_client_id);
    assert!(named("shared").shared_client_id && named("shared").warning.is_some());
    assert!(!named("disk").shared_client_id);
    let json = serde_json::to_string(&accounts).unwrap();
    assert!(
        !json.contains("SUPER-SECRET") && !json.contains("TOKEN-VALUE"),
        "{json}"
    );
}

#[tokio::test]
async fn listing_and_creating_remote_folders_goes_through_the_remote() {
    let Some(bed) = bed() else { return };
    let root = format!("testremote:{}", bed.remote().display());
    fs::create_dir_all(bed.remote().join("Beta")).unwrap();

    bed.ops.make_folder(&format!("{root}/alpha")).await.unwrap();
    let listed = bed.ops.folders(&root).await.unwrap();
    let refused = bed.ops.folders("/etc").await.unwrap_err();

    assert_eq!(listed.folders, ["alpha", "Beta"]);
    assert_eq!(refused.exit_code(), 2);
}

#[tokio::test]
async fn a_conflict_is_listed_and_keeping_both_renames_the_copy() {
    let Some(bed) = bed() else { return };
    bed.synced_docs().await;
    write(&bed.local().join("a.txt.conflict1"), "other side");

    let listed = bed.ops.conflicts("docs").await.unwrap();
    let resolved = bed
        .ops
        .resolve_conflict("docs", Path::new("a.txt.conflict1"), Keep::Both)
        .await
        .unwrap();

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].path, Path::new("a.txt.conflict1"));
    assert_eq!(listed[0].conflicted.size, 10);
    assert!(resolved.sync_started);
    assert!(bed.local().join("a (conflict 1).txt").exists());
    assert!(!bed.local().join("a.txt.conflict1").exists());
}

#[tokio::test]
async fn a_path_that_is_not_a_conflict_is_not_settled() {
    let Some(bed) = bed() else { return };
    bed.synced_docs().await;

    let err = bed
        .ops
        .resolve_conflict("docs", Path::new("a.txt"), Keep::Current)
        .await
        .unwrap_err();

    assert!(bed.local().join("a.txt").exists());
    assert!(err.to_string().contains("cannot settle a.txt"), "{err}");
}

#[tokio::test]
async fn pausing_and_resuming_change_the_config_and_the_status() {
    let Some(bed) = bed() else { return };
    bed.synced_docs().await;

    let paused = bed.ops.set_enabled(Some("docs"), false).await.unwrap();
    let resumed = bed.ops.set_enabled(None, true).await.unwrap();

    assert_eq!(paused.syncs[0].status, "Paused");
    assert_eq!(resumed.syncs[0].status, "UpToDate");
}

#[tokio::test]
async fn a_sync_the_watcher_could_not_watch_says_real_time_sync_is_off() {
    let Some(bed) = bed() else { return };
    bed.synced_docs().await;
    let file = bed.ops.paths.watch_limit_file();
    fs::write(&file, r#"{"pairs":["docs"]}"#).unwrap();

    let limited = bed.ops.status().await.unwrap();
    fs::remove_file(&file).unwrap();
    let watched = bed.ops.status().await.unwrap();

    assert_eq!(
        limited[0].notice,
        Some("Real-time sync is off: raise fs.inotify.max_user_watches.")
    );
    assert_eq!(watched[0].notice, None);
}

#[tokio::test]
async fn removing_a_sync_takes_it_out_and_unknown_ids_are_not_found() {
    let Some(bed) = bed() else { return };
    bed.synced_docs().await;

    let removed = bed.ops.remove("docs", true).await.unwrap();
    let again = bed.ops.remove("docs", false).await.unwrap_err();

    assert!(removed.versions_deleted);
    assert!(bed.ops.status().await.unwrap().is_empty());
    assert!(bed.local().join("a.txt").exists() && bed.remote().join("a.txt").exists());
    assert!(matches!(again, OpError::NotFound(_)));
    assert!(!bed.ops.paths.pair_state_dir("docs").exists());
}

#[tokio::test]
async fn a_round_shows_up_in_the_activity_with_its_counts_and_its_log() {
    let Some(bed) = bed() else { return };
    bed.synced_docs().await;
    write(&bed.local().join("e.txt"), "new");
    bed.ops.sync_now(Some("docs"), WAIT).await.unwrap();

    let rounds = bed.ops.activity("docs").await.unwrap();
    let log = bed.ops.round_log("docs", &rounds[0].round).await.unwrap();

    assert_eq!(rounds[0].status, "UpToDate");
    assert_eq!(rounds[0].uploaded, Some(1));
    assert!(
        log.messages
            .iter()
            .any(|line| line.contains("Bisync successful"))
    );
    let missing = bed.ops.round_log("docs", "nope.jsonl").await.unwrap_err();
    assert_eq!(missing.exit_code(), 2);
}

#[tokio::test]
async fn show_reports_the_filters_hash_and_the_counters_of_the_last_round() {
    let Some(bed) = bed() else { return };
    bed.synced_docs().await;

    let detail = bed.ops.show("docs").await.unwrap();

    assert_eq!(detail.filters_hash.len(), 64);
    assert_eq!(detail.last_run.unwrap().uploaded, Some(4));
    assert_eq!(detail.sync.status, "UpToDate");
}

#[tokio::test]
async fn a_theme_is_saved_beside_what_the_window_remembers_and_unknown_ones_are_refused() {
    let Some(bed) = bed() else { return };
    let gui = bed.ops.paths.state_dir().join("gui.json");
    write(&gui, "{\"welcome_done\": true, \"theme\": \"system\"}");
    write(
        &bed.ops.paths.config_dir().join("themes/noctalia.json"),
        "{}",
    );

    bed.ops.set_theme("noctalia.json").await.unwrap();
    let refused = bed.ops.set_theme("neon").await.unwrap_err();
    let info = bed.ops.theme().await;

    let saved: serde_json::Value = serde_json::from_str(&fs::read_to_string(gui).unwrap()).unwrap();
    assert_eq!(saved["theme"], "noctalia.json");
    assert_eq!(saved["welcome_done"], true);
    assert_eq!(info.current, "noctalia.json");
    assert_eq!(info.available, ["system", "light", "dark", "noctalia.json"]);
    assert_eq!(refused.exit_code(), 2);
}

impl Bed {
    /// `testremote:<root>/remote/<name>`.
    fn remote_at(&self, name: &str) -> String {
        format!("testremote:{}", self.remote().join(name).display())
    }

    fn options() -> TransferOptions {
        TransferOptions {
            dry_run: false,
            wait: WAIT,
        }
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[tokio::test]
async fn copying_a_local_folder_to_a_remote_one_leaves_the_source_and_creates_the_destination() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("photos/a.txt"), "aaaa");
    write(&bed.local().join("photos/deep/b.txt"), "bb");
    fs::create_dir_all(bed.local().join("photos/empty")).unwrap();
    let source = bed.local().join("photos");

    let copied = bed
        .ops
        .copy(
            source.to_str().unwrap(),
            &bed.remote_at("backup"),
            Bed::options(),
        )
        .await
        .unwrap();

    assert!(copied.finished);
    assert_eq!(copied.transfer.status, "done");
    assert_eq!(copied.transfer.operation, "copy");
    assert_eq!(copied.transfer.kind, "folder");
    assert_eq!((copied.transfer.files, copied.transfer.bytes), (2, 6));
    assert!(!copied.transfer.server_side);
    assert_eq!(read(&bed.remote().join("backup/a.txt")), "aaaa");
    assert_eq!(read(&bed.remote().join("backup/deep/b.txt")), "bb");
    assert!(bed.remote().join("backup/empty").is_dir());
    assert_eq!(read(&source.join("a.txt")), "aaaa");
}

#[tokio::test]
async fn a_remote_file_is_copied_to_this_computer_under_the_name_given() {
    let Some(bed) = bed() else { return };
    write(&bed.remote().join("notes/todo.txt"), "milk");
    let destination = bed.local().join("renamed.txt");

    let copied = bed
        .ops
        .copy(
            &bed.remote_at("notes/todo.txt"),
            destination.to_str().unwrap(),
            Bed::options(),
        )
        .await
        .unwrap();

    assert_eq!(copied.transfer.kind, "file");
    assert_eq!((copied.transfer.files, copied.transfer.bytes), (1, 4));
    assert_eq!(read(&destination), "milk");
    assert!(bed.remote().join("notes/todo.txt").exists());
}

#[tokio::test]
async fn a_tilde_names_the_home_folder_and_a_local_folder_moves_to_another() {
    let Some(bed) = bed() else { return };
    write(&bed.ops.paths.home.join("inbox/x.txt"), "x");

    let moved = bed
        .ops
        .r#move("~/inbox", "~/archive/inbox", Bed::options())
        .await
        .unwrap();

    assert!(moved.transfer.server_side);
    assert_eq!(read(&bed.ops.paths.home.join("archive/inbox/x.txt")), "x");
    assert!(!bed.ops.paths.home.join("inbox").exists());
}

#[tokio::test]
async fn moving_between_two_folders_of_one_account_renames_on_the_server() {
    let Some(bed) = bed() else { return };
    write(&bed.remote().join("inbox/a.txt"), "a");
    write(&bed.remote().join("inbox/sub/b.txt"), "b");
    fs::create_dir_all(bed.remote().join("inbox/empty")).unwrap();

    let moved = bed
        .ops
        .r#move(
            &bed.remote_at("inbox"),
            &bed.remote_at("archive/2026"),
            Bed::options(),
        )
        .await
        .unwrap();

    assert!(moved.transfer.server_side);
    assert_eq!(moved.transfer.operation, "move");
    assert_eq!(moved.transfer.status, "done");
    assert_eq!(read(&bed.remote().join("archive/2026/a.txt")), "a");
    assert_eq!(read(&bed.remote().join("archive/2026/sub/b.txt")), "b");
    assert!(bed.remote().join("archive/2026/empty").is_dir());
    assert!(!bed.remote().join("inbox").exists());
}

#[tokio::test]
async fn moving_between_two_accounts_deletes_the_source_only_after_the_transfer() {
    let Some(bed) = bed_with("[one]\ntype = local\n[two]\ntype = local\n") else {
        return;
    };
    write(&bed.remote().join("src/a.txt"), "a");
    write(&bed.remote().join("src/sub/b.txt"), "b");
    let from = format!("one:{}", bed.remote().join("src").display());
    let to = format!("two:{}", bed.remote().join("dst").display());

    let moved = bed.ops.r#move(&from, &to, Bed::options()).await.unwrap();

    assert!(!moved.transfer.server_side);
    assert_eq!(read(&bed.remote().join("dst/sub/b.txt")), "b");
    assert!(!bed.remote().join("src/a.txt").exists());
    assert!(!bed.remote().join("src").exists());
}

#[tokio::test]
async fn an_existing_destination_is_refused_with_two_and_nothing_is_overwritten_or_merged() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("docs/new.txt"), "new");
    write(&bed.remote().join("docs/old.txt"), "old");
    write(&bed.remote().join("file.txt"), "old file");
    write(&bed.local().join("file.txt"), "new file");

    let folder = bed
        .ops
        .copy(
            bed.local().join("docs").to_str().unwrap(),
            &bed.remote_at("docs"),
            Bed::options(),
        )
        .await
        .unwrap_err();
    let file = bed
        .ops
        .r#move(
            bed.local().join("file.txt").to_str().unwrap(),
            &bed.remote_at("file.txt"),
            Bed::options(),
        )
        .await
        .unwrap_err();

    assert_eq!(folder.exit_code(), 2);
    assert!(folder.to_string().contains("already exists"), "{folder}");
    assert_eq!(file.exit_code(), 2);
    assert!(!bed.remote().join("docs/new.txt").exists());
    assert_eq!(read(&bed.remote().join("file.txt")), "old file");
    assert_eq!(read(&bed.local().join("file.txt")), "new file");
    assert!(!bed.ops.paths.transfers_dir().exists());
}

#[tokio::test]
async fn a_missing_source_an_unknown_account_and_a_destination_inside_the_source_are_refused() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("docs/a.txt"), "a");
    let docs = bed.local().join("docs");
    let docs = docs.to_str().unwrap();

    let missing = bed
        .ops
        .copy(
            bed.local().join("nope").to_str().unwrap(),
            &bed.remote_at("x"),
            Bed::options(),
        )
        .await
        .unwrap_err();
    let account = bed
        .ops
        .copy(docs, "nowhere:x", Bed::options())
        .await
        .unwrap_err();
    let inside = bed
        .ops
        .copy(docs, &format!("{docs}/copy"), Bed::options())
        .await
        .unwrap_err();

    assert_eq!(missing.exit_code(), 2);
    assert!(missing.to_string().contains("does not exist"), "{missing}");
    assert_eq!(
        account.to_string(),
        "account \"nowhere\" is not set up in rclone"
    );
    assert_eq!(inside.exit_code(), 2);
    assert!(!bed.local().join("docs/copy").exists());
}

#[tokio::test]
async fn the_root_of_an_account_cannot_be_the_source() {
    let Some(bed) = bed() else { return };

    let err = bed
        .ops
        .copy("testremote:", "/tmp/never", Bed::options())
        .await
        .unwrap_err();

    assert_eq!(err.exit_code(), 2);
    assert!(err.to_string().contains("root of an account"), "{err}");
}

#[tokio::test]
async fn the_folder_of_a_sync_and_what_contains_it_cannot_be_moved_but_files_inside_can() {
    let Some(bed) = bed() else { return };
    bed.synced_docs().await;
    let local = bed.local();
    let parent_of_remote = format!("testremote:{}", bed.root.display());

    let local_folder = bed
        .ops
        .r#move(local.to_str().unwrap(), "~/elsewhere", Bed::options())
        .await
        .unwrap_err();
    let remote_folder = bed
        .ops
        .r#move(&bed.remote_at(""), &bed.remote_at("../x"), Bed::options())
        .await
        .unwrap_err();
    let ancestor = bed
        .ops
        .copy(&parent_of_remote, "~/everything", Bed::options())
        .await
        .unwrap_err();
    let inside = bed
        .ops
        .r#move(
            local.join("a.txt").to_str().unwrap(),
            &bed.remote_at("outside/a.txt"),
            Bed::options(),
        )
        .await
        .unwrap();

    for refused in [&local_folder, &remote_folder, &ancestor] {
        assert_eq!(refused.exit_code(), 2, "{refused}");
        assert!(refused.to_string().contains("\"docs\""), "{refused}");
    }
    assert!(local.is_dir() && bed.remote().join("b.txt").exists());
    assert_eq!(inside.transfer.status, "done");
    assert!(!local.join("a.txt").exists());
    assert_eq!(
        read(&bed.remote().join("outside/a.txt")),
        "content of a.txt"
    );
}

#[tokio::test]
async fn a_source_or_destination_inside_a_sync_that_is_running_is_refused_with_three() {
    let Some(bed) = bed() else { return };
    bed.synced_docs().await;
    write(&bed.root.join("outside.txt"), "outside");
    let round = try_lock_pair(&bed.ops.paths, "docs").unwrap();
    assert!(round.is_some());

    let from_inside = bed
        .ops
        .copy(
            bed.local().join("a.txt").to_str().unwrap(),
            "~/a.txt",
            Bed::options(),
        )
        .await
        .unwrap_err();
    let into_remote_side = bed
        .ops
        .copy(
            bed.root.join("outside.txt").to_str().unwrap(),
            &bed.remote_at("new.txt"),
            Bed::options(),
        )
        .await
        .unwrap_err();
    let into_local_side = bed
        .ops
        .r#move(
            bed.root.join("outside.txt").to_str().unwrap(),
            bed.local().join("new.txt").to_str().unwrap(),
            Bed::options(),
        )
        .await
        .unwrap_err();

    for refused in [&from_inside, &into_remote_side, &into_local_side] {
        assert!(matches!(refused, OpError::Busy), "{refused}");
        assert_eq!(refused.exit_code(), 3);
    }
    assert!(bed.root.join("outside.txt").exists());
    assert!(!bed.remote().join("new.txt").exists());
    assert!(!bed.local().join("new.txt").exists());
    assert!(!bed.ops.paths.transfers_dir().exists());
}

#[tokio::test]
async fn a_move_that_fails_leaves_the_source_where_it_was_and_says_why() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("keep/a.txt"), "a");
    write(&bed.local().join("keep/sub/b.txt"), "b");
    write(&bed.remote().join("blocker"), "a file, not a folder");
    let source = bed.local().join("keep");

    let moved = bed
        .ops
        .r#move(
            source.to_str().unwrap(),
            &bed.remote_at("blocker/keep"),
            Bed::options(),
        )
        .await
        .unwrap_err();

    assert_eq!(moved.exit_code(), 1);
    assert!(moved.to_string().contains("rclone"), "{moved}");
    assert_eq!(read(&source.join("a.txt")), "a");
    assert_eq!(read(&source.join("sub/b.txt")), "b");
    assert_eq!(read(&bed.remote().join("blocker")), "a file, not a folder");
}

#[tokio::test]
async fn a_dry_run_reports_files_and_bytes_and_changes_nothing_on_either_side() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("docs/a.txt"), "aaa");
    write(&bed.local().join("docs/sub/b.txt"), "bb");
    let source = bed.local().join("docs");
    let options = TransferOptions {
        dry_run: true,
        wait: WAIT,
    };

    let planned = bed
        .ops
        .r#move(source.to_str().unwrap(), &bed.remote_at("docs"), options)
        .await
        .unwrap();
    let refused = bed
        .ops
        .copy(source.to_str().unwrap(), source.to_str().unwrap(), options)
        .await
        .unwrap_err();

    assert!(planned.finished);
    assert_eq!(planned.transfer.status, "dry_run");
    assert_eq!(planned.transfer.id, None);
    assert_eq!((planned.transfer.files, planned.transfer.bytes), (2, 5));
    assert_eq!(refused.exit_code(), 2);
    assert_eq!(read(&source.join("a.txt")), "aaa");
    assert!(!bed.remote().join("docs").exists());
    assert!(!bed.ops.paths.transfers_dir().exists());
}

#[tokio::test]
async fn a_transfer_that_outlasts_the_wait_is_read_later_by_its_id() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("big/a.txt"), "a");
    let source = bed.local().join("big");
    let options = TransferOptions {
        dry_run: false,
        wait: Duration::ZERO,
    };

    let started = bed
        .ops
        .copy(source.to_str().unwrap(), &bed.remote_at("big"), options)
        .await
        .unwrap();
    let id = started.transfer.id.clone().unwrap();
    let later = bed.ops.transfer_status(&id, WAIT).await.unwrap();
    let unknown = bed
        .ops
        .transfer_status("19990101-000000-000", WAIT)
        .await
        .unwrap_err();
    let traversal = bed
        .ops
        .transfer_status("../config", WAIT)
        .await
        .unwrap_err();

    assert!(matches!(started.transfer.status, "running" | "done"));
    assert!(later.finished);
    assert_eq!(later.transfer.status, "done");
    assert_eq!(later.transfer.id.as_deref(), Some(id.as_str()));
    assert_eq!(read(&bed.remote().join("big/a.txt")), "a");
    assert_eq!(unknown.exit_code(), 2);
    assert_eq!(traversal.exit_code(), 2);
}
