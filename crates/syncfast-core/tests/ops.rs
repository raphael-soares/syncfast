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
use syncfast_core::ops::{ExploreOptions, Keep, NewSync, OpError, Ops};
use syncfast_core::paths::Paths;
use syncfast_core::rclone::Rclone;
use syncfast_core::run::{RunEnv, run};
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
