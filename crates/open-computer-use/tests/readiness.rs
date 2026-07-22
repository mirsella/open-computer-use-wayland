use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{ColorType, GenericImageView, ImageEncoder, ImageFormat, codecs::png::PngEncoder};
use open_computer_use::{
    contract::TOOL_NAMES,
    errors::{RuntimeError, ToolOutcome},
    runtime::{DesktopRuntime, ToolOutput},
    server::OpenComputerUseServer,
    validation::{
        ApplicationScope, ElementAction, KeyboardAction, KeyboardFocus, ObservationView,
        PointerAction, ToolCall,
    },
};
use rmcp::{
    RoleClient, ServiceExt,
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::{IntoTransport, Transport},
};
use serde_json::{Value, json};

const RUNTIME_ERROR_TEXT: &str = "fake runtime unavailable\nCode: fake_runtime_unavailable\nOutcome: not_started\nRetryable: true\nRecovery: Call observe for current state, then retry only if the requested action is still needed.";

#[derive(Clone)]
struct FakeRuntime {
    state: Arc<FakeRuntimeState>,
}

struct FakeRuntimeState {
    calls: Mutex<Vec<ToolCall>>,
    fail_next: AtomicBool,
    png_base64: String,
    shutdowns: AtomicUsize,
}

impl FakeRuntime {
    fn new(png_base64: String) -> Self {
        Self {
            state: Arc::new(FakeRuntimeState {
                calls: Mutex::new(Vec::new()),
                fail_next: AtomicBool::new(false),
                png_base64,
                shutdowns: AtomicUsize::new(0),
            }),
        }
    }

    fn calls(&self) -> Vec<ToolCall> {
        self.state.calls.lock().expect("fake calls lock").clone()
    }

    fn fail_next(&self) {
        self.state.fail_next.store(true, Ordering::Release);
    }

    fn shutdowns(&self) -> usize {
        self.state.shutdowns.load(Ordering::Acquire)
    }
}

impl DesktopRuntime for FakeRuntime {
    fn execute(
        &self,
        call: ToolCall,
    ) -> impl std::future::Future<Output = Result<ToolOutput, RuntimeError>> + Send + '_ {
        self.state
            .calls
            .lock()
            .expect("fake calls lock")
            .push(call.clone());
        let fail = self.state.fail_next.swap(false, Ordering::AcqRel);
        let png_base64 =
            matches!(call, ToolCall::Observe { .. }).then(|| self.state.png_base64.clone());
        async move {
            if fail {
                return Err(RuntimeError::new(
                    "fake_runtime_unavailable",
                    "fake runtime unavailable",
                    ToolOutcome::NotStarted,
                    true,
                    "Call observe for current state, then retry only if the requested action is still needed.",
                ));
            }
            let output = ToolOutput::text("fake runtime success")
                .with_structured_content(json!({"status": "fake"}));
            Ok(match png_base64 {
                Some(png_base64) => output.with_png_base64(png_base64),
                None => output,
            })
        }
    }

    async fn cleanup(&self) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn shutdown(&self) -> impl std::future::Future<Output = Result<(), RuntimeError>> + Send + '_ {
        self.state.shutdowns.fetch_add(1, Ordering::AcqRel);
        async { Ok(()) }
    }
}

#[tokio::test]
async fn mcp_agent_path_dispatches_every_tool_and_preserves_error_boundaries() {
    let png = test_png();
    let runtime = FakeRuntime::new(STANDARD.encode(&png));
    let server_runtime = runtime.clone();
    let (server_transport, client_transport) = tokio::io::duplex(16 * 1024);
    let server = tokio::spawn(async move {
        let service = OpenComputerUseServer::<FakeRuntime>::new(Arc::new(server_runtime.clone()))
            .serve(server_transport)
            .await
            .expect("initialize server");
        let waiting = service.waiting().await;
        let shutdown = server_runtime.shutdown().await;
        waiting.expect("wait for server");
        shutdown.expect("shut down fake runtime");
    });
    let mut client = IntoTransport::<RoleClient, _, _>::into_transport(client_transport);

    client
        .send(message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "readiness-test", "version": "0.0.0"},
            },
        })))
        .await
        .expect("send initialize");
    let initialized = response_value(client.receive().await.expect("initialize response"));
    assert_eq!(initialized["id"], 1);
    assert_eq!(
        initialized["result"]["serverInfo"]["name"],
        "open-computer-use"
    );
    client
        .send(message(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        })))
        .await
        .expect("send initialized notification");

    client
        .send(message(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
        })))
        .await
        .expect("send tools/list");
    let listed = response_value(client.receive().await.expect("tools/list response"));
    let listed_names: Vec<_> = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    assert_eq!(listed_names, TOOL_NAMES);

    let calls = valid_tool_calls();
    let mut observe_response = None;
    for (offset, (name, arguments)) in calls.into_iter().enumerate() {
        let id = 10 + offset as u64;
        client
            .send(message(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            })))
            .await
            .expect("send valid tool call");
        let response = response_value(client.receive().await.expect("valid tool response"));
        assert_eq!(response["id"], id, "response for {name}");
        assert_success(&response, name);
        if name == "observe" {
            observe_response = Some(response);
        }
    }

    assert_eq!(runtime.calls(), expected_tool_calls());
    assert_png_content(
        observe_response
            .as_ref()
            .expect("observe response must be present"),
        &png,
    );

    for (id, name, arguments) in [
        (30, "not_a_tool", json!({})),
        (
            31,
            "pointer",
            json!({"state_id": "s-0123456789abcdef", "action": {}}),
        ),
    ] {
        client
            .send(message(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            })))
            .await
            .expect("send invalid tool call");
        let response = response_value(client.receive().await.expect("protocol error response"));
        assert_eq!(response["id"], id);
        assert_eq!(response["error"]["code"], -32602);
        assert!(response.get("result").is_none());
    }
    assert_eq!(runtime.calls(), expected_tool_calls());

    runtime.fail_next();
    client
        .send(message(json!({
            "jsonrpc": "2.0",
            "id": 32,
            "method": "tools/call",
            "params": {"name": "list_applications", "arguments": {"scope": "running"}},
        })))
        .await
        .expect("send runtime failure call");
    let runtime_error = response_value(client.receive().await.expect("runtime error response"));
    assert_eq!(runtime_error["id"], 32);
    assert_eq!(
        runtime_error["result"],
        json!({
            "content": [{"type": "text", "text": RUNTIME_ERROR_TEXT}],
            "isError": true,
            "structuredContent": runtime_error_structured_content(),
        })
    );
    assert!(runtime_error.get("error").is_none());

    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server should stop when the transport closes")
        .expect("join server");
    assert_eq!(runtime.shutdowns(), 1);
}

#[tokio::test]
async fn structured_content_is_gated_by_protocol_version_at_one_return_point() {
    for (protocol_version, expects_structured_content) in [
        ("2024-11-05", false),
        ("2025-03-26", false),
        ("2025-11-25", true),
    ] {
        let (success, response) = results_for_protocol(protocol_version).await;
        assert_eq!(
            success["result"].get("structuredContent"),
            expects_structured_content.then_some(&json!({"status": "fake"})),
            "successful result for protocol {protocol_version}"
        );
        let result = &response["result"];
        let text = result["content"][0]["text"]
            .as_str()
            .expect("runtime error text");

        assert_eq!(text, RUNTIME_ERROR_TEXT, "protocol {protocol_version}");
        assert_eq!(result["isError"], true, "protocol {protocol_version}");
        assert_eq!(
            result.get("structuredContent"),
            expects_structured_content.then_some(&runtime_error_structured_content()),
            "protocol {protocol_version}"
        );
        assert!(
            response.get("error").is_none(),
            "protocol {protocol_version}"
        );
    }
}

async fn results_for_protocol(protocol_version: &str) -> (Value, Value) {
    let runtime = FakeRuntime::new(STANDARD.encode(test_png()));
    let server_runtime = runtime.clone();
    let (server_transport, client_transport) = tokio::io::duplex(8 * 1024);
    let server = tokio::spawn(async move {
        let service = OpenComputerUseServer::<FakeRuntime>::new(Arc::new(server_runtime.clone()))
            .serve(server_transport)
            .await
            .expect("initialize server");
        let waiting = service.waiting().await;
        let shutdown = server_runtime.shutdown().await;
        waiting.expect("wait for server");
        shutdown.expect("shut down fake runtime");
    });
    let mut client = IntoTransport::<RoleClient, _, _>::into_transport(client_transport);

    client
        .send(message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": protocol_version,
                "capabilities": {},
                "clientInfo": {"name": "readiness-protocol-test", "version": "0.0.0"},
            },
        })))
        .await
        .expect("send initialize");
    let initialized = response_value(client.receive().await.expect("initialize response"));
    assert_eq!(
        initialized["result"]["protocolVersion"], protocol_version,
        "protocol negotiation"
    );
    client
        .send(message(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
        })))
        .await
        .expect("send initialized notification");

    client
        .send(message(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "list_applications", "arguments": {"scope": "running"}},
        })))
        .await
        .expect("send successful runtime call");
    let success = response_value(client.receive().await.expect("runtime success response"));
    assert_eq!(success["id"], 2);

    runtime.fail_next();
    client
        .send(message(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "list_applications", "arguments": {"scope": "running"}},
        })))
        .await
        .expect("send runtime failure call");
    let response = response_value(client.receive().await.expect("runtime error response"));
    assert_eq!(response["id"], 3);

    drop(client);
    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("server should stop when the transport closes")
        .expect("join server");
    assert_eq!(runtime.shutdowns(), 1);
    (success, response)
}

fn runtime_error_structured_content() -> Value {
    json!({
        "code": "fake_runtime_unavailable",
        "message": "fake runtime unavailable",
        "outcome": "not_started",
        "retryable": true,
        "recovery": "Call observe for current state, then retry only if the requested action is still needed.",
    })
}

fn valid_tool_calls() -> Vec<(&'static str, Value)> {
    vec![
        ("list_applications", json!({"scope": "running"})),
        (
            "launch_application",
            json!({"desktop_id": "org.example.Music.desktop"}),
        ),
        (
            "observe",
            json!({
                "target": " Editor ",
                "view": "visible",
                "query": " button ",
            }),
        ),
        (
            "act_on_element",
            json!({
                "state_id": "s-0123456789abcdef",
                "element_id": "007",
                "action": {"type": "named", "name": " show menu "},
            }),
        ),
        (
            "pointer",
            json!({
                "state_id": "s-0123456789abcdef",
                "action": {"type": "scroll", "x": 10.5, "y": 20.25, "direction": "right", "steps": 3},
            }),
        ),
        (
            "keyboard",
            json!({
                "state_id": "s-0123456789abcdef",
                "focus": {"element_id": "7"},
                "action": {"type": "type", "text": "hello\nworld"},
            }),
        ),
    ]
}

fn expected_tool_calls() -> Vec<ToolCall> {
    vec![
        ToolCall::ListApplications {
            scope: ApplicationScope::Running,
        },
        ToolCall::LaunchApplication {
            desktop_id: "org.example.Music.desktop".to_owned(),
        },
        ToolCall::Observe {
            target: "Editor".to_owned(),
            view: ObservationView::Visible,
            query: Some("button".to_owned()),
            text_limit: None,
            max_tree_nodes: None,
            max_tree_depth: None,
        },
        ToolCall::ActOnElement {
            state_id: "s-0123456789abcdef".to_owned(),
            element_id: "007".to_owned(),
            action: ElementAction::Named("show menu".to_owned()),
        },
        ToolCall::Pointer {
            state_id: "s-0123456789abcdef".to_owned(),
            action: PointerAction::Scroll {
                x: 10.5,
                y: 20.25,
                delta_x: 360,
                delta_y: 0,
            },
        },
        ToolCall::Keyboard {
            state_id: "s-0123456789abcdef".to_owned(),
            focus: KeyboardFocus::Element("7".to_owned()),
            action: KeyboardAction::Type("hello\nworld".to_owned()),
        },
    ]
}

fn assert_success(response: &Value, name: &str) {
    assert!(response.get("error").is_none(), "{name}: {response}");
    assert_eq!(response["result"]["isError"], false, "{name}: {response}");
    assert_eq!(
        response["result"]["content"][0],
        json!({"type": "text", "text": "fake runtime success"}),
        "{name}: {response}"
    );
    assert_eq!(
        response["result"]["structuredContent"],
        json!({"status": "fake"}),
        "{name}: {response}"
    );
}

fn assert_png_content(response: &Value, expected_png: &[u8]) {
    let content = response["result"]["content"]
        .as_array()
        .expect("standard MCP content array");
    assert_eq!(content.len(), 2);
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["mimeType"], "image/png");
    let decoded = STANDARD
        .decode(content[1]["data"].as_str().expect("base64 image data"))
        .expect("decode image content");
    assert_eq!(decoded, expected_png);
    let image = image::load_from_memory_with_format(&decoded, ImageFormat::Png)
        .expect("decode generated PNG");
    assert_eq!(image.dimensions(), (3, 2));
}

fn test_png() -> Vec<u8> {
    let pixels = [
        255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0, 255, 0, 255, 0, 255, 255,
    ];
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&pixels, 3, 2, ColorType::Rgb8.into())
        .expect("encode test PNG");
    png
}

fn message(value: Value) -> ClientJsonRpcMessage {
    serde_json::from_value(value).expect("valid client message")
}

fn response_value(message: ServerJsonRpcMessage) -> Value {
    serde_json::to_value(message).expect("serialize server message")
}
