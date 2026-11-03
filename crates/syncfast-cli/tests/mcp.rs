//! `syncfast mcp` end to end: the real binary on stdio, spoken to as a
//! client would.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{BufRead as _, BufReader, Write as _};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use common::{Bed, bed, write};
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

const TOOLS: [&str; 20] = [
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
    "sync_now",
    "show_review",
    "confirm_review",
    "list_conflicts",
    "resolve_conflict",
    "list_activity",
    "show_round_log",
    "theme",
];

#[test]
fn initialize_names_the_server_and_carries_the_recommended_flow() {
    let Some(bed) = bed() else { return };
    let mut client = Client::start(&bed);

    let result = client.initialize();

    assert_eq!(result["serverInfo"]["name"], "syncfast");
    let instructions = result["instructions"].as_str().unwrap();
    for step in ["Explore", "Propose", "Add", "Review", "confirm_review"] {
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
