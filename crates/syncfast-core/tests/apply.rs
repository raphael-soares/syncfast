//! `syncfast apply` against a tempdir: units, state cleanup and the
//! `systemctl` calls (recorded by a fake; the real one is never run).

#![allow(clippy::unwrap_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use common::Bed;
use syncfast_core::backup::key::Key;
use syncfast_core::backup::keystore::{KeyStore, MemoryKeyStore};
use syncfast_core::backup::state::BackupState;
use syncfast_core::network::BoxFuture;
use syncfast_core::state::{GateKind, State, try_lock_pair};
use syncfast_core::units::{ApplyEnv, ApplyReport, Systemctl, SystemdMode, apply};

const BIN: &str = "/opt/syncfast bin/syncfast";

#[derive(Default)]
struct FakeSystemctl {
    calls: Mutex<Vec<String>>,
    failing: Option<&'static str>,
}

impl Systemctl for FakeSystemctl {
    fn run<'a>(&'a self, args: &'a [&'a str]) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let line = args.join(" ");
            self.calls.lock().unwrap().push(line.clone());
            if self.failing == Some(line.as_str()) {
                Err("Failed to enable unit: boom".to_owned())
            } else {
                Ok(())
            }
        })
    }
}

impl FakeSystemctl {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

async fn apply_with(
    bed: &Bed,
    mode: SystemdMode,
    systemctl: &FakeSystemctl,
    delete_versions: &[&str],
) -> ApplyReport {
    let bin = PathBuf::from(BIN);
    let env = ApplyEnv::new(&bed.paths, &bed.rclone, &bin, mode, systemctl);
    let delete: Vec<String> = delete_versions.iter().map(|id| (*id).to_owned()).collect();
    apply(&env, &delete).await
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// State, filters, bisync listings and versions of a sync that once ran.
fn leave_traces(bed: &Bed, id: &str) {
    let paths = &bed.paths;
    for (path, text) in [
        (paths.state_file(id), "{}"),
        (paths.filters_file(id), "- a\n"),
        (paths.bisync_workdir(id).join("x.lst"), "x"),
        (paths.versions_dir(id).join("20260101-000000/a.txt"), "old"),
    ] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let mut md5 = paths.filters_file(id).into_os_string();
    md5.push(".md5");
    fs::write(md5, "hash").unwrap();
}

fn traces_left(bed: &Bed, id: &str) -> Vec<&'static str> {
    let paths = &bed.paths;
    let mut md5 = paths.filters_file(id).into_os_string();
    md5.push(".md5");
    [
        ("state", paths.pair_state_dir(id)),
        ("filters", paths.filters_file(id)),
        ("md5", PathBuf::from(md5)),
        ("workdir", paths.bisync_workdir(id)),
        ("versions", paths.versions_dir(id)),
    ]
    .into_iter()
    .filter(|(_, path)| path.exists())
    .map(|(name, _)| name)
    .collect()
}

#[tokio::test]
async fn off_mode_writes_the_units_to_the_preview_folder_and_never_runs_systemctl() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let systemctl = FakeSystemctl::default();

    let report = apply_with(&bed, SystemdMode::Off, &systemctl, &[]).await;

    assert!(report.is_empty(), "{report}");
    let preview = bed.paths.units_preview_dir();
    assert_eq!(
        names(&preview),
        [
            "syncfast-docs.service",
            "syncfast-docs.timer",
            "syncfast-watch.service"
        ]
    );
    let service = fs::read_to_string(preview.join("syncfast-docs.service")).unwrap();
    let rclone = bed.rclone.bin().canonicalize().unwrap();
    assert!(service.contains(&format!("ExecStart=\"{BIN}\" run docs\n")));
    assert!(service.contains(&format!(
        "Environment=\"SYNCFAST_RCLONE={}\"\n",
        rclone.display()
    )));
    let timer = fs::read_to_string(preview.join("syncfast-docs.timer")).unwrap();
    assert!(timer.contains("OnUnitInactiveSec=300s\n"));
    assert!(systemctl.take().is_empty());
    assert!(!bed.paths.systemd_user_dir().exists());
}

#[tokio::test]
async fn user_mode_reloads_enables_the_timer_and_restarts_the_watcher_in_order() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let systemctl = FakeSystemctl::default();

    let report = apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;

    assert!(report.is_empty(), "{report}");
    assert_eq!(
        systemctl.take(),
        [
            "daemon-reload",
            "enable --now syncfast-docs.timer",
            "enable syncfast-watch.service",
            "restart syncfast-watch.service",
        ]
    );
    assert_eq!(
        names(&bed.paths.systemd_user_dir()),
        [
            "syncfast-docs.service",
            "syncfast-docs.timer",
            "syncfast-watch.service"
        ]
    );
}

#[tokio::test]
async fn second_apply_with_nothing_changed_skips_daemon_reload_and_keeps_the_files() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let systemctl = FakeSystemctl::default();
    apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;
    systemctl.take();
    let timer = bed.paths.systemd_user_dir().join("syncfast-docs.timer");
    let modified = fs::metadata(&timer).unwrap().modified().unwrap();

    apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;

    assert_eq!(
        systemctl.take(),
        [
            "enable --now syncfast-docs.timer",
            "enable syncfast-watch.service",
            "restart syncfast-watch.service",
        ]
    );
    assert_eq!(fs::metadata(&timer).unwrap().modified().unwrap(), modified);
}

#[tokio::test]
async fn paused_pair_gets_its_units_and_a_disabled_timer() {
    let bed = Bed::new();
    bed.write_config(&[("docs", false)]);
    let systemctl = FakeSystemctl::default();

    apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;

    assert!(
        systemctl
            .take()
            .contains(&"disable --now syncfast-docs.timer".to_owned())
    );
    assert!(
        bed.paths
            .systemd_user_dir()
            .join("syncfast-docs.service")
            .exists()
    );
}

#[tokio::test]
async fn changed_interval_rewrites_only_the_unit_that_changed_and_reloads() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let systemctl = FakeSystemctl::default();
    apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;
    systemctl.take();
    let dir = bed.paths.systemd_user_dir();
    let service = dir.join("syncfast-docs.service");
    fs::write(
        &service,
        "# Generated by syncfast. Do not edit; run \"syncfast apply\".\nstale\n",
    )
    .unwrap();

    apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;

    assert!(
        fs::read_to_string(&service)
            .unwrap()
            .contains("ExecStart=\"/opt/syncfast bin/syncfast\" run docs")
    );
    assert_eq!(systemctl.take()[0], "daemon-reload");
}

#[tokio::test]
async fn a_failed_reload_is_retried_by_the_next_apply_even_when_no_unit_changed() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let broken = FakeSystemctl {
        failing: Some("daemon-reload"),
        ..FakeSystemctl::default()
    };
    let report = apply_with(&bed, SystemdMode::User, &broken, &[]).await;
    assert!(!report.is_empty());
    assert!(bed.paths.reload_pending_file().exists());
    let systemctl = FakeSystemctl::default();

    let retry = apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;

    assert!(retry.is_empty(), "{retry}");
    assert_eq!(systemctl.take()[0], "daemon-reload");
    assert!(!bed.paths.reload_pending_file().exists());

    apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;

    assert!(!systemctl.take().contains(&"daemon-reload".to_owned()));
}

#[tokio::test]
async fn removed_pair_is_stopped_then_its_units_and_traces_are_deleted_and_versions_kept() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true), ("notes", true)]);
    let systemctl = FakeSystemctl::default();
    apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;
    leave_traces(&bed, "docs");
    leave_traces(&bed, "notes");
    systemctl.take();
    bed.write_config(&[("notes", true)]);

    let report = apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;

    assert!(report.is_empty(), "{report}");
    assert_eq!(
        systemctl.take(),
        [
            "disable --now syncfast-docs.timer",
            "stop syncfast-docs.service",
            "daemon-reload",
            "enable --now syncfast-notes.timer",
            "enable syncfast-watch.service",
            "restart syncfast-watch.service",
        ]
    );
    assert_eq!(traces_left(&bed, "docs"), ["versions"]);
    assert_eq!(
        traces_left(&bed, "notes"),
        ["state", "filters", "md5", "workdir", "versions"]
    );
    assert_eq!(
        names(&bed.paths.systemd_user_dir()),
        [
            "syncfast-notes.service",
            "syncfast-notes.timer",
            "syncfast-watch.service"
        ]
    );
}

#[tokio::test]
async fn off_mode_removal_deletes_state_filters_md5_and_workdir_but_keeps_versions() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let systemctl = FakeSystemctl::default();
    apply_with(&bed, SystemdMode::Off, &systemctl, &[]).await;
    leave_traces(&bed, "docs");
    bed.write_config(&[]);

    let report = apply_with(&bed, SystemdMode::Off, &systemctl, &[]).await;

    assert!(report.is_empty(), "{report}");
    assert_eq!(traces_left(&bed, "docs"), ["versions"]);
    assert_eq!(
        names(&bed.paths.units_preview_dir()),
        ["syncfast-watch.service"]
    );
    assert!(systemctl.take().is_empty());
}

#[tokio::test]
async fn delete_versions_removes_the_versions_of_the_removed_sync_with_it() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let systemctl = FakeSystemctl::default();
    apply_with(&bed, SystemdMode::Off, &systemctl, &[]).await;
    leave_traces(&bed, "docs");
    bed.write_config(&[]);

    let report = apply_with(&bed, SystemdMode::Off, &systemctl, &["docs"]).await;

    assert!(report.is_empty(), "{report}");
    assert!(traces_left(&bed, "docs").is_empty());
}

#[tokio::test]
async fn delete_versions_after_an_earlier_removal_still_deletes_them() {
    let bed = Bed::new();
    bed.write_config(&[]);
    leave_traces(&bed, "docs");
    let systemctl = FakeSystemctl::default();

    let report = apply_with(&bed, SystemdMode::Off, &systemctl, &["docs"]).await;

    assert!(report.is_empty(), "{report}");
    assert!(traces_left(&bed, "docs").is_empty());
}

#[tokio::test]
async fn delete_versions_of_a_sync_still_in_the_config_is_refused() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    leave_traces(&bed, "docs");
    let systemctl = FakeSystemctl::default();

    let report = apply_with(&bed, SystemdMode::Off, &systemctl, &["docs"]).await;

    assert_eq!(report.exit_code(), 1);
    assert_eq!(report.failures[0].pair_id.as_deref(), Some("docs"));
    assert_eq!(report.failures[0].command, "delete versions");
    assert!(traces_left(&bed, "docs").contains(&"versions"));
}

#[tokio::test]
async fn delete_versions_with_a_path_instead_of_an_id_is_refused() {
    let bed = Bed::new();
    bed.write_config(&[]);
    let outside = bed.root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    let systemctl = FakeSystemctl::default();

    let report = apply_with(&bed, SystemdMode::Off, &systemctl, &["../../outside"]).await;

    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.failures[0].detail, "not a valid sync id");
    assert!(outside.exists());
}

#[tokio::test]
async fn invalid_pair_loses_its_units_keeps_its_state_and_gets_the_config_error_gate() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let systemctl = FakeSystemctl::default();
    apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;
    leave_traces(&bed, "docs");
    fs::remove_file(bed.paths.state_file("docs")).unwrap();
    systemctl.take();
    bed.write_config_text(
        "version = 1\n\n[[pair]]\nid = \"docs\"\nname = \"Docs\"\nlocal = \"relative/dir\"\nremote = \"testremote:x\"\n",
    );

    let report = apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;

    assert_eq!(report.exit_code(), 0);
    assert_eq!(report.invalid_pairs.len(), 1);
    assert_eq!(
        report.invalid_pairs[0].message,
        "local folder must be an absolute path"
    );
    assert_eq!(
        systemctl.take()[..2],
        [
            "disable --now syncfast-docs.timer",
            "stop syncfast-docs.service"
        ]
    );
    assert_eq!(
        names(&bed.paths.systemd_user_dir()),
        ["syncfast-watch.service"]
    );
    assert_eq!(
        traces_left(&bed, "docs"),
        ["state", "filters", "md5", "workdir", "versions"]
    );
    let gate = State::load(&bed.paths, "docs").await.unwrap().gate.unwrap();
    assert_eq!(gate.kind, GateKind::ConfigError);
    assert_eq!(gate.detail, "local folder must be an absolute path");
}

#[tokio::test]
async fn global_config_error_changes_nothing_and_exits_with_two() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let systemctl = FakeSystemctl::default();
    apply_with(&bed, SystemdMode::Off, &systemctl, &[]).await;
    bed.write_config_text("version = 4\n");

    let report = apply_with(&bed, SystemdMode::Off, &systemctl, &["docs"]).await;

    assert_eq!(report.exit_code(), 2);
    assert_eq!(
        report.config_error.as_deref(),
        Some("unsupported config version 4")
    );
    assert_eq!(
        names(&bed.paths.units_preview_dir()),
        [
            "syncfast-docs.service",
            "syncfast-docs.timer",
            "syncfast-watch.service"
        ]
    );
}

#[tokio::test]
async fn removal_waits_for_a_running_round_and_is_left_for_the_next_apply() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let systemctl = FakeSystemctl::default();
    apply_with(&bed, SystemdMode::Off, &systemctl, &[]).await;
    leave_traces(&bed, "docs");
    bed.write_config(&[]);
    let round = try_lock_pair(&bed.paths, "docs").unwrap().unwrap();
    let bin = PathBuf::from(BIN);
    let mut env = ApplyEnv::new(&bed.paths, &bed.rclone, &bin, SystemdMode::Off, &systemctl);
    env.pair_lock_timeout = Duration::from_millis(300);

    let blocked = syncfast_core::units::apply(&env, &[]).await;
    drop(round);
    let freed = syncfast_core::units::apply(&env, &[]).await;

    assert_eq!(blocked.failures.len(), 1);
    assert_eq!(blocked.failures[0].pair_id.as_deref(), Some("docs"));
    assert_eq!(blocked.failures[0].command, "wait for the running sync");
    assert!(
        blocked.failures[0]
            .detail
            .contains("left for the next apply")
    );
    assert!(freed.is_empty(), "{freed}");
    assert_eq!(traces_left(&bed, "docs"), ["versions"]);
    assert_eq!(
        names(&bed.paths.units_preview_dir()),
        ["syncfast-watch.service"]
    );
}

#[tokio::test]
async fn blocked_removal_keeps_the_pairs_state_and_units() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let systemctl = FakeSystemctl::default();
    apply_with(&bed, SystemdMode::Off, &systemctl, &[]).await;
    leave_traces(&bed, "docs");
    bed.write_config(&[]);
    let _round = try_lock_pair(&bed.paths, "docs").unwrap().unwrap();
    let bin = PathBuf::from(BIN);
    let mut env = ApplyEnv::new(&bed.paths, &bed.rclone, &bin, SystemdMode::Off, &systemctl);
    env.pair_lock_timeout = Duration::from_millis(200);

    syncfast_core::units::apply(&env, &[]).await;

    assert_eq!(
        traces_left(&bed, "docs"),
        ["state", "filters", "md5", "workdir", "versions"]
    );
    assert!(
        bed.paths
            .units_preview_dir()
            .join("syncfast-docs.timer")
            .exists()
    );
}

#[tokio::test]
async fn failing_systemctl_command_is_reported_and_apply_carries_on() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let systemctl = FakeSystemctl {
        failing: Some("enable --now syncfast-docs.timer"),
        ..FakeSystemctl::default()
    };

    let report = apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;

    assert_eq!(report.exit_code(), 1);
    assert_eq!(report.failures.len(), 1);
    let failure = &report.failures[0];
    assert_eq!(failure.pair_id.as_deref(), Some("docs"));
    assert_eq!(
        failure.command,
        "systemctl --user enable --now syncfast-docs.timer"
    );
    assert_eq!(failure.detail, "Failed to enable unit: boom");
    assert_eq!(
        systemctl.take().last().unwrap(),
        "restart syncfast-watch.service"
    );
}

#[tokio::test]
async fn units_the_user_wrote_by_hand_are_never_removed() {
    let bed = Bed::new();
    bed.write_config(&[]);
    let dir = bed.paths.systemd_user_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("syncfast-mine.service"),
        "[Service]\nExecStart=/bin/true\n",
    )
    .unwrap();
    let systemctl = FakeSystemctl::default();

    let report = apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;

    assert!(report.is_empty(), "{report}");
    assert!(dir.join("syncfast-mine.service").exists());
    assert!(
        !systemctl
            .take()
            .iter()
            .any(|call| call.contains("syncfast-mine"))
    );
}

#[tokio::test]
async fn rclone_that_cannot_list_accounts_changes_nothing() {
    let mut bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let systemctl = FakeSystemctl::default();
    apply_with(&bed, SystemdMode::Off, &systemctl, &[]).await;
    bed.rclone = syncfast_core::rclone::Rclone::new(common::script(
        &bed.root,
        "broken-rclone",
        "echo 'cannot read config' >&2; exit 1",
    ));

    let report = apply_with(&bed, SystemdMode::Off, &systemctl, &[]).await;

    assert_eq!(report.exit_code(), 1);
    assert_eq!(report.failures[0].command, "rclone listremotes");
    assert_eq!(
        names(&bed.paths.units_preview_dir()),
        [
            "syncfast-docs.service",
            "syncfast-docs.timer",
            "syncfast-watch.service"
        ]
    );
}

#[tokio::test]
async fn apply_waits_for_another_apply_to_finish() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let systemctl = FakeSystemctl::default();
    fs::create_dir_all(bed.paths.runtime_subdir()).unwrap();
    let held = fs::File::create(bed.paths.apply_lock_file()).unwrap();
    held.lock().unwrap();

    let waiting = tokio::time::timeout(
        Duration::from_millis(300),
        apply_with(&bed, SystemdMode::Off, &systemctl, &[]),
    )
    .await;
    let written_while_held = bed.paths.units_preview_dir().exists();
    held.unlock().unwrap();
    let report = apply_with(&bed, SystemdMode::Off, &systemctl, &[]).await;

    assert!(waiting.is_err());
    assert!(!written_while_held);
    assert!(report.is_empty(), "{report}");
    assert!(
        bed.paths
            .units_preview_dir()
            .join("syncfast-docs.timer")
            .exists()
    );
}

#[tokio::test]
async fn delete_versions_of_the_reserved_watch_id_is_refused() {
    let bed = Bed::new();
    bed.write_config(&[]);
    leave_traces(&bed, "watch");
    let systemctl = FakeSystemctl::default();

    let report = apply_with(&bed, SystemdMode::Off, &systemctl, &["watch"]).await;

    assert_eq!(report.exit_code(), 1);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(
        report.failures[0].detail,
        "\"watch\" is reserved for the watcher"
    );
    assert_eq!(
        traces_left(&bed, "watch"),
        ["state", "filters", "md5", "workdir", "versions"]
    );
}

fn pair_text(bed: &Bed, id: &str, extra: &str) -> String {
    fs::create_dir_all(bed.local(id)).unwrap();
    format!(
        "\n[[pair]]\nid = \"{id}\"\nname = \"{id}\"\nlocal = \"{}\"\nremote = \"testremote:{}\"\n{extra}",
        bed.local(id).display(),
        bed.remote(id).display(),
    )
}

fn timer_interval(bed: &Bed, id: &str) -> String {
    let text = fs::read_to_string(
        bed.paths
            .units_preview_dir()
            .join(format!("syncfast-{id}.timer")),
    )
    .unwrap();
    text.lines()
        .find_map(|line| line.strip_prefix("OnUnitInactiveSec="))
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn each_timer_uses_the_interval_its_sync_resolves() {
    let bed = Bed::new();
    let text = format!(
        "version = 2\n\n[defaults]\ninterval = \"10m\"\n{}{}",
        pair_text(&bed, "inherits", ""),
        pair_text(&bed, "own", "interval = \"2h\"\n"),
    );
    bed.write_config_text(&text);

    let report = apply_with(&bed, SystemdMode::Off, &FakeSystemctl::default(), &[]).await;

    assert!(report.is_empty(), "{report}");
    assert_eq!(timer_interval(&bed, "inherits"), "600s");
    assert_eq!(timer_interval(&bed, "own"), "7200s");
}

fn user_timer(bed: &Bed, id: &str) -> String {
    fs::read_to_string(
        bed.paths
            .systemd_user_dir()
            .join(format!("syncfast-{id}.timer")),
    )
    .unwrap()
}

#[tokio::test]
async fn a_new_interval_reaches_only_that_timer_with_a_reload_and_no_restart() {
    let bed = Bed::new();
    let config = |docs: &str| {
        format!(
            "version = 2\n{}{}",
            pair_text(&bed, "docs", docs),
            pair_text(&bed, "notes", "")
        )
    };
    bed.write_config_text(&config(""));
    let systemctl = FakeSystemctl::default();
    apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;
    systemctl.take();
    let notes_before = user_timer(&bed, "notes");
    bed.write_config_text(&config("interval = \"30m\"\n"));

    let report = apply_with(&bed, SystemdMode::User, &systemctl, &[]).await;

    assert!(report.is_empty(), "{report}");
    assert!(user_timer(&bed, "docs").contains("OnUnitInactiveSec=1800s"));
    assert_eq!(user_timer(&bed, "notes"), notes_before);
    let calls = systemctl.take();
    assert_eq!(calls[0], "daemon-reload");
    assert!(
        calls
            .iter()
            .all(|call| !(call.starts_with("restart") && call.contains(".timer"))),
        "{calls:?}"
    );
}

#[tokio::test]
async fn changing_the_interval_in_preview_mode_rewrites_the_preview_timer() {
    let bed = Bed::new();
    bed.write_config_text(&format!("version = 2\n{}", pair_text(&bed, "docs", "")));
    apply_with(&bed, SystemdMode::Off, &FakeSystemctl::default(), &[]).await;
    assert_eq!(timer_interval(&bed, "docs"), "300s");
    bed.write_config_text(&format!(
        "version = 2\n{}",
        pair_text(&bed, "docs", "interval = \"1h\"\n")
    ));

    apply_with(&bed, SystemdMode::Off, &FakeSystemctl::default(), &[]).await;

    assert_eq!(timer_interval(&bed, "docs"), "3600s");
}

#[tokio::test]
async fn apply_migrates_a_v1_config_and_keeps_a_copy_of_the_old_file() {
    let bed = Bed::new();
    bed.write_config(&[("docs", true)]);
    let old = fs::read_to_string(bed.paths.config_file()).unwrap();

    let report = apply_with(&bed, SystemdMode::Off, &FakeSystemctl::default(), &[]).await;

    assert!(report.is_empty(), "{report}");
    let migrated = fs::read_to_string(bed.paths.config_file()).unwrap();
    assert!(migrated.starts_with("version = 2\n"), "{migrated}");
    assert!(migrated.contains("id = \"docs\""));
    let backup = bed.paths.config_file().with_file_name("config.toml.v1");
    assert_eq!(fs::read_to_string(backup).unwrap(), old);
    assert_eq!(timer_interval(&bed, "docs"), "300s");
}

fn backup_text(bed: &Bed, id: &str, extra: &str) -> String {
    format!(
        "[[backup]]\nid = \"{id}\"\nname = \"Documents\"\npaths = [\"{data}\"]\ndest = \"{repo}\"\n{extra}",
        data = bed.root.join(format!("data-{id}")).display(),
        repo = bed.root.join(format!("repo-{id}")).display(),
    )
}

fn write_backups(bed: &Bed, backups: &[String]) {
    bed.write_config_text(&format!("version = 3\n{}", backups.join("\n")));
}

fn fake_restic(bed: &Bed) -> PathBuf {
    let restic = bed.root.join("restic");
    fs::write(&restic, "").unwrap();
    restic
}

async fn apply_backups(bed: &Bed, mode: SystemdMode, systemctl: &FakeSystemctl) -> ApplyReport {
    let bin = PathBuf::from(BIN);
    let env = ApplyEnv::new(&bed.paths, &bed.rclone, &bin, mode, systemctl)
        .with_restic(Some(fake_restic(bed)));
    apply(&env, &[]).await
}

fn preview_unit(bed: &Bed, name: &str) -> String {
    fs::read_to_string(bed.paths.units_preview_dir().join(name)).unwrap()
}

fn position(calls: &[String], call: &str) -> usize {
    assert!(
        calls.iter().any(|c| c == call),
        "{call} is not in {calls:?}"
    );
    calls.iter().position(|c| c == call).unwrap()
}

#[tokio::test]
async fn off_mode_creates_the_units_of_a_backup_in_the_preview_folder() {
    let bed = Bed::new();
    write_backups(&bed, &[backup_text(&bed, "docs", "")]);
    let systemctl = FakeSystemctl::default();

    let report = apply_backups(&bed, SystemdMode::Off, &systemctl).await;

    assert!(report.is_empty(), "{report}");
    assert_eq!(
        names(&bed.paths.units_preview_dir()),
        [
            "syncfast-backup_docs.service",
            "syncfast-backup_docs.timer",
            "syncfast-watch.service"
        ]
    );
    let service = preview_unit(&bed, "syncfast-backup_docs.service");
    let rclone = bed.rclone.bin().canonicalize().unwrap();
    assert!(service.contains(&format!("ExecStart=\"{BIN}\" backup-run docs\n")));
    assert!(service.contains(&format!(
        "Environment=\"SYNCFAST_RCLONE={}\"\nEnvironment=\"SYNCFAST_RESTIC={}\"\n",
        rclone.display(),
        bed.root.join("restic").display()
    )));
    let timer = preview_unit(&bed, "syncfast-backup_docs.timer");
    assert!(timer.contains("OnCalendar=*-*-* 03:00:00\n"));
    assert!(timer.contains("Unit=syncfast-backup_docs.service\n"));
    assert!(systemctl.take().is_empty());
}

#[tokio::test]
async fn an_unfound_restic_leaves_its_environment_line_out() {
    let bed = Bed::new();
    write_backups(&bed, &[backup_text(&bed, "docs", "")]);
    let bin = PathBuf::from(BIN);
    let systemctl = FakeSystemctl::default();
    let env = ApplyEnv::new(&bed.paths, &bed.rclone, &bin, SystemdMode::Off, &systemctl)
        .with_restic(None);

    let report = apply(&env, &[]).await;

    assert!(report.is_empty(), "{report}");
    let service = preview_unit(&bed, "syncfast-backup_docs.service");
    assert!(service.contains("SYNCFAST_RCLONE="));
    assert!(!service.contains("SYNCFAST_RESTIC"));
}

#[tokio::test]
async fn changing_the_schedule_of_a_backup_rewrites_only_its_timer() {
    let bed = Bed::new();
    write_backups(&bed, &[backup_text(&bed, "docs", "")]);
    let systemctl = FakeSystemctl::default();
    apply_backups(&bed, SystemdMode::Off, &systemctl).await;
    let service = preview_unit(&bed, "syncfast-backup_docs.service");
    write_backups(&bed, &[backup_text(&bed, "docs", "every = \"weekly\"\n")]);

    let report = apply_backups(&bed, SystemdMode::Off, &systemctl).await;

    assert!(report.is_empty(), "{report}");
    let timer = preview_unit(&bed, "syncfast-backup_docs.timer");
    assert!(timer.contains("OnCalendar=Mon *-*-* 03:00:00\n"), "{timer}");
    assert_eq!(preview_unit(&bed, "syncfast-backup_docs.service"), service);
}

#[tokio::test]
async fn user_mode_reloads_then_enables_the_timer_of_an_enabled_backup() {
    let bed = Bed::new();
    write_backups(&bed, &[backup_text(&bed, "docs", "")]);
    let systemctl = FakeSystemctl::default();

    let report = apply_backups(&bed, SystemdMode::User, &systemctl).await;

    assert!(report.is_empty(), "{report}");
    let calls = systemctl.take();
    assert!(
        position(&calls, "daemon-reload")
            < position(&calls, "enable --now syncfast-backup_docs.timer")
    );
    assert!(
        position(&calls, "enable --now syncfast-backup_docs.timer")
            < position(&calls, "restart syncfast-watch.service")
    );
    assert!(
        bed.paths
            .systemd_user_dir()
            .join("syncfast-backup_docs.timer")
            .exists()
    );
}

#[tokio::test]
async fn user_mode_disables_the_timer_of_a_paused_backup() {
    let bed = Bed::new();
    write_backups(&bed, &[backup_text(&bed, "docs", "enabled = false\n")]);
    let systemctl = FakeSystemctl::default();

    let report = apply_backups(&bed, SystemdMode::User, &systemctl).await;

    assert!(report.is_empty(), "{report}");
    let calls = systemctl.take();
    assert!(calls.contains(&"disable --now syncfast-backup_docs.timer".to_owned()));
    assert!(
        !calls
            .iter()
            .any(|call| call.starts_with("enable --now syncfast-backup_"))
    );
    assert!(
        bed.paths
            .systemd_user_dir()
            .join("syncfast-backup_docs.service")
            .exists()
    );
}

#[tokio::test]
async fn a_backup_that_left_the_config_loses_units_and_state_but_not_the_repository_or_the_key() {
    let bed = Bed::new();
    write_backups(
        &bed,
        &[
            backup_text(&bed, "docs", ""),
            backup_text(&bed, "photos", ""),
        ],
    );
    let systemctl = FakeSystemctl::default();
    apply_backups(&bed, SystemdMode::Off, &systemctl).await;
    let repo = bed.root.join("repo-docs");
    fs::create_dir_all(&repo).unwrap();
    fs::write(repo.join("config"), "repository").unwrap();
    let keys = MemoryKeyStore::default();
    let key = Key::generate().unwrap();
    keys.put("docs", "Documents", &key).await.unwrap();
    BackupState::default()
        .save(&bed.paths, "docs")
        .await
        .unwrap();
    BackupState::default()
        .save(&bed.paths, "photos")
        .await
        .unwrap();
    write_backups(&bed, &[backup_text(&bed, "photos", "")]);

    let report = apply_backups(&bed, SystemdMode::Off, &systemctl).await;

    assert!(report.is_empty(), "{report}");
    assert_eq!(
        names(&bed.paths.units_preview_dir()),
        [
            "syncfast-backup_photos.service",
            "syncfast-backup_photos.timer",
            "syncfast-watch.service"
        ]
    );
    assert!(!bed.paths.backup_state_dir("docs").exists());
    assert!(bed.paths.backup_state_dir("photos").exists());
    assert_eq!(
        fs::read_to_string(repo.join("config")).unwrap(),
        "repository"
    );
    assert_eq!(keys.get("docs").await.unwrap(), Some(key));
}

#[tokio::test]
async fn user_mode_stops_the_timer_and_service_of_a_removed_backup_before_deleting_it() {
    let bed = Bed::new();
    write_backups(&bed, &[backup_text(&bed, "docs", "")]);
    let systemctl = FakeSystemctl::default();
    apply_backups(&bed, SystemdMode::User, &systemctl).await;
    systemctl.take();
    write_backups(&bed, &[]);

    let report = apply_backups(&bed, SystemdMode::User, &systemctl).await;

    assert!(report.is_empty(), "{report}");
    let calls = systemctl.take();
    assert!(
        position(&calls, "disable --now syncfast-backup_docs.timer")
            < position(&calls, "stop syncfast-backup_docs.service")
    );
    assert!(
        position(&calls, "stop syncfast-backup_docs.service") < position(&calls, "daemon-reload")
    );
    assert_eq!(
        names(&bed.paths.systemd_user_dir()),
        ["syncfast-watch.service"]
    );
}

#[tokio::test]
async fn an_invalid_backup_gets_no_units_keeps_its_state_and_gets_a_config_error_gate() {
    let bed = Bed::new();
    write_backups(&bed, &[backup_text(&bed, "docs", "")]);
    let systemctl = FakeSystemctl::default();
    apply_backups(&bed, SystemdMode::Off, &systemctl).await;
    BackupState::default()
        .save(&bed.paths, "docs")
        .await
        .unwrap();
    write_backups(&bed, &[backup_text(&bed, "docs", "every = \"monthly\"\n")]);

    let report = apply_backups(&bed, SystemdMode::Off, &systemctl).await;

    assert_eq!(report.invalid_backups.len(), 1);
    assert_eq!(report.invalid_backups[0].backup_id, "docs");
    assert_eq!(report.exit_code(), 0);
    assert_eq!(
        names(&bed.paths.units_preview_dir()),
        ["syncfast-watch.service"]
    );
    let state = BackupState::load(&bed.paths, "docs").await.unwrap();
    let gate = state.gate.unwrap();
    assert_eq!(gate.kind, GateKind::ConfigError);
    assert_eq!(gate.detail, report.invalid_backups[0].message);
}

#[tokio::test]
async fn an_invalid_backup_that_never_had_units_still_gets_the_gate() {
    let bed = Bed::new();
    write_backups(&bed, &[backup_text(&bed, "docs", "every = \"monthly\"\n")]);

    let report = apply_backups(&bed, SystemdMode::Off, &FakeSystemctl::default()).await;

    assert_eq!(report.invalid_backups.len(), 1);
    let state = BackupState::load(&bed.paths, "docs").await.unwrap();
    assert_eq!(state.gate.unwrap().kind, GateKind::ConfigError);
}

#[tokio::test]
async fn a_config_error_gate_ends_when_the_backup_is_valid_again() {
    let bed = Bed::new();
    write_backups(&bed, &[backup_text(&bed, "docs", "every = \"monthly\"\n")]);
    let systemctl = FakeSystemctl::default();
    apply_backups(&bed, SystemdMode::Off, &systemctl).await;
    write_backups(&bed, &[backup_text(&bed, "docs", "")]);

    let report = apply_backups(&bed, SystemdMode::Off, &systemctl).await;

    assert!(report.is_empty(), "{report}");
    let state = BackupState::load(&bed.paths, "docs").await.unwrap();
    assert_eq!(state.gate, None);
    assert_eq!(
        names(&bed.paths.units_preview_dir()),
        [
            "syncfast-backup_docs.service",
            "syncfast-backup_docs.timer",
            "syncfast-watch.service"
        ]
    );
}

#[tokio::test]
async fn syncs_and_backups_are_reconciled_side_by_side() {
    let bed = Bed::new();
    bed.write_config_text(&format!(
        "version = 3\n{}\n{}",
        pair_text(&bed, "docs", ""),
        backup_text(&bed, "docs", "")
    ));

    let report = apply_backups(&bed, SystemdMode::Off, &FakeSystemctl::default()).await;

    assert!(report.is_empty(), "{report}");
    assert_eq!(
        names(&bed.paths.units_preview_dir()),
        [
            "syncfast-backup_docs.service",
            "syncfast-backup_docs.timer",
            "syncfast-docs.service",
            "syncfast-docs.timer",
            "syncfast-watch.service"
        ]
    );
}
