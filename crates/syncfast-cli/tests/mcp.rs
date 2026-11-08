//! `syncfast mcp` end to end: the real binary on stdio, spoken to as a
//! client would.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use common::{Bed, backup_bed, bed, write};
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(120);

struct Client {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Client {
    fn start(bed: &Bed) -> Self {
        let mut child = bed
            .command()
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
            next_id: 0,
        }
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Sends a request and returns its `result`, or panics with its error.
    fn request(&mut self, method: &str, params: &Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let line = self
                .lines
                .recv_timeout(TIMEOUT)
                .expect("the server answers");
            let message: Value = serde_json::from_str(&line).unwrap();
            if message["id"] == json!(id) {
                assert!(message.get("error").is_none(), "{message}");
                return message["result"].clone();
            }
        }
    }

    fn initialize(&mut self) -> Value {
        let result = self.request(
            "initialize",
            &json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "0.0.1"}
            }),
        );
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        result
    }

    /// `(is_error, text)` of a tool call.
    fn call(&mut self, tool: &str, arguments: &Value) -> (bool, String) {
        let result = self.request("tools/call", &json!({"name": tool, "arguments": arguments}));
        let text = result["content"][0]["text"].as_str().unwrap().to_owned();
        (result["isError"] == json!(true), text)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const TOOLS: [&str; 39] = [
    "status",
    "show_sync",
    "add_sync",
    "remove_sync",
    "pause_sync",
    "resume_sync",
    "set_filters",
    "test_filters",
    "list_accounts",
    "list_remote_folders",
    "create_remote_folder",
    "explore",
    "copy",
    "move",
    "show_transfer",
    "sync_now",
    "show_review",
    "confirm_review",
    "list_conflicts",
    "resolve_conflict",
    "list_activity",
    "show_round_log",
    "theme",
    "get_settings",
    "set_setting",
    "reset_setting",
    "list_backups",
    "show_backup",
    "list_snapshots",
    "list_snapshot_files",
    "backup_now",
    "check_backup",
    "pause_backup",
    "resume_backup",
    "create_backup",
    "edit_backup",
    "remove_backup",
    "restore_snapshot",
    "show_backup_job",
];

#[test]
fn initialize_names_the_server_and_carries_the_recommended_flow() {
    let Some(bed) = bed() else { return };
    let mut client = Client::start(&bed);

    let result = client.initialize();

    assert_eq!(result["serverInfo"]["name"], "syncfast");
    let instructions = result["instructions"].as_str().unwrap();
    for step in [
        "Explore",
        "Propose",
        "Add",
        "Review",
        "confirm_review",
        "Organize",
        "show_transfer",
        "Backups",
        "create_backup",
        "restore_snapshot",
    ] {
        assert!(
            instructions.contains(step),
            "missing {step}: {instructions}"
        );
    }
    assert!(result["capabilities"]["tools"].is_object());
}

#[test]
fn tools_list_has_one_tool_per_operation_each_with_a_description_and_a_schema() {
    let Some(bed) = bed() else { return };
    let mut client = Client::start(&bed);
    client.initialize();

    let listed = client.request("tools/list", &json!({}));

    let tools = listed["tools"].as_array().unwrap();
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    let mut expected = TOOLS.to_vec();
    expected.sort_unstable();
    assert_eq!(names, expected);
    for tool in tools {
        assert!(
            tool["description"].as_str().is_some_and(|d| d.len() > 20),
            "{tool}"
        );
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
    }
    let explore = tools.iter().find(|t| t["name"] == "explore").unwrap();
    assert_eq!(explore["inputSchema"]["required"], json!(["target"]));
    assert!(
        !names
            .iter()
            .any(|name| name.contains("account") && name.contains("create"))
    );
}

#[test]
fn calling_status_returns_the_syncs_as_json_text() {
    let Some(bed) = bed() else { return };
    assert_eq!(bed.add_docs().code, 0);
    let mut client = Client::start(&bed);
    client.initialize();

    let (is_error, text) = client.call("status", &json!({}));

    assert!(!is_error, "{text}");
    let syncs: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(syncs[0]["id"], "docs");
    assert_eq!(syncs[0]["status"], "NeedsReview");
}

#[test]
fn calling_explore_returns_counts_and_sizes_and_never_content() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("pics/a.png"), &"x".repeat(50));
    write(&bed.local().join("pics/b.png"), &"y".repeat(30));
    write(&bed.local().join("notes.txt"), "SECRET-NOTE");
    let mut client = Client::start(&bed);
    client.initialize();

    let (is_error, text) = client.call(
        "explore",
        &json!({"target": bed.local().to_str().unwrap(), "depth": 1}),
    );

    assert!(!is_error, "{text}");
    let found: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        (found["files"].as_u64(), found["bytes"].as_u64()),
        (Some(3), Some(91))
    );
    let pics = found["tree"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == "pics")
        .unwrap();
    assert_eq!(
        (pics["files"].as_u64(), pics["bytes"].as_u64()),
        (Some(2), Some(80))
    );
    assert!(!text.contains("SECRET"));
}

#[test]
fn a_review_that_deletes_files_comes_back_as_a_tool_error_with_the_core_message() {
    let Some(bed) = bed() else { return };
    bed.synced_docs();
    for name in ["b.txt", "c.txt", "d.txt"] {
        std::fs::remove_file(bed.local().join(name)).unwrap();
    }
    assert_eq!(bed.run(&["sync", "docs"]).code, 0);
    let mut client = Client::start(&bed);
    client.initialize();
    let (_, shown) = client.call("show_review", &json!({"id": "docs"}));
    let hash = serde_json::from_str::<Value>(&shown).unwrap()["filters_hash"]
        .as_str()
        .unwrap()
        .to_owned();

    let (is_error, text) = client.call(
        "confirm_review",
        &json!({"id": "docs", "filters_hash": hash}),
    );

    assert!(is_error);
    assert_eq!(text, "This review deletes files. Confirm it in syncfast.");
    assert!(bed.remote().join("b.txt").exists());
}

#[test]
fn an_agent_adds_reviews_and_confirms_a_sync_over_mcp() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("a.txt"), "hello");
    let mut client = Client::start(&bed);
    client.initialize();

    let (added_error, added) = client.call(
        "add_sync",
        &json!({"name": "Docs", "local": bed.local().to_str().unwrap(), "remote": bed.remote_arg()}),
    );
    let (_, shown) = client.call("show_review", &json!({"id": "docs"}));
    let hash = serde_json::from_str::<Value>(&shown).unwrap()["filters_hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let (confirm_error, confirmed) = client.call(
        "confirm_review",
        &json!({"id": "docs", "filters_hash": hash}),
    );

    assert!(!added_error, "{added}");
    assert!(!confirm_error, "{confirmed}");
    let confirmed: Value = serde_json::from_str(&confirmed).unwrap();
    assert_eq!(confirmed["sync"]["status"], "UpToDate");
    assert_eq!(
        std::fs::read_to_string(bed.remote().join("a.txt")).unwrap(),
        "hello"
    );
}

#[test]
fn an_unknown_sync_is_a_tool_error_not_a_protocol_error() {
    let Some(bed) = bed() else { return };
    let mut client = Client::start(&bed);
    client.initialize();

    let (is_error, text) = client.call("show_sync", &json!({"id": "nope"}));

    assert!(is_error);
    assert_eq!(text, "no sync has the id \"nope\"");
}

#[test]
fn settings_over_mcp_answer_as_the_commands_do() {
    let Some(bed) = bed() else { return };
    bed.synced_docs();
    let cli = bed.run(&["settings", "--sync", "docs", "--json"]).json();
    let mut client = Client::start(&bed);
    client.initialize();

    let (is_error, text) = client.call("get_settings", &json!({"sync": "docs"}));
    let (changed_error, changed) = client.call(
        "set_setting",
        &json!({"sync": "docs", "key": "interval", "value": "10m"}),
    );
    let cli_after = bed.run(&["settings", "--sync", "docs", "--json"]).json();
    let (reset_error, reset) =
        client.call("reset_setting", &json!({"sync": "docs", "key": "interval"}));

    assert!(!is_error, "{text}");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), cli);
    assert!(!changed_error, "{changed}");
    assert_eq!(
        serde_json::from_str::<Value>(&changed).unwrap()["setting"]["value"],
        "10m"
    );
    assert_eq!(cli_after["settings"][3]["value"], "10m");
    assert!(!reset_error, "{reset}");
    assert_eq!(
        serde_json::from_str::<Value>(&reset).unwrap()["setting"]["value"],
        "5m"
    );
}

#[test]
fn a_setting_that_lowers_safety_is_a_tool_error_with_the_message_of_the_cli() {
    let Some(bed) = bed() else { return };
    bed.synced_docs();
    let cli = bed.run(&["settings", "set", "versions", "false"]);
    let mut client = Client::start(&bed);
    client.initialize();

    let (is_error, text) = client.call("set_setting", &json!({"key": "versions", "value": false}));
    let (list_error, invalid) =
        client.call("set_setting", &json!({"key": "debounce", "value": "2s"}));
    let (shape_error, shape) =
        client.call("set_setting", &json!({"key": "debounce", "value": [1]}));

    assert_eq!(cli.code, 4);
    assert!(is_error);
    assert_eq!(format!("syncfast: {text}"), cli.stderr.trim());
    assert!(list_error);
    assert_eq!(invalid, "debounce must be at least 5s");
    assert!(shape_error);
    assert_eq!(shape, "value must be a boolean, a whole number or a string");
}

#[test]
fn a_deleted_loser_is_a_tool_error_that_changes_nothing() {
    let Some(bed) = bed() else { return };
    bed.synced_docs();
    let cli = bed.run(&[
        "settings",
        "set",
        "conflict_loser",
        "delete",
        "--sync",
        "docs",
    ]);
    let config = bed.root.join("config/syncfast/config.toml");
    let before = std::fs::read_to_string(&config).unwrap();
    let mut client = Client::start(&bed);
    client.initialize();

    let (is_error, text) = client.call(
        "set_setting",
        &json!({"key": "conflict_loser", "value": "delete", "sync": "docs"}),
    );

    assert_eq!(cli.code, 4);
    assert!(is_error);
    assert_eq!(format!("syncfast: {text}"), cli.stderr.trim());
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
}

#[test]
fn copy_and_move_answer_over_mcp_as_the_commands_do_and_show_transfer_reads_them() {
    let Some(bed) = bed() else { return };
    write(&bed.local().join("docs/a.txt"), "hello");
    let source = bed.local().join("docs");
    let source = source.to_str().unwrap();
    let mut client = Client::start(&bed);
    client.initialize();
    let copy_to = format!("{}/copied", bed.remote_arg());
    let move_to = format!("{}/moved", bed.remote_arg());

    let (planned_error, planned) = client.call(
        "move",
        &json!({"source": source, "destination": move_to, "dry_run": true}),
    );
    let (copy_error, copied) = client.call(
        "copy",
        &json!({"source": source, "destination": copy_to, "wait_seconds": 0}),
    );
    let id = serde_json::from_str::<Value>(&copied).unwrap()["transfer"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (show_error, shown) = client.call("show_transfer", &json!({"id": id}));
    let (refused, message) =
        client.call("copy", &json!({"source": source, "destination": copy_to}));

    assert!(!planned_error, "{planned}");
    let planned: Value = serde_json::from_str(&planned).unwrap();
    assert_eq!(planned["transfer"]["status"], "dry_run");
    assert_eq!(planned["transfer"]["bytes"], 5);
    assert!(!copy_error, "{copied}");
    assert!(!show_error, "{shown}");
    let shown: Value = serde_json::from_str(&shown).unwrap();
    assert_eq!(shown["transfer"]["status"], "done");
    assert_eq!(shown["finished"], true);
    assert_eq!(
        std::fs::read_to_string(bed.remote().join("copied/a.txt")).unwrap(),
        "hello"
    );
    assert!(bed.local().join("docs/a.txt").exists());
    assert!(refused);
    assert!(message.contains("already exists"), "{message}");
}

fn parsed(call: &(bool, String)) -> Value {
    assert!(!call.0, "{}", call.1);
    serde_json::from_str(&call.1).unwrap()
}

#[test]
fn the_backup_tools_create_run_read_and_restore_without_ever_returning_the_key() {
    let Some(bed) = backup_bed() else { return };
    let docs = bed.root.join("docs");
    write(&docs.join("a.txt"), "content of a");
    write(&docs.join("sub/b.txt"), "content of b");
    let dest = bed.root.join("repo");
    let to = bed.root.join("restored");
    let mut client = Client::start(&bed);
    client.initialize();
    let mut transcript = String::new();
    let mut call = |client: &mut Client, tool: &str, arguments: Value| {
        let answer = client.call(tool, &arguments);
        transcript.push_str(&answer.1);
        answer
    };

    let created = parsed(&call(
        &mut client,
        "create_backup",
        json!({"name": "Docs", "paths": [docs], "dest": dest}),
    ));
    let listed = parsed(&call(&mut client, "list_backups", json!({})));
    let run = parsed(&call(&mut client, "backup_now", json!({"id": "docs"})));
    let snapshots = parsed(&call(&mut client, "list_snapshots", json!({"id": "docs"})));
    let top = parsed(&call(
        &mut client,
        "list_snapshot_files",
        json!({"id": "docs", "snapshot": "latest"}),
    ));
    let shown = parsed(&call(&mut client, "show_backup", json!({"id": "docs"})));
    let dry = parsed(&call(
        &mut client,
        "restore_snapshot",
        json!({"id": "docs", "snapshot": "latest", "to": to, "dry_run": true}),
    ));
    let dry_created = to.exists();
    let restored = parsed(&call(
        &mut client,
        "restore_snapshot",
        json!({"id": "docs", "snapshot": "latest", "to": to}),
    ));
    let again = call(
        &mut client,
        "restore_snapshot",
        json!({"id": "docs", "snapshot": "latest", "to": to}),
    );
    let job = parsed(&call(
        &mut client,
        "show_backup_job",
        json!({"id": restored["id"]}),
    ));

    assert_eq!(created["id"], "docs");
    assert_eq!(created["key_unsaved"], true);
    assert_eq!(listed[0]["key"], "stored");
    assert_eq!(run["finished"], true, "{run}");
    assert_eq!(snapshots.as_array().unwrap().len(), 1);
    assert_eq!(top["files"][0]["path"], docs.to_str().unwrap());
    assert_eq!(shown["last_run"]["summary"]["files_new"], 2, "{shown}");
    assert_eq!(dry["status"], "dry_run");
    assert!(!dry_created);
    assert_eq!(restored["status"], "done", "{restored}");
    assert_eq!(
        std::fs::read_to_string(to.join("a.txt")).unwrap(),
        "content of a"
    );
    assert!(again.0, "a second restore into the folder is refused");
    assert_eq!(job["status"], "done", "{job}");
    let key = std::fs::read_to_string(bed.root.join("data/syncfast/dev-keys/docs.key")).unwrap();
    assert!(key.trim().len() >= 40);
    assert!(!transcript.contains(key.trim()), "a tool returned the key");
}

#[test]
fn the_backup_tools_refuse_what_lowers_safety_and_change_nothing() {
    let Some(bed) = backup_bed() else { return };
    let docs = bed.root.join("docs");
    write(&docs.join("a.txt"), "content of a");
    let dest = bed.root.join("repo");
    let mut client = Client::start(&bed);
    client.initialize();
    parsed(&client.call(
        "create_backup",
        &json!({"name": "Docs", "paths": [docs], "dest": dest}),
    ));
    let before = parsed(&client.call("show_backup", &json!({"id": "docs"})));

    for (args, key) in [
        (json!({"id": "docs", "keep": {"daily": 1}}), "keep.daily"),
        (
            json!({"id": "docs", "one_file_system": false}),
            "one_file_system",
        ),
        (json!({"id": "docs", "paths": []}), "paths"),
    ] {
        let (is_error, text) = client.call("edit_backup", &args);

        assert!(is_error, "{args}: {text}");
        assert!(
            text.contains(&format!("This setting lowers safety ({key})")),
            "{args}: {text}"
        );
    }
    let (moved, moved_text) = client.call(
        "edit_backup",
        &json!({"id": "docs", "dest": bed.root.join("elsewhere")}),
    );
    let (removed, removed_text) = client.call("remove_backup", &json!({"id": "docs"}));
    let allowed = client.call("edit_backup", &json!({"id": "docs", "keep": {"daily": 30}}));
    let after = parsed(&client.call("show_backup", &json!({"id": "docs"})));

    assert!(moved, "{moved_text}");
    assert!(removed, "{removed_text}");
    assert!(!allowed.0, "{}", allowed.1);
    assert_eq!(after["keep"]["daily"], 30);
    assert_eq!(after["paths"], before["paths"]);
    assert_eq!(after["dest"], before["dest"]);
}

#[test]
fn pausing_a_backup_needs_an_id_or_all_but_not_both() {
    let Some(bed) = backup_bed() else { return };
    let mut client = Client::start(&bed);
    client.initialize();

    let (neither, _) = client.call("pause_backup", &json!({}));
    let (both, _) = client.call("pause_backup", &json!({"id": "x", "all": true}));
    let (all, text) = client.call("pause_backup", &json!({"all": true}));

    assert!(neither && both);
    assert!(!all, "{text}");
}
