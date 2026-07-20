use std::{
    future,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use open_computer_use::{
    runtime::{DesktopRuntime, RuntimeFuture, ToolOutput},
    server::OpenComputerUseServer,
    validation::ToolCall,
};
use rmcp::{
    RoleClient, ServiceExt,
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::{IntoTransport, Transport},
};
use serde_json::{Value, json};

struct BlockingRuntime {
    calls: Arc<AtomicUsize>,
    cleanup_complete: Arc<AtomicBool>,
}

struct QueuedRuntime {
    calls: Arc<AtomicUsize>,
    release_first: Arc<tokio::sync::Notify>,
}

impl DesktopRuntime for QueuedRuntime {
    fn execute(&self, _call: ToolCall) -> RuntimeFuture<'_> {
        let first = self.calls.fetch_add(1, Ordering::AcqRel) == 0;
        Box::pin(async move {
            if first {
                self.release_first.notified().await;
            }
            Ok(ToolOutput::text("executed"))
        })
    }

    fn cleanup(&self) -> RuntimeFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

impl DesktopRuntime for BlockingRuntime {
    fn execute(&self, _call: ToolCall) -> RuntimeFuture<'_> {
        if self.calls.fetch_add(1, Ordering::AcqRel) == 0 {
            Box::pin(future::pending())
        } else {
            let clean = self.cleanup_complete.load(Ordering::Acquire);
            Box::pin(async move {
                if !clean {
                    return Err(open_computer_use::errors::RuntimeError::not_started(
                        "cleanup_incomplete",
                        "mutation started before cleanup barrier",
                    ));
                }
                Ok(ToolOutput::text("after cleanup"))
            })
        }
    }

    fn cleanup(&self) -> RuntimeFuture<'_, ()> {
        Box::pin(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            self.cleanup_complete.store(true, Ordering::Release);
            Ok(())
        })
    }
}

#[tokio::test]
async fn cancelled_runtime_call_emits_no_response_and_server_continues() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let cleanup_complete = Arc::new(AtomicBool::new(false));
    let server_cleanup = Arc::clone(&cleanup_complete);
    let calls = Arc::new(AtomicUsize::new(0));
    let server_calls = Arc::clone(&calls);
    let server = tokio::spawn(async move {
        let service = OpenComputerUseServer::new(BlockingRuntime {
            calls: server_calls,
            cleanup_complete: server_cleanup,
        })
        .serve(server_transport)
        .await
        .expect("initialize server");
        service.waiting().await.expect("wait for server");
    });
    let mut client = IntoTransport::<RoleClient, _, _>::into_transport(client_transport);

    client
        .send(message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "cancellation-test", "version": "0.0.0"},
            },
        })))
        .await
        .expect("send initialize");
    assert_eq!(
        response_id(client.receive().await.expect("initialize response")),
        Some(1)
    );

    for payload in [
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "keyboard", "arguments": keyboard_arguments()},
        }),
    ] {
        client.send(message(payload)).await.expect("send message");
    }
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while calls.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first mutation should start before cancellation");

    for payload in [
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 2, "reason": "test"},
        }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "keyboard", "arguments": keyboard_arguments()},
        }),
    ] {
        client.send(message(payload)).await.expect("send message");
    }

    let response = client.receive().await.expect("ping response");
    assert_eq!(response_id(response), Some(3));
    assert!(cleanup_complete.load(Ordering::Acquire));
    drop(client);
    server.await.expect("join server");
}

#[tokio::test]
async fn mutation_cancelled_while_queued_never_executes() {
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let calls = Arc::new(AtomicUsize::new(0));
    let release_first = Arc::new(tokio::sync::Notify::new());
    let server_calls = Arc::clone(&calls);
    let server_release = Arc::clone(&release_first);
    let server = tokio::spawn(async move {
        let service = OpenComputerUseServer::new(QueuedRuntime {
            calls: server_calls,
            release_first: server_release,
        })
        .serve(server_transport)
        .await
        .expect("initialize server");
        service.waiting().await.expect("wait for server");
    });
    let mut client = IntoTransport::<RoleClient, _, _>::into_transport(client_transport);

    client
        .send(message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "queued-cancellation-test", "version": "0.0.0"},
            },
        })))
        .await
        .unwrap();
    assert_eq!(response_id(client.receive().await.unwrap()), Some(1));
    for payload in [
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        tool_call(2),
    ] {
        client.send(message(payload)).await.unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while calls.load(Ordering::Acquire) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first mutation should start");

    client.send(message(tool_call(3))).await.unwrap();
    client
        .send(message(json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": 3, "reason": "queued test"},
        })))
        .await
        .unwrap();
    release_first.notify_one();
    assert_eq!(response_id(client.receive().await.unwrap()), Some(2));

    client.send(message(tool_call(4))).await.unwrap();
    assert_eq!(response_id(client.receive().await.unwrap()), Some(4));
    assert_eq!(calls.load(Ordering::Acquire), 2);
    drop(client);
    server.await.unwrap();
}

fn tool_call(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": "keyboard", "arguments": keyboard_arguments()},
    })
}

fn keyboard_arguments() -> Value {
    json!({
        "state_id": "s-0000000000000000",
        "focus": {"x": 10, "y": 20},
        "action": {"type": "press", "key": "Return"},
    })
}

fn message(value: Value) -> ClientJsonRpcMessage {
    serde_json::from_value(value).expect("valid client message")
}

fn response_id(message: ServerJsonRpcMessage) -> Option<u64> {
    serde_json::to_value(message).expect("serialize server message")["id"].as_u64()
}
