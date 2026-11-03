//! `syncfast apply` against a tempdir: units, state cleanup and the
//! `systemctl` calls (recorded by a fake; the real one is never run).

#![allow(clippy::unwrap_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use common::Bed;
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
    bed.write_config_text("version = 2\n");

    let report = apply_with(&bed, SystemdMode::Off, &systemctl, &["docs"]).await;

    assert_eq!(report.exit_code(), 2);
    assert_eq!(
        report.config_error.as_deref(),
        Some("unsupported config version 2")
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
