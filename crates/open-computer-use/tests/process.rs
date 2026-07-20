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
fn direct_call_rejects_incomplete_keyboard_input_before_desktop_access() {
    let output = run(
        &["call", "-"],
        r#"{"name":"keyboard","arguments":{"state_id":"s-0000000000000000","focus":{"x":1,"y":2},"action":{"type":"press"}}}"#,
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(text(&output.stderr).contains("missing required argument \"key\""));
}

#[test]
fn mcp_initialize_list_and_generated_calls_require_prior_state() {
    let mut messages = initialization();
    messages.push(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
    }));
    for (offset, (name, arguments)) in valid_calls().into_iter().enumerate() {
        messages.push(json!({
            "jsonrpc": "2.0",
            "id": offset + 10,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }));
    }

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

    for id in 10..13 {
        let result = &responses[&id]["result"];
        assert_eq!(result["isError"], true, "response {id}: {result}");
        assert!(
            result["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("no observation is available")),
            "response {id}: {result}"
        );
    }

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
fn tool_argument_validation_is_invalid_params() {
    let mut messages = initialization();
    for (id, name, arguments) in [
        (3, "pointer", json!({"state_id": "s-0000000000000000"})),
        (
            4,
            "keyboard",
            json!({"state_id": "s-0000000000000000", "focus": {"x": 1, "y": 2}}),
        ),
        (
            5,
            "act_on_element",
            json!({"state_id": "s-0000000000000000", "element_id": "1"}),
        ),
    ] {
        messages.push(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }));
    }
    let output = run(&["mcp"], &json_lines(&messages));
    let responses = responses_by_id(&output);
    for id in 3..=5 {
        assert_eq!(responses[&id]["error"]["code"], -32602);
        assert!(responses[&id].get("result").is_none());
    }
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

fn valid_calls() -> Vec<(&'static str, Value)> {
    vec![
        (
            "act_on_element",
            json!({"state_id": "s-0000000000000000", "element_id": "1", "action": {"type": "invoke"}}),
        ),
        (
            "pointer",
            json!({"state_id": "s-0000000000000000", "action": {"type": "drag", "from_x": 1, "from_y": 2, "to_x": 3, "to_y": 4}}),
        ),
        (
            "keyboard",
            json!({"state_id": "s-0000000000000000", "focus": {"x": 10, "y": 20}, "action": {"type": "press", "key": "Return"}}),
        ),
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
