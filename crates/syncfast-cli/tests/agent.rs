//! The commands an agent runs, through the real binary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;

use common::{bed, write};

#[test]
fn status_json_of_an_empty_config_is_an_empty_list() {
    let Some(bed) = bed() else { return };

    let run = bed.run(&["status", "--json"]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.json(), serde_json::json!([]));
}

#[test]
fn an_agent_adds_a_sync_reviews_it_confirms_it_and_sees_it_up_to_date() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("a.txt"), "hello");

    let added = bed.add_docs();
    let shown = bed.run(&["review", "show", "docs", "--json"]);
    let hash = shown.json()["filters_hash"].as_str().unwrap().to_owned();
    let confirmed = bed.run(&[
        "review",
        "confirm",
        "docs",
        "--filters-hash",
        &hash,
        "--json",
    ]);
    let status = bed.run(&["status"]);

    assert_eq!(added.code, 0, "{}", added.stdout);
    assert_eq!(added.json()["sync"]["status"], "NeedsReview");
    assert_eq!(shown.json()["counts"]["upload"], 1);
    assert_eq!(shown.json()["agent_can_confirm"], true);
    assert_eq!(confirmed.code, 0, "{}", confirmed.stdout);
    assert_eq!(confirmed.json()["sync"]["status"], "UpToDate");
    assert_eq!(
        fs::read_to_string(bed.remote().join("a.txt")).unwrap(),
        "hello"
    );
    assert!(
        status.stdout.starts_with("docs\tUpToDate\t"),
        "{}",
        status.stdout
    );
}

#[test]
fn a_review_that_deletes_files_exits_four_with_the_dedicated_message_and_changes_nothing() {
    let Some(bed) = bed() else { return };
    bed.synced_docs();
    for name in ["b.txt", "c.txt", "d.txt"] {
        fs::remove_file(bed.local().join(name)).unwrap();
    }
    assert_eq!(bed.run(&["sync", "docs", "--json"]).code, 0);
    let shown = bed.run(&["review", "show", "docs", "--json"]);
    let hash = shown.json()["filters_hash"].as_str().unwrap().to_owned();

    let refused = bed.run(&["review", "confirm", "docs", "--filters-hash", &hash]);
    let json = bed.run(&[
        "review",
        "confirm",
        "docs",
        "--filters-hash",
        &hash,
        "--json",
    ]);

    assert_eq!(shown.json()["agent_can_confirm"], false);
    assert_eq!(refused.code, 4);
    assert_eq!(
        refused.stderr.trim(),
        "syncfast: This review deletes files. Confirm it in syncfast."
    );
    assert_eq!(json.code, 4);
    assert_eq!(json.json()["error"]["code"], "deletes_need_user");
    assert!(bed.remote().join("b.txt").exists());
    assert!(
        !bed.root
            .join("state/syncfast/pairs/docs/confirm.json")
            .exists()
    );
    assert_eq!(
        bed.run(&["status"]).stdout.split('\t').nth(1),
        Some("NeedsReview")
    );
}

#[test]
fn a_stale_hash_exits_five() {
    let Some(bed) = bed() else { return };
    assert_eq!(bed.add_docs().code, 0);
    assert_eq!(bed.run(&["review", "show", "docs", "--json"]).code, 0);

    let run = bed.run(&["review", "confirm", "docs", "--filters-hash", "stale"]);

    assert_eq!(run.code, 5);
}

#[test]
fn explore_prints_counts_and_sizes_as_json_and_a_tree_as_text() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("src/a.rs"), "fn main() {}");
    write(&bed.local().join("src/b.rs"), "fn b() {}");
    write(&bed.local().join("readme.md"), "# SECRET-TITLE");
    let target = bed.local();
    let target = target.to_str().unwrap();

    let json = bed.run(&["explore", target, "--json"]);
    let text = bed.run(&["explore", target]);

    assert_eq!(json.code, 0, "{}", json.stderr);
    assert_eq!(json.json()["files"], 3);
    assert_eq!(json.json()["bytes"], 12 + 9 + 14);
    assert!(!json.stdout.contains("SECRET"));
    assert!(text.stdout.contains("3 files"), "{}", text.stdout);
    assert!(text.stdout.contains("src/"), "{}", text.stdout);
}

#[test]
fn an_unknown_sync_exits_two_and_json_errors_go_to_stdout() {
    let Some(bed) = bed() else { return };

    let run = bed.run(&["show", "nope", "--json"]);

    assert_eq!(run.code, 2);
    assert_eq!(run.json()["error"]["code"], "not_found");
    assert_eq!(
        run.json()["error"]["message"],
        "no sync has the id \"nope\""
    );
}

#[test]
fn the_agent_guide_is_the_text_of_docs_agents_md() {
    let Some(bed) = bed() else { return };

    let run = bed.run(&["agent-guide"]);

    assert_eq!(run.stdout, include_str!("../../../docs/agents.md"));
    assert!(
        run.stdout
            .contains("flatpak run --command=syncfast io.github.raphael_soares.syncfast mcp")
    );
}

#[test]
fn copy_and_move_json_carry_the_files_the_bytes_and_the_outcome() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("docs/a.txt"), "aaa");
    write(&bed.local().join("docs/sub/b.txt"), "bb");
    let source = bed.local().join("docs");
    let source = source.to_str().unwrap();
    let copy_to = format!("{}/copied", bed.remote_arg());
    let move_to = format!("{}/moved", bed.remote_arg());

    let planned = bed.run(&["move", source, &move_to, "--dry-run", "--json"]);
    let after_dry_run = (
        bed.local().join("docs/a.txt").exists(),
        bed.remote().join("moved").exists(),
    );
    let copied = bed.run(&["copy", source, &copy_to, "--json"]);
    let moved = bed.run(&["move", source, &move_to, "--json"]);

    assert_eq!(planned.code, 0, "{}", planned.stderr);
    assert_eq!(planned.json()["transfer"]["status"], "dry_run");
    assert_eq!(planned.json()["transfer"]["files"], 2);
    assert_eq!(planned.json()["transfer"]["bytes"], 5);
    assert_eq!(planned.json()["finished"], true);
    assert_eq!(after_dry_run, (true, false));
    assert_eq!(copied.code, 0, "{}", copied.stderr);
    assert_eq!(copied.json()["transfer"]["operation"], "copy");
    assert_eq!(copied.json()["transfer"]["status"], "done");
    assert_eq!(moved.code, 0, "{}", moved.stderr);
    assert_eq!(moved.json()["transfer"]["status"], "done");
    assert_eq!(
        fs::read_to_string(bed.remote().join("moved/sub/b.txt")).unwrap(),
        "bb"
    );
    assert!(bed.remote().join("copied/a.txt").exists());
    assert!(!bed.local().join("docs").exists());
}

#[test]
fn a_transfer_that_outlasts_the_wait_is_read_with_transfer_and_the_text_says_what_happened() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("one.txt"), "1");
    let source = bed.local().join("one.txt");
    let source = source.to_str().unwrap();
    let destination = format!("{}/one.txt", bed.remote_arg());

    let started = bed.run(&["copy", source, &destination, "--wait", "0", "--json"]);
    let id = started.json()["transfer"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let later = bed.run(&["transfer", &id, "--json"]);
    let text = bed.run(&["transfer", &id]);

    assert_eq!(started.code, 0, "{}", started.stderr);
    assert_eq!(later.code, 0, "{}", later.stderr);
    assert_eq!(later.json()["finished"], true);
    assert_eq!(later.json()["transfer"]["status"], "done");
    assert!(text.stdout.starts_with("Copied file "), "{}", text.stdout);
    assert_eq!(
        fs::read_to_string(bed.remote().join("one.txt")).unwrap(),
        "1"
    );
}

#[test]
fn an_existing_destination_exits_two_and_an_unknown_transfer_exits_two() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("a.txt"), "new");
    write(&bed.remote().join("a.txt"), "old");
    let source = bed.local().join("a.txt");
    let destination = format!("{}/a.txt", bed.remote_arg());

    let refused = bed.run(&["copy", source.to_str().unwrap(), &destination, "--json"]);
    let unknown = bed.run(&["transfer", "20260101-000000-000", "--json"]);

    assert_eq!(refused.code, 2);
    assert_eq!(refused.json()["error"]["code"], "invalid");
    assert_eq!(
        fs::read_to_string(bed.remote().join("a.txt")).unwrap(),
        "old"
    );
    assert_eq!(unknown.code, 2);
}

#[test]
fn moving_the_folder_of_a_sync_exits_two_and_names_the_sync() {
    let Some(bed) = bed() else { return };
    bed.synced_docs();
    let local = bed.local();

    let refused = bed.run(&["move", local.to_str().unwrap(), "~/elsewhere", "--json"]);

    assert_eq!(refused.code, 2);
    assert!(
        refused.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("\"docs\"")
    );
    assert!(local.join("a.txt").exists());
}

fn config_text(bed: &common::Bed) -> String {
    fs::read_to_string(bed.root.join("config/syncfast/config.toml")).unwrap()
}

#[test]
fn settings_json_lists_every_key_with_its_origin_and_default() {
    let Some(bed) = bed() else { return };
    bed.synced_docs();

    let defaults = bed.run(&["settings", "--json"]);
    let of_sync = bed.run(&["settings", "--sync", "docs", "--json"]);

    assert_eq!(defaults.code, 0, "{}", defaults.stderr);
    assert_eq!(defaults.json()["scope"], "defaults");
    assert_eq!(defaults.json()["settings"].as_array().unwrap().len(), 13);
    assert_eq!(defaults.json()["lowered_safety"], serde_json::json!([]));
    assert_eq!(of_sync.json()["scope"], "docs");
    assert_eq!(
        of_sync.json()["settings"][1],
        serde_json::json!({
            "key": "debounce", "value": "30s", "origin": "builtin", "default": "30s", "applies": true
        })
    );
}

#[test]
fn settings_text_is_one_line_per_key() {
    let Some(bed) = bed() else { return };

    let run = bed.run(&["settings"]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stdout.starts_with("settings of defaults\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("\nmax_delete\t50\tbuiltin\n"),
        "{}",
        run.stdout
    );
}

#[test]
fn settings_set_writes_defaults_or_the_sync_and_reset_takes_it_back() {
    let Some(bed) = bed() else { return };
    bed.synced_docs();

    let defaults = bed.run(&["settings", "set", "debounce", "10s", "--json"]);
    let own = bed.run(&[
        "settings", "set", "interval", "10m", "--sync", "docs", "--json",
    ]);
    let written = config_text(&bed);
    let reset = bed.run(&["settings", "reset", "interval", "--sync", "docs", "--json"]);
    let reset_defaults = bed.run(&["settings", "reset", "debounce", "--json"]);

    assert_eq!(defaults.code, 0, "{}", defaults.stderr);
    assert_eq!(defaults.json()["setting"]["value"], "10s");
    assert_eq!(own.json()["setting"]["origin"], "sync");
    assert!(
        written.contains("[defaults]\ndebounce = \"10s\""),
        "{written}"
    );
    assert!(written.contains("interval = \"10m\""), "{written}");
    assert_eq!(reset.json()["setting"]["origin"], "builtin");
    assert_eq!(reset_defaults.json()["setting"]["value"], "30s");
    let after = config_text(&bed);
    assert!(
        !after.contains("debounce =") && !after.contains("interval ="),
        "{after}"
    );
}

#[test]
fn an_invalid_setting_exits_two_with_the_message_of_the_config() {
    let Some(bed) = bed() else { return };
    bed.synced_docs();
    let before = config_text(&bed);

    let run = bed.run(&["settings", "set", "debounce", "2s"]);
    let json = bed.run(&["settings", "set", "debounce", "2s", "--json"]);
    let unknown = bed.run(&["settings", "set", "nope", "1"]);

    assert_eq!(run.code, 2);
    assert_eq!(run.stderr.trim(), "syncfast: debounce must be at least 5s");
    assert_eq!(json.json()["error"]["code"], "invalid");
    assert_eq!(unknown.code, 2);
    assert_eq!(config_text(&bed), before);
}

#[test]
fn a_setting_that_lowers_safety_exits_four_and_changes_nothing() {
    let Some(bed) = bed() else { return };
    bed.synced_docs();
    let before = config_text(&bed);

    let raise = bed.run(&["settings", "set", "max_delete", "60", "--sync", "docs"]);
    let versions = bed.run(&["settings", "set", "versions", "false", "--json"]);
    let lower = bed.run(&[
        "settings",
        "set",
        "max_delete",
        "20",
        "--sync",
        "docs",
        "--json",
    ]);

    assert_eq!(raise.code, 4);
    assert_eq!(
        raise.stderr.trim(),
        "syncfast: This setting lowers safety (max_delete). Change it in syncfast."
    );
    assert_eq!(versions.code, 4);
    assert_eq!(versions.json()["error"]["code"], "needs_user");
    assert_eq!(lower.code, 0, "{}", lower.stderr);
    assert!(!config_text(&bed).contains("max_delete = 60"));
    assert_ne!(config_text(&bed), before);
}

#[test]
fn show_and_status_report_the_keys_a_user_made_less_safe() {
    let Some(bed) = bed() else { return };
    bed.synced_docs();
    let config = config_text(&bed).replace("[[pair]]", "[[pair]]\nmax_delete = 90");
    write(&bed.root.join("config/syncfast/config.toml"), &config);

    let status = bed.run(&["status", "--json"]);
    let shown = bed.run(&["show", "docs"]);
    let settings = bed.run(&["settings", "--sync", "docs", "--json"]);

    assert_eq!(
        status.json()[0]["lowered_safety"],
        serde_json::json!(["max_delete"])
    );
    assert!(
        shown.stdout.contains("safety lowered: max_delete"),
        "{}",
        shown.stdout
    );
    assert_eq!(
        settings.json()["lowered_safety"],
        serde_json::json!(["max_delete"])
    );
}
