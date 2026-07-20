use std::fmt::{self, Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError(pub String);

impl Display for ValidationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ValidationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOutcome {
    NotStarted,
    Unknown,
    Completed,
}

impl ToolOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Unknown => "unknown",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeError {
    pub code: &'static str,
    pub message: String,
    pub outcome: ToolOutcome,
    pub retryable: bool,
    pub recovery: String,
}

impl RuntimeError {
    pub fn new(
        code: &'static str,
        message: impl Into<String>,
        outcome: ToolOutcome,
        retryable: bool,
        recovery: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            outcome,
            retryable,
            recovery: recovery.into(),
        }
    }

    pub fn not_started(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(
            code,
            message,
            ToolOutcome::NotStarted,
            true,
            "Call observe for current state, then retry only if the requested action is still needed.",
        )
    }

    pub fn with_execution_status(
        mut self,
        outcome: ToolOutcome,
        retryable: bool,
        recovery: impl Into<String>,
    ) -> Self {
        self.outcome = outcome;
        self.retryable = retryable;
        self.recovery = recovery.into();
        self
    }
}

impl Display for RuntimeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RuntimeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    InvalidCommand(String),
    InvalidArguments(String),
    Mcp(String),
}

impl Display for CliError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCommand(command) => write!(formatter, "unknown command: {command}"),
            Self::InvalidArguments(message) | Self::Mcp(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for CliError {}
