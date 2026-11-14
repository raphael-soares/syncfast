//! The text the commands print when `--json` is off.

use std::fmt::Write as _;

use jiff::Timestamp;
use syncfast_core::backup::restore_job::RestoreReport;
use syncfast_core::backup::snapshots::SnapshotView;
use syncfast_core::bisync_log::ChangeKind;
use syncfast_core::ops::{
    AccountView, AddedSync, BackupCreated, BackupDetail, BackupRun, BackupSaved, BackupView,
    BackupsToggled, ConfirmedReview, ConflictView, Exploration, FilterTestView, FiltersSaved,
    FolderList, FolderMade, PairDetail, PairView, RemovedSync, Resolved, Restored, ReviewView,
    RoundLog, RoundView, SettingChanged, SettingView, SettingsView, SnapshotFiles, SyncRounds,
    ThemeInfo, ThemeSet, Toggled, TransferReport,
};
use syncfast_core::recently_deleted::{DeletedKind, DeletedList, DeletedSide};

fn day(at: Timestamp) -> String {
    at.strftime("%Y-%m-%d").to_string()
}

fn when(at: Option<Timestamp>) -> String {
    at.map_or_else(|| "never".to_owned(), |at| at.to_string())
}

#[allow(clippy::cast_precision_loss)]
pub fn bytes(count: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = count as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{count} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn warnings(out: &mut String, found: &[String]) {
    for warning in found {
        let _ = writeln!(out, "warning: {warning}");
    }
}

fn one_line(sync: &PairView) -> String {
    let mut line = format!(
        "{}\t{}\t{}",
        sync.id,
        sync.status_text,
        sync.last_success
            .map_or_else(|| "never synced".to_owned(), |at| format!("synced {at}"))
    );
    if let Some(gate) = &sync.gate {
        let _ = write!(line, "\t{}", gate.explanation);
    }
    line
}

fn sync_block(out: &mut String, sync: &PairView) {
    let _ = writeln!(out, "{} ({})", sync.name, sync.id);
    let _ = writeln!(
        out,
        "  folders:  {} <-> {}",
        sync.local.display(),
        sync.remote
    );
    let _ = writeln!(
        out,
        "  status:   {}{}",
        sync.status_text,
        if sync.enabled { "" } else { " (paused)" }
    );
    if let Some(gate) = &sync.gate {
        let _ = writeln!(
            out,
            "  gate:     {}: {}",
            gate.kind.as_str(),
            gate.explanation
        );
    }
    if let Some(error) = &sync.config_error {
        let _ = writeln!(out, "  config:   {error}");
    }
    if let Some(notice) = sync.notice {
        let _ = writeln!(out, "  notice:   {notice}");
    }
    if !sync.lowered_safety.is_empty() {
        let _ = writeln!(out, "  safety lowered: {}", sync.lowered_safety.join(", "));
    }
    let _ = writeln!(out, "  presets:  {}", sync.presets.join(", "));
    for rule in &sync.filters {
        let _ = writeln!(out, "  rule:     {rule}");
    }
}

pub fn show(detail: &PairDetail) -> String {
    let mut out = String::new();
    sync_block(&mut out, &detail.sync);
    let _ = writeln!(out, "  last sync: {}", when(detail.sync.last_success));
    let _ = writeln!(out, "  filters hash: {}", detail.filters_hash);
    let _ = writeln!(out, "  versions: {}", detail.versions_dir.display());
    if let Some(run) = &detail.last_run {
        let count = |value: Option<u32>| value.map_or_else(|| "?".to_owned(), |n| n.to_string());
        let _ = writeln!(
            out,
            "  last round: {} up {} down {} deleted here {} deleted there {} renamed {} conflicts {} (exit {})",
            run.finished,
            count(run.uploaded),
            count(run.downloaded),
            count(run.deleted_local),
            count(run.deleted_remote),
            count(run.renamed),
            count(run.conflicts_new),
            run.exit_code
        );
    }
    out
}

pub fn added(added: &AddedSync) -> String {
    let mut out = format!("Added \"{}\" ({}).\n", added.sync.name, added.sync.id);
    warnings(&mut out, &added.warnings);
    let _ = writeln!(out, "{}", added.next_step);
    out
}

pub fn removed(removed: &RemovedSync) -> String {
    let mut out = format!("Removed \"{}\".", removed.id);
    if removed.versions_deleted {
        out.push_str(" Saved versions deleted.");
    }
    out.push('\n');
    warnings(&mut out, &removed.warnings);
    out
}

pub fn toggled(toggled: &Toggled) -> String {
    let mut out = String::new();
    for sync in &toggled.syncs {
        let _ = writeln!(out, "{}", one_line(sync));
    }
    warnings(&mut out, &toggled.warnings);
    out
}

pub fn filters_saved(saved: &FiltersSaved) -> String {
    let mut out = format!("{}\nfilters hash: {}\n", saved.message, saved.filters_hash);
    warnings(&mut out, &saved.warnings);
    out
}

pub fn filter_test(test: &FilterTestView) -> String {
    let mut out = format!("{}\n", test.summary);
    for folder in &test.folders {
        let _ = writeln!(
            out,
            "  {} \u{b7} {} files",
            folder.folder, folder.excluded_files
        );
    }
    if !test.git_repos.is_empty() {
        out.push_str("Git repositories left out:\n");
        for repo in &test.git_repos {
            let _ = writeln!(out, "  {repo}/");
        }
    }
    out
}

pub fn accounts(accounts: &[AccountView]) -> String {
    let mut out = String::new();
    for account in accounts {
        let _ = write!(out, "{}\t{}", account.name, account.kind);
        if let Some(warning) = account.warning {
            let _ = write!(out, "\t{warning}");
        }
        out.push('\n');
    }
    if accounts.is_empty() {
        out.push_str("No accounts. Add one in syncfast (Settings > Accounts).\n");
    }
    out
}

pub fn folders(list: &FolderList) -> String {
    let mut out = String::new();
    for folder in &list.folders {
        let _ = writeln!(out, "{folder}/");
    }
    if list.folders.is_empty() {
        let _ = writeln!(out, "No folders in {}.", list.path);
    }
    out
}

pub fn folder_made(made: &FolderMade) -> String {
    format!("Created {}\n", made.path)
}

pub fn explore(found: &Exploration) -> String {
    let mut out = format!(
        "{} ({:?}): {} files, {} folders, {}",
        found.target,
        found.location,
        found.files,
        found.folders,
        bytes(found.bytes)
    );
    if let Some(newest) = found.newest {
        let _ = write!(out, ", newest {}", day(newest));
    }
    out.push('\n');
    if found.files_of_unknown_size > 0 {
        let _ = writeln!(
            out,
            "{} files report no size (Google Docs) and count as 0 B.",
            found.files_of_unknown_size
        );
    }
    for stat in &found.tree {
        let name = if stat.path == "." {
            ".".to_owned()
        } else {
            format!("{}/", stat.path.rsplit('/').next().unwrap_or(&stat.path))
        };
        let extensions: Vec<String> = stat
            .top_extensions
            .iter()
            .map(|ext| format!(".{}({})", ext.extension.trim_matches(['(', ')']), ext.files))
            .collect();
        let _ = writeln!(
            out,
            "{}{name}  {} files, {}{}  {}",
            "  ".repeat(stat.depth),
            stat.files,
            bytes(stat.bytes),
            stat.newest
                .map_or_else(String::new, |newest| format!(", newest {}", day(newest))),
            extensions.join(" ")
        );
    }
    if found.folders_omitted > 0 {
        let _ = writeln!(
            out,
            "... {} more folders not shown; explore a subfolder or raise --max-folders",
            found.folders_omitted
        );
    }
    out
}

pub fn transfer(report: &TransferReport) -> String {
    let found = &report.transfer;
    let moving = found.operation == "move";
    let verb = match (found.status, moving) {
        ("dry_run", true) => "Would move",
        ("dry_run", false) => "Would copy",
        ("running", true) => "Moving",
        ("running", false) => "Copying",
        ("done", true) => "Moved",
        ("done", false) => "Copied",
        (_, true) => "Failed to move",
        (_, false) => "Failed to copy",
    };
    let files = if found.files == 1 { "file" } else { "files" };
    let mut out = format!(
        "{verb} {} {} to {}: {} {files}, {}\n",
        found.kind,
        found.source,
        found.destination,
        found.files,
        bytes(found.bytes),
    );
    if let Some(error) = &found.error {
        let _ = writeln!(out, "{error}");
    }
    let _ = writeln!(out, "{}", found.note);
    out
}

pub fn sync_rounds(rounds: &SyncRounds) -> String {
    let mut out = String::new();
    for round in &rounds.rounds {
        let _ = write!(out, "{}", one_line(&round.sync));
        if !round.finished {
            out.push_str("\t(still running; check status later)");
        }
        out.push('\n');
    }
    if rounds.rounds.is_empty() {
        out.push_str("No syncs to run.\n");
    }
    out
}

pub fn review(review: &ReviewView) -> String {
    let counts = &review.counts;
    let mut out = format!(
        "Review for \"{}\": {} ({:?})\n",
        review.sync_id, review.reason, review.kind
    );
    let _ = writeln!(
        out,
        "  upload {} download {} replace here {} replace there {} delete here {} delete there {} rename {} other {}",
        counts.upload,
        counts.download,
        counts.overwrite_local,
        counts.overwrite_remote,
        counts.delete_local,
        counts.delete_remote,
        counts.renamed,
        counts.other
    );
    for change in &review.changes {
        let label = match change.kind {
            ChangeKind::Upload => "upload",
            ChangeKind::Download => "download",
            ChangeKind::OverwriteLocal => "replace here",
            ChangeKind::OverwriteRemote => "replace there",
            ChangeKind::DeleteLocal => "delete here",
            ChangeKind::DeleteRemote => "delete there",
            ChangeKind::RenameLocal => "rename here",
            ChangeKind::RenameRemote => "rename there",
            ChangeKind::Other => "other",
        };
        match &change.from {
            Some(from) => {
                let _ = writeln!(out, "  {label}: {from} -> {}", change.path);
            }
            None => {
                let _ = writeln!(out, "  {label}: {}", change.path);
            }
        }
    }
    if review.changes_omitted > 0 {
        let _ = writeln!(out, "  ... {} more changes", review.changes_omitted);
    }
    if review.returning > 0 {
        let _ = writeln!(
            out,
            "  coming back (deleted or renamed since the last sync): {}",
            review.returning
        );
        for change in &review.returning_changes {
            let _ = writeln!(out, "  - {}", change.path);
        }
    }
    if !review.excluded_repos.is_empty() {
        let _ = writeln!(
            out,
            "  left out as git repositories: {}",
            review.excluded_repos.len()
        );
        for repo in &review.excluded_repos {
            let _ = writeln!(out, "  - {repo}/");
        }
    }
    let _ = writeln!(out, "filters hash: {}", review.filters_hash);
    let _ = writeln!(out, "{}", review.note);
    out
}

pub fn confirmed(confirmed: &ConfirmedReview) -> String {
    let mut out = format!("Confirmed. {}", one_line(&confirmed.sync));
    if !confirmed.finished {
        out.push_str("\t(still running; check status later)");
    }
    out.push('\n');
    out
}

pub fn conflicts(found: &[ConflictView]) -> String {
    let mut out = String::new();
    for conflict in found {
        let current = conflict.current.as_ref().map_or_else(
            || "original gone".to_owned(),
            |file| format!("current {} {}", bytes(file.size), when(file.modified)),
        );
        let _ = writeln!(
            out,
            "{}\t{current}\tconflicted {} {}",
            conflict.path.display(),
            bytes(conflict.conflicted.size),
            when(conflict.conflicted.modified)
        );
    }
    if found.is_empty() {
        out.push_str("No conflicts.\n");
    }
    out
}

pub fn resolved(resolved: &Resolved) -> String {
    let mut out = format!(
        "Settled {} (kept {:?}).{}\n",
        resolved.path.display(),
        resolved.keep,
        if resolved.sync_started {
            " A sync was started."
        } else {
            ""
        }
    );
    if let Some(warning) = &resolved.warning {
        warnings(&mut out, std::slice::from_ref(warning));
    }
    out
}

pub fn deleted(list: &DeletedList) -> String {
    let mut out = String::new();
    for item in &list.items {
        let side = match item.side {
            DeletedSide::Local => "local",
            DeletedSide::Remote => "remote",
            DeletedSide::Drive => "drive",
        };
        let kind = match item.kind {
            DeletedKind::Deleted => "deleted",
            DeletedKind::Replaced => "replaced",
        };
        let _ = writeln!(
            out,
            "{side}\t{}\t{kind}\t{}\t{}",
            item.when
                .map_or_else(|| "-".to_owned(), |at| at.to_string()),
            item.path,
            item.key
        );
    }
    if list.items.is_empty() {
        out.push_str("Nothing deleted recently.\n");
    }
    if let Some(warning) = &list.warning {
        warnings(&mut out, std::slice::from_ref(warning));
    }
    out
}

pub fn restored(restored: &Restored) -> String {
    let mut out = format!(
        "Restored to {}.{}\n",
        restored.path,
        if restored.sync_started {
            " A sync was started."
        } else {
            ""
        }
    );
    if let Some(warning) = &restored.warning {
        warnings(&mut out, std::slice::from_ref(warning));
    }
    out
}

pub fn activity(rounds: &[RoundView]) -> String {
    let mut out = String::new();
    for round in rounds {
        let count = |value: Option<u32>| value.map_or_else(|| "?".to_owned(), |n| n.to_string());
        let _ = write!(
            out,
            "{}\t{}\t{}s\tup {} down {} deleted {}",
            round.round,
            round.status_text,
            round
                .seconds
                .map_or_else(|| "?".to_owned(), |s| s.to_string()),
            count(round.uploaded),
            count(round.downloaded),
            count(
                round
                    .deleted_local
                    .zip(round.deleted_remote)
                    .map(|(a, b)| a + b)
            ),
        );
        if let Some(renamed) = round.renamed.filter(|renamed| *renamed > 0) {
            let _ = write!(out, " renamed {renamed}");
        }
        if round.auto_reviewed.is_some() {
            out.push_str("\treviewed automatically");
        }
        if let Some(detail) = &round.detail {
            let _ = write!(out, "\t{detail}");
        }
        out.push('\n');
    }
    if rounds.is_empty() {
        out.push_str("No activity yet.\n");
    }
    out
}

pub fn round_log(log: &RoundLog) -> String {
    let mut out = String::new();
    for message in &log.messages {
        let _ = writeln!(out, "{message}");
    }
    out
}

pub fn theme_info(info: &ThemeInfo) -> String {
    format!(
        "theme: {}\navailable: {}\n",
        info.current,
        info.available.join(", ")
    )
}

pub fn theme_set(set: &ThemeSet) -> String {
    format!("Theme set to {}.\n", set.theme)
}

fn plain(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn setting_line(setting: &SettingView) -> String {
    let mut line = format!(
        "{}\t{}\t{}",
        setting.key,
        plain(&setting.value),
        setting.origin
    );
    if setting.origin != "builtin" {
        let _ = write!(line, "\t(default {})", plain(&setting.default));
    }
    line
}

pub fn settings(view: &SettingsView) -> String {
    let mut out = format!("settings of {}\n", view.scope);
    for setting in &view.settings {
        let _ = writeln!(out, "{}", setting_line(setting));
    }
    if !view.lowered_safety.is_empty() {
        let _ = writeln!(out, "safety lowered: {}", view.lowered_safety.join(", "));
    }
    out
}

pub fn setting_changed(changed: &SettingChanged) -> String {
    let mut out = format!("{}\n", setting_line(&changed.setting));
    warnings(&mut out, &changed.warnings);
    out
}

fn backup_line(backup: &BackupView) -> String {
    let mut line = format!(
        "{}\t{}\t{}",
        backup.id,
        backup.status_text,
        backup.last_success.map_or_else(
            || "never backed up".to_owned(),
            |at| format!("backed up {at}")
        )
    );
    if let Some(gate) = &backup.gate {
        let _ = write!(line, "\t{}", gate.detail);
    }
    line
}

pub fn backups(found: &[BackupView]) -> String {
    if found.is_empty() {
        return "No backups.\n".to_owned();
    }
    found.iter().fold(String::new(), |mut out, backup| {
        let _ = writeln!(out, "{}", backup_line(backup));
        out
    })
}

fn backup_block(out: &mut String, backup: &BackupView) {
    let _ = writeln!(out, "{} ({})", backup.name, backup.id);
    let folders: Vec<String> = backup
        .paths
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    let _ = writeln!(out, "  folders:  {}", folders.join(", "));
    let _ = writeln!(out, "  to:       {}", backup.dest);
    let _ = writeln!(
        out,
        "  status:   {}{}",
        backup.status_text,
        if backup.enabled { "" } else { " (paused)" }
    );
    let _ = writeln!(out, "  every:    {}", backup.every);
    let _ = writeln!(
        out,
        "  keep:     daily={} weekly={} monthly={} yearly={}",
        backup.keep.daily, backup.keep.weekly, backup.keep.monthly, backup.keep.yearly
    );
    let _ = writeln!(out, "  key:      {}", backup.key);
    if backup.key_unsaved {
        out.push_str("  The user has not saved the key yet: ask them to open syncfast.\n");
    }
    if let Some(snapshot) = &backup.last_snapshot {
        let _ = writeln!(out, "  snapshot: {} at {}", snapshot.id, snapshot.time);
    }
    if let Some(gate) = &backup.gate {
        let _ = writeln!(out, "  gate:     {}: {}", gate.kind.as_str(), gate.detail);
    }
    if let Some(error) = &backup.config_error {
        let _ = writeln!(out, "  config:   {error}");
    }
}

pub fn backup_show(detail: &BackupDetail) -> String {
    let mut out = String::new();
    backup_block(&mut out, &detail.backup);
    if let Some(run) = &detail.last_run {
        let _ = writeln!(
            out,
            "  last run: {} files new, {} changed, {} added; exit {}",
            run.summary.files_new,
            run.summary.files_changed,
            bytes(run.summary.data_added),
            run.exit_code
        );
    }
    if let Some(check) = &detail.last_check {
        let _ = writeln!(
            out,
            "  last check: {} {}: {}",
            check.kind.as_str(),
            if check.ok { "ok" } else { "failed" },
            check.detail
        );
    }
    if let Some(error) = &detail.prune_error {
        let _ = writeln!(out, "  prune error: {error}");
    }
    out
}

pub fn snapshots(found: &[SnapshotView]) -> String {
    if found.is_empty() {
        return "No snapshots yet.\n".to_owned();
    }
    found.iter().fold(String::new(), |mut out, snapshot| {
        let _ = writeln!(
            out,
            "{}\t{}\t{}\t{}",
            snapshot.short_id,
            snapshot.time,
            snapshot.host,
            snapshot
                .files
                .map_or_else(|| "?".to_owned(), |files| format!("{files} files")),
        );
        out
    })
}

pub fn snapshot_files(found: &SnapshotFiles) -> String {
    let mut out = String::new();
    for node in &found.files {
        let _ = writeln!(
            out,
            "{:?}\t{}\t{}\t{}",
            node.kind,
            node.size.map_or_else(String::new, bytes),
            node.mtime.map_or_else(String::new, day),
            node.path
        );
    }
    if found.truncated {
        out.push_str("There are more entries; raise --max-entries or list a folder inside.\n");
    }
    if found.files.is_empty() {
        out.push_str("Nothing here.\n");
    }
    out
}

pub fn backup_run(run: &BackupRun) -> String {
    let mut out = backup_line(&run.backup);
    if !run.finished {
        out.push_str("\t(still running; check backups later)");
    }
    out.push('\n');
    out
}

pub fn backups_toggled(toggled: &BackupsToggled) -> String {
    let mut out = backups(&toggled.backups);
    warnings(&mut out, &toggled.warnings);
    out
}

pub fn backup_created(created: &BackupCreated) -> String {
    let mut out = format!(
        "Created backup {}. Its key is in the key storage.\n",
        created.id
    );
    if created.key_unsaved {
        out.push_str(
            "The key is not saved yet: tell the user to open syncfast and confirm that they saved it.\n",
        );
    }
    warnings(&mut out, &created.warnings);
    out
}

pub fn backup_saved(saved: &BackupSaved) -> String {
    let mut out = format!("Saved {}.\n", saved.backup.id);
    warnings(&mut out, &saved.warnings);
    out
}

pub fn restore_report(report: &RestoreReport) -> String {
    let verb = match report.status {
        "dry_run" => "Would restore",
        "running" => "Restoring",
        _ => "Restored",
    };
    let (files, size) = if report.status == "dry_run" {
        (report.total_files, report.total_bytes)
    } else {
        (report.files_restored, report.bytes_restored)
    };
    let mut out = format!(
        "{verb} {files} files, {} into {}\n",
        bytes(size),
        report.destination
    );
    if let Some(error) = &report.error {
        let _ = writeln!(out, "{error}");
    }
    if let Some(id) = report.id.as_deref().filter(|_| !report.finished) {
        let _ = writeln!(out, "Still running; read it with: backup job {id}");
    }
    out
}
