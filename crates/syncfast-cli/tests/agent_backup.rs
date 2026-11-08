//! The backup commands of `syncfast`, the way an agent drives them: the real
//! binary, a real restic and rclone, `type = local` remotes and folders.

#![allow(clippy::unwrap_used)]

mod common;

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use common::{Bed, Run, backup_bed, write};
use serde_json::Value;

struct Rig {
    bed: Bed,
    docs: PathBuf,
    dest: PathBuf,
}

fn rig() -> Option<Rig> {
    let bed = backup_bed()?;
    let docs = bed.root.join("docs");
    write(&docs.join("a.txt"), "content of a");
    write(&docs.join("b.txt"), "content of b");
    write(&docs.join("sub/c.txt"), "content of c");
    let dest = bed.root.join("repo");
    Some(Rig { bed, docs, dest })
}

impl Rig {
    fn add(&self) -> Run {
        self.bed.run(&[
            "backup",
            "add",
            "--name",
            "Docs",
            "--path",
            self.docs.to_str().unwrap(),
            "--dest",
            self.dest.to_str().unwrap(),
            "--json",
        ])
    }

    /// A backup with one snapshot, ready for the other commands.
    fn backed_up(&self) {
        let added = self.add();
        assert_eq!(added.code, 0, "{} {}", added.stdout, added.stderr);
        let started = Instant::now();
        while self.show()["last_success"].is_null() {
            assert!(
                started.elapsed() < Duration::from_secs(90),
                "the first run never ended: {}",
                self.show()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn show(&self) -> Value {
        let shown = self.bed.run(&["backup", "show", "docs", "--json"]);
        assert_eq!(shown.code, 0, "{} {}", shown.stdout, shown.stderr);
        shown.json()
    }

    /// The key as the plain dev key storage holds it.
    fn key(&self) -> String {
        let file = self.bed.root.join("data/syncfast/dev-keys/docs.key");
        fs::read_to_string(file).unwrap().trim().to_owned()
    }
}

fn assert_no_key(key: &str, run: &Run) {
    assert!(key.len() >= 40, "the sentinel is the real key: {key}");
    assert!(!run.stdout.contains(key), "stdout holds the key");
    assert!(!run.stderr.contains(key), "stderr holds the key");
}

fn names(files: &Value) -> Vec<String> {
    files["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn add_creates_the_backup_and_leaves_the_key_for_the_user_to_save() {
    let Some(rig) = rig() else { return };

    let added = rig.add();

    assert_eq!(added.code, 0, "{} {}", added.stdout, added.stderr);
    let created = added.json();
    assert_eq!(created["id"], "docs");
    assert_eq!(created["key_unsaved"], true);
    let shown = rig.show();
    assert_eq!(shown["key"], "stored");
    assert_eq!(shown["key_unsaved"], true);
    assert_eq!(shown["status"], "save_key");
    assert!(rig.key().len() >= 40);
}

#[test]
fn add_in_text_tells_the_user_to_open_syncfast_and_never_prints_the_key() {
    let Some(rig) = rig() else { return };

    let added = rig.bed.run(&[
        "backup",
        "add",
        "--name",
        "Docs",
        "--path",
        rig.docs.to_str().unwrap(),
        "--dest",
        rig.dest.to_str().unwrap(),
    ]);

    assert_eq!(added.code, 0, "{} {}", added.stdout, added.stderr);
    assert!(added.stdout.contains("open syncfast"), "{}", added.stdout);
    assert_no_key(&rig.key(), &added);
}

#[test]
fn no_command_prints_the_key() {
    let Some(rig) = rig() else { return };
    rig.backed_up();
    let key = rig.key();
    let out = rig.bed.root.join("restored");
    let out = out.to_str().unwrap();

    let runs = [
        rig.bed.run(&["backups", "--json"]),
        rig.bed.run(&["backups"]),
        rig.bed.run(&["backup", "show", "docs"]),
        rig.bed.run(&["backup", "show", "docs", "--json"]),
        rig.bed.run(&["backup", "snapshots", "docs", "--json"]),
        rig.bed
            .run(&["backup", "files", "docs", "latest", "--json"]),
        rig.bed.run(&["backup", "run", "docs"]),
        rig.bed.run(&["backup", "check", "docs", "--json"]),
        rig.bed.run(&["backup", "pause", "docs"]),
        rig.bed.run(&["backup", "resume", "docs"]),
        rig.bed.run(&["backup", "set", "docs", "keep.daily", "30"]),
        rig.bed.run(&["backup", "set", "docs", "keep.daily", "1"]),
        rig.bed.run(&["backup", "remove", "docs"]),
        rig.bed
            .run(&["backup", "restore", "docs", "latest", "--to", out]),
        rig.bed
            .run(&["backup", "restore", "docs", "latest", "--to", out]),
        rig.bed.run(&["backup", "job", "docs"]),
        rig.bed.run(&["backup", "show", "nope"]),
        rig.bed.run(&["agent-guide"]),
    ];

    for run in &runs {
        assert_no_key(&key, run);
    }
}

#[test]
fn backups_lists_the_backup_and_says_when_there_is_none() {
    let Some(rig) = rig() else { return };
    let none = rig.bed.run(&["backups", "--json"]);
    assert_eq!(none.code, 0);
    assert_eq!(none.json().as_array().unwrap().len(), 0);

    rig.backed_up();
    let listed = rig.bed.run(&["backups", "--json"]);

    assert_eq!(listed.code, 0);
    let listed = listed.json();
    assert_eq!(listed[0]["id"], "docs");
    assert!(listed[0]["last_success"].is_string(), "{listed}");
    let text = rig.bed.run(&["backups"]);
    assert!(text.stdout.starts_with("docs\t"), "{}", text.stdout);
}

#[test]
fn run_adds_a_snapshot_with_what_changed_and_snapshots_and_files_read_it() {
    let Some(rig) = rig() else { return };
    rig.backed_up();
    write(&rig.docs.join("d.txt"), "content of d");

    let run = rig.bed.run(&["backup", "run", "docs", "--json"]);

    assert_eq!(run.code, 0, "{} {}", run.stdout, run.stderr);
    assert_eq!(run.json()["finished"], true, "{}", run.stdout);
    let shown = rig.show();
    assert_eq!(shown["last_run"]["exit_code"], 0, "{shown}");
    assert_eq!(shown["last_run"]["summary"]["files_new"], 1, "{shown}");
    let snapshots = rig.bed.run(&["backup", "snapshots", "docs", "--json"]);
    assert_eq!(snapshots.code, 0, "{}", snapshots.stderr);
    assert_eq!(snapshots.json().as_array().unwrap().len(), 2);
    let top = rig
        .bed
        .run(&["backup", "files", "docs", "latest", "--json"]);
    assert_eq!(top.code, 0, "{}", top.stderr);
    assert_eq!(top.json()["files"][0]["path"], rig.docs.to_str().unwrap());
    let docs_dir = top.json()["files"][0]["path"].as_str().unwrap().to_owned();
    let inside = rig
        .bed
        .run(&["backup", "files", "docs", "latest", &docs_dir, "--json"]);
    assert_eq!(names(&inside.json()), ["sub", "a.txt", "b.txt", "d.txt"]);
}

#[test]
fn files_cut_at_the_limit_say_so() {
    let Some(rig) = rig() else { return };
    rig.backed_up();
    let top = rig
        .bed
        .run(&["backup", "files", "docs", "latest", "--json"]);
    let docs_dir = top.json()["files"][0]["path"].as_str().unwrap().to_owned();

    let cut = rig.bed.run(&[
        "backup", "files", "docs", "latest", &docs_dir, "--limit", "1", "--json",
    ]);

    assert_eq!(cut.code, 0);
    assert_eq!(cut.json()["files"].as_array().unwrap().len(), 1);
    assert_eq!(cut.json()["truncated"], true);
}

#[test]
fn check_records_its_result() {
    let Some(rig) = rig() else { return };
    rig.backed_up();

    let checked = rig.bed.run(&["backup", "check", "docs", "--json"]);

    assert_eq!(checked.code, 0, "{} {}", checked.stdout, checked.stderr);
    assert_eq!(checked.json()["finished"], true, "{}", checked.stdout);
    let shown = rig.show();
    assert_eq!(shown["last_check"]["ok"], true, "{shown}");
    assert_eq!(shown["last_check"]["kind"], "quick", "{shown}");
}

#[test]
fn pause_and_resume_change_whether_the_backup_is_enabled() {
    let Some(rig) = rig() else { return };
    rig.backed_up();

    let paused = rig.bed.run(&["backup", "pause", "docs", "--json"]);
    assert_eq!(paused.code, 0, "{}", paused.stderr);
    assert_eq!(rig.show()["enabled"], false);
    let resumed = rig.bed.run(&["backup", "resume", "docs", "--json"]);

    assert_eq!(resumed.code, 0, "{}", resumed.stderr);
    assert_eq!(rig.show()["enabled"], true);
}

#[test]
fn a_setting_that_lowers_safety_exits_4_with_the_message_and_changes_nothing() {
    let Some(rig) = rig() else { return };
    rig.backed_up();
    let before = rig.show();
    let other = rig.bed.root.join("other");
    fs::create_dir_all(&other).unwrap();

    let attempts: [(&[&str], &str); 4] = [
        (&["keep.daily", "1"], "keep.daily"),
        (&["one_file_system", "false"], "one_file_system"),
        (&["paths"], "paths"),
        (&["paths", other.to_str().unwrap()], "paths"),
    ];
    for (args, key) in attempts {
        let mut command = vec!["backup", "set", "docs"];
        command.extend_from_slice(args);
        let run = rig.bed.run(&command);

        assert_eq!(run.code, 4, "{args:?}: {} {}", run.stdout, run.stderr);
        assert!(
            run.stderr.contains(&format!(
                "This setting lowers safety ({key}). Change it in syncfast."
            )),
            "{args:?}: {}",
            run.stderr
        );
        assert_eq!(rig.show(), before, "{args:?} changed the config");
    }
}

#[test]
fn a_destination_change_is_refused_for_good() {
    let Some(rig) = rig() else { return };
    rig.backed_up();
    let before = rig.show();
    let elsewhere = rig.bed.root.join("elsewhere");

    let run = rig
        .bed
        .run(&["backup", "set", "docs", "dest", elsewhere.to_str().unwrap()]);

    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    assert!(
        run.stderr.contains("Ask the user to create a new backup."),
        "{}",
        run.stderr
    );
    assert_eq!(rig.show(), before);
}

#[test]
fn settings_that_keep_more_or_change_nothing_unsafe_are_applied() {
    let Some(rig) = rig() else { return };
    rig.backed_up();

    for args in [
        ["keep.daily", "30"],
        ["name", "Papers"],
        ["limit_upload", "2M"],
    ] {
        let run = rig
            .bed
            .run(&["backup", "set", "docs", "--json", args[0], args[1]]);
        assert_eq!(run.code, 0, "{args:?}: {} {}", run.stdout, run.stderr);
    }

    let shown = rig.show();
    assert_eq!(shown["keep"]["daily"], 30);
    assert_eq!(shown["name"], "Papers");
}

#[test]
fn an_invalid_value_exits_2_and_changes_nothing() {
    let Some(rig) = rig() else { return };
    rig.backed_up();
    let before = rig.show();

    let run = rig
        .bed
        .run(&["backup", "set", "docs", "keep.daily", "many"]);

    assert_eq!(run.code, 2, "{} {}", run.stdout, run.stderr);
    assert_eq!(rig.show(), before);
}

#[test]
fn remove_always_exits_4() {
    let Some(rig) = rig() else { return };
    rig.backed_up();

    let run = rig.bed.run(&["backup", "remove", "docs"]);

    assert_eq!(run.code, 4, "{} {}", run.stdout, run.stderr);
    assert_eq!(rig.show()["id"], "docs");
}

#[test]
fn restore_into_a_new_folder_brings_the_files_and_a_second_restore_there_exits_2() {
    let Some(rig) = rig() else { return };
    rig.backed_up();
    let to = rig.bed.root.join("restored");

    let restored = rig.bed.run(&[
        "backup",
        "restore",
        "docs",
        "latest",
        "--to",
        to.to_str().unwrap(),
        "--json",
    ]);

    assert_eq!(restored.code, 0, "{} {}", restored.stdout, restored.stderr);
    let report = restored.json();
    assert_eq!(report["status"], "done", "{report}");
    assert_eq!(report["bytes_restored"], 36, "{report}");
    assert_eq!(
        fs::read_to_string(to.join("a.txt")).unwrap(),
        "content of a"
    );
    let again = rig.bed.run(&[
        "backup",
        "restore",
        "docs",
        "latest",
        "--to",
        to.to_str().unwrap(),
    ]);
    assert_eq!(again.code, 2, "{} {}", again.stdout, again.stderr);
    assert_eq!(
        fs::read_to_string(to.join("a.txt")).unwrap(),
        "content of a"
    );
}

#[test]
fn restore_can_take_only_some_paths() {
    let Some(rig) = rig() else { return };
    rig.backed_up();
    let to = rig.bed.root.join("some");
    let wanted = format!("{}/a.txt", rig.docs.display());

    let restored = rig.bed.run(&[
        "backup",
        "restore",
        "docs",
        "latest",
        "--to",
        to.to_str().unwrap(),
        "--path",
        &wanted,
        "--json",
    ]);

    assert_eq!(restored.code, 0, "{} {}", restored.stdout, restored.stderr);
    assert_eq!(restored.json()["files_restored"], 1);
    assert!(!to.join("b.txt").exists() && !to.join("docs/b.txt").exists());
}

#[test]
fn a_dry_run_reports_the_counts_and_creates_nothing() {
    let Some(rig) = rig() else { return };
    rig.backed_up();
    let to = rig.bed.root.join("never");

    let dry = rig.bed.run(&[
        "backup",
        "restore",
        "docs",
        "latest",
        "--to",
        to.to_str().unwrap(),
        "--dry-run",
        "--json",
    ]);

    assert_eq!(dry.code, 0, "{} {}", dry.stdout, dry.stderr);
    let report = dry.json();
    assert_eq!(report["status"], "dry_run", "{report}");
    assert_eq!(report["total_bytes"], 36, "{report}");
    assert_eq!(report["bytes_restored"], 0, "{report}");
    assert!(!to.exists());
}

#[test]
fn a_backup_that_does_not_exist_exits_2_and_the_error_is_json_with_json() {
    let Some(rig) = rig() else { return };

    let run = rig.bed.run(&["backup", "show", "nope", "--json"]);

    assert_eq!(run.code, 2, "{} {}", run.stdout, run.stderr);
    assert_eq!(run.json()["error"]["code"], "not_found");
    assert!(run.stdout.contains("nope"), "{}", run.stdout);
}

#[test]
fn a_destination_that_is_not_empty_exits_2_and_creates_nothing() {
    let Some(rig) = rig() else { return };
    write(&rig.dest.join("something"), "in the way");

    let added = rig.add();

    assert_eq!(added.code, 2, "{} {}", added.stdout, added.stderr);
    assert_eq!(
        rig.bed
            .run(&["backups", "--json"])
            .json()
            .as_array()
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn no_backup_command_ever_exits_5() {
    let Some(rig) = rig() else { return };
    rig.backed_up();
    let to = rig.bed.root.join("r");
    let to = to.to_str().unwrap();

    let codes: Vec<i32> = [
        vec!["backup", "set", "docs", "keep.daily", "1"],
        vec!["backup", "remove", "docs"],
        vec!["backup", "show", "nope"],
        vec!["backup", "restore", "docs", "nope", "--to", to],
        vec!["backup", "files", "docs", "nope"],
    ]
    .iter()
    .map(|args| rig.bed.run(args).code)
    .collect();

    assert_eq!(codes, [4, 4, 2, 2, 2]);
}

#[test]
fn a_relative_folder_is_made_absolute_from_the_working_folder() {
    let Some(rig) = rig() else { return };
    let cwd = rig.bed.root.join("work");
    write(&cwd.join(".dev/docs/a.txt"), "dev a");
    write(&cwd.join(".dev/docs/x.log"), "log");
    write(&cwd.join(".dev/docs/b.txt"), "dev b");

    let added = rig.bed.run_in(
        &cwd,
        &[
            "backup",
            "add",
            "--name",
            "dev-agent",
            "--path",
            "./.dev/docs",
            "--dest",
            "./.dev/repo",
            "--json",
        ],
    );

    assert_eq!(added.code, 0, "{} {}", added.stdout, added.stderr);
    let shown = rig
        .bed
        .run(&["backup", "show", "dev-agent", "--json"])
        .json();
    assert_eq!(shown["paths"][0], cwd.join(".dev/docs").to_str().unwrap());
    assert_eq!(shown["dest"], cwd.join(".dev/repo").to_str().unwrap());
    let lowered = rig
        .bed
        .run(&["backup", "set", "dev-agent", "keep.daily", "1"]);
    assert_eq!(lowered.code, 4, "{} {}", lowered.stdout, lowered.stderr);
    assert!(
        lowered
            .stderr
            .contains("This setting lowers safety (keep.daily). Change it in syncfast."),
        "{}",
        lowered.stderr
    );
}

#[test]
fn the_guide_teaches_the_backup_flow() {
    let Some(rig) = rig() else { return };

    let guide = rig.bed.run(&["agent-guide"]);

    assert_eq!(guide.code, 0);
    for needle in [
        "backup snapshots",
        "backup restore",
        "--dry-run",
        "never see the key",
        "exit 4",
    ] {
        assert!(guide.stdout.contains(needle), "missing {needle}");
    }
}
