use std::{
    collections::BTreeMap,
    io::Write,
    process::{Command, Output, Stdio},
};

use open_computer_use::{VERSION, contract::TOOL_NAMES};
use serde_json::{Value, json};

#[test]
fn cli_help_version_and_errors_are_truthful() {
    let help = run(&["help"], "");
    assert!(help.status.success());
    let help_text = text(&help.stdout);
    assert!(help_text.contains("Open Computer Use for Linux Wayland"));
    assert!(help_text.contains("list-apps"));
    assert!(help_text.contains("snapshot APP"));
    assert!(help_text.contains("init"));
    assert!(help_text.contains("call FILE"));

    let version = run(&["version"], "");
    assert!(version.status.success());
    assert_eq!(text(&version.stdout).trim(), VERSION);

    let unknown = run(&["not-a-command"], "");
    assert!(!unknown.status.success());
    assert!(text(&unknown.stderr).contains("unknown command"));

    let missing_app = run(&["snapshot"], "");
    assert!(!missing_app.status.success());
    assert!(text(&missing_app.stderr).contains("requires exactly one"));
}

#[test]
fn mcp_initialize_lists_tools_and_requires_prior_state() {
    let mut messages = initialization();
    messages.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
    }));
    messages.push(json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "tools/call",
        "params": {
            "name": "act_on_element",
            "arguments": {
                "state_id": "s-0000000000000000",
                "element_id": "1",
                "action": {"type": "invoke"},
            },
        },
    }));

    let output = run(&["mcp"], &json_lines(&messages));
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    let responses = responses_by_id(&output);

    let initialized = &responses[&1]["result"];
    assert_eq!(initialized["serverInfo"]["name"], "open-computer-use");
    assert_eq!(initialized["serverInfo"]["version"], VERSION);
    assert_eq!(initialized["capabilities"]["tools"]["listChanged"], false);
    assert!(
        initialized["instructions"]
            .as_str()
            .is_some_and(|text| text.contains("AT-SPI"))
    );

    let listed = responses[&2]["result"]["tools"]
        .as_array()
        .expect("tools array");
    let names: Vec<_> = listed
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    assert_eq!(names, TOOL_NAMES);

    let result = &responses[&10]["result"];
    assert_eq!(result["isError"], true, "response 10: {result}");
    assert!(
        result["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("no observation is available")),
        "response 10: {result}"
    );

    assert_eq!(
        output.stdout.iter().filter(|byte| **byte == b'\n').count(),
        responses.len()
    );
    assert!(text(&output.stderr).contains("no observation is available"));
}

#[test]
fn rmcp_rejects_a_malformed_call_shape_before_tool_validation() {
    let mut messages = initialization();
    messages.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "list_applications", "arguments": []},
    }));
    let output = run(&["mcp"], &json_lines(&messages));
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    let responses = responses_by_id(&output);
    assert_eq!(responses[&2]["error"]["code"], -32601);
    assert!(responses[&2].get("result").is_none());
}

#[test]
fn mcp_treats_eof_as_clean_shutdown_and_keeps_stdout_pure() {
    let output = run(&["mcp"], "");
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert!(output.stdout.is_empty());

    let mut messages = initialization();
    messages.push(json!({"jsonrpc": "2.0", "id": 8, "method": "ping"}));
    let output = run(&["mcp"], &json_lines(&messages));
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    for line in text(&output.stdout).lines() {
        serde_json::from_str::<Value>(line).expect("stdout line must be JSON-RPC");
    }
    assert!(responses_by_id(&output).contains_key(&8));
}

fn initialization() -> Vec<Value> {
    vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "stage-one-test", "version": "0.0.0"},
            },
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    ]
}

fn run(arguments: &[&str], input: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_open-computer-use"));
    command.args(arguments);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn open-computer-use");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(input.as_bytes())
        .expect("write child stdin");
    child.wait_with_output().expect("wait for child")
}

fn json_lines(messages: &[Value]) -> String {
    let mut output = messages
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    output.push('\n');
    output
}

fn responses_by_id(output: &Output) -> BTreeMap<u64, Value> {
    text(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("valid JSON-RPC stdout"))
        .filter_map(|response| response["id"].as_u64().map(|id| (id, response)))
        .collect()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
