use std::{sync::Arc, time::Duration};

use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResult, Implementation, ListToolsResult,
        PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
        ToolsCapability,
    },
    service::{RequestContext, RoleServer, ServerInitializeError},
};

use crate::{
    VERSION,
    accessibility::{RuntimeConfig, SemanticRuntime},
    atspi_adapter::AtspiAdapter,
    contract::{SERVER_INSTRUCTIONS, tool_definitions},
    errors::{CliError, RuntimeError, ToolOutcome},
    runtime::{DesktopRuntime, tool_error_result},
    screenshot::ProductionScreenshotCoordinator,
    validation::validate_call,
};

#[derive(Debug)]
pub struct OpenComputerUseServer<R = SemanticRuntime<AtspiAdapter, ProductionScreenshotCoordinator>>
{
    runtime: R,
    mutation: tokio::sync::Mutex<()>,
}

impl<R> OpenComputerUseServer<R> {
    pub fn new(runtime: R) -> Self {
        Self {
            runtime,
            mutation: tokio::sync::Mutex::new(()),
        }
    }
}

impl<R: DesktopRuntime> ServerHandler for OpenComputerUseServer<R> {
    fn get_info(&self) -> ServerInfo {
        let mut tools = ToolsCapability::default();
        tools.list_changed = Some(false);
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools_with(tools)
                .build(),
        )
        .with_server_info(Implementation::new("open-computer-use", VERSION))
        .with_protocol_version(ProtocolVersion::V_2025_06_18)
        .with_instructions(SERVER_INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tool_definitions()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tool_definitions()
            .into_iter()
            .find(|tool| tool.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let arguments = request.arguments.unwrap_or_default();
        let structured = context.protocol_version().is_some_and(|version| {
            version == ProtocolVersion::V_2025_06_18
                || version == ProtocolVersion::V_2025_11_25
                || version == ProtocolVersion::V_2026_07_28
        });
        let call = match validate_call(&request.name, arguments) {
            Ok(call) => call,
            Err(error) => return Err(McpError::invalid_params(error.to_string(), None)),
        };
        let _mutation = if call.is_mutating() {
            Some(self.mutation.lock().await)
        } else {
            None
        };
        if context.ct.is_cancelled() {
            eprintln!("open-computer-use: queued tool call cancelled before execution");
            return Ok(for_protocol(
                tool_error_result(&RuntimeError::new(
                    "cancelled",
                    "tool call cancelled before execution",
                    ToolOutcome::NotStarted,
                    true,
                    "Retry the call if it is still needed.",
                )),
                structured,
            ));
        }
        let runtime_result = tokio::select! {
            result = self.runtime.execute(call) => result,
            () = context.ct.cancelled() => {
                eprintln!("open-computer-use: tool call cancelled");
                match tokio::time::timeout(Duration::from_secs(2), self.runtime.cleanup()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => eprintln!("open-computer-use: cancellation cleanup failed: {error}"),
                    Err(_) => {
                        eprintln!("open-computer-use: cancellation cleanup timed out; shutting down the desktop session");
                        if let Err(error) = self.runtime.shutdown().await {
                            eprintln!("open-computer-use: cancellation shutdown failed: {error}");
                        }
                    }
                }
                let error = RuntimeError::new(
                    "cancelled",
                    "tool call cancelled while execution was active",
                    ToolOutcome::Unknown,
                    false,
                    "Call observe and inspect current state before deciding whether another action is needed.",
                );
                return Ok(for_protocol(tool_error_result(&error), structured));
            }
        };
        match runtime_result {
            Ok(output) => Ok(for_protocol(output.into_mcp_result(), structured)),
            Err(error) => {
                eprintln!("open-computer-use: {error}");
                Ok(for_protocol(tool_error_result(&error), structured))
            }
        }
    }
}

pub async fn serve_stdio() -> Result<(), CliError> {
    let runtime = production_runtime();
    let service = match OpenComputerUseServer::new(Arc::clone(&runtime))
        .serve(rmcp::transport::stdio())
        .await
    {
        Ok(service) => service,
        // An MCP host may close stdin while starting or stopping the child.
        // No request was accepted, so this is a clean shutdown rather than a server failure.
        Err(ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
        Err(error) => {
            return Err(CliError::Mcp(format!(
                "failed to start MCP stdio server: {error}"
            )));
        }
    };
    let result =
        service.waiting().await.map(|_| ()).map_err(|error| {
            CliError::Mcp(format!("MCP stdio server stopped with an error: {error}"))
        });
    if let Err(error) = runtime.shutdown().await {
        eprintln!("open-computer-use: shutdown cleanup failed: {error}");
    }
    result
}

pub fn production_runtime() -> Arc<SemanticRuntime<AtspiAdapter, ProductionScreenshotCoordinator>> {
    Arc::new(SemanticRuntime::with_screenshot_provider(
        AtspiAdapter::default(),
        ProductionScreenshotCoordinator::default(),
        RuntimeConfig::default(),
    ))
}

fn for_protocol(mut result: CallToolResult, structured: bool) -> CallToolResult {
    if !structured {
        result.structured_content = None;
    }
    result
}
