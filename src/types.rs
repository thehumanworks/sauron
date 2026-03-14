use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const DEFAULT_VIEWPORT_WIDTH: u32 = 1440;
pub const DEFAULT_VIEWPORT_HEIGHT: u32 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: DEFAULT_VIEWPORT_WIDTH,
            height: DEFAULT_VIEWPORT_HEIGHT,
        }
    }
}

// --- Output contract ---
// Every browser command emits exactly one of these as JSON to stdout.

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseMeta {
    pub request_id: String,
    pub timestamp: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<ResponseSessionMeta>,
}

impl ResponseMeta {
    pub fn new(
        request_id: impl Into<String>,
        timestamp: impl Into<String>,
        duration_ms: u64,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            timestamp: timestamp.into(),
            duration_ms,
            session: None,
        }
    }

    pub fn with_session(mut self, session: ResponseSessionMeta) -> Self {
        self.session = Some(session);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseSessionMeta {
    pub session_id: String,
    pub instance_id: String,
    pub client_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Input,
    State,
    Timeout,
    Navigation,
    Infrastructure,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryStrategy {
    AfterCommand,
    AfterDelay,
    Manual,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryHint {
    pub retryable: bool,
    pub after_ms: u64,
    pub strategy: RetryStrategy,
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryHint {
    pub action: String,
    pub steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandResult<T>
where
    T: Serialize,
{
    pub ok: bool,
    pub command: String,
    pub data: T,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEnvelope {
    pub code: ErrorCode,
    pub message: String,
    pub hint: String,
    pub recoverable: bool,
    pub exit_code: i32,
    pub category: ErrorCategory,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryHint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoveryHint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorResult {
    pub ok: bool,
    pub command: String,
    pub error: ErrorEnvelope,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum EnvelopeResult<T>
where
    T: Serialize,
{
    Ok(CommandResult<T>),
    Err(ErrorResult),
}

#[derive(Debug, Clone, Serialize)]
pub struct ResultEnvelope<T>
where
    T: Serialize,
{
    pub meta: ResponseMeta,
    pub result: EnvelopeResult<T>,
}

impl<T> ResultEnvelope<T>
where
    T: Serialize,
{
    pub fn success(command: impl Into<String>, data: T, meta: ResponseMeta) -> Self {
        Self {
            meta,
            result: EnvelopeResult::Ok(CommandResult {
                ok: true,
                command: command.into(),
                data,
            }),
        }
    }

    pub fn error(command: impl Into<String>, error: ErrorEnvelope, meta: ResponseMeta) -> Self {
        Self {
            meta,
            result: EnvelopeResult::Err(ErrorResult {
                ok: false,
                command: command.into(),
                error,
            }),
        }
    }
}

// --- Snapshot types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotRef {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Stored for compatibility with the TS version (Puppeteer locator string).
    /// The Rust version resolves refs via the accessibility tree and may ignore this field.
    pub locator: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistedRefState {
    pub snapshot_id: u64,
    pub url: String,
    pub last_snapshot: String,
    pub refs: HashMap<String, SnapshotRef>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct SnapshotOptions {
    pub interactive: bool,
    pub clickable: bool,
    pub scope: Option<String>,
    pub include_iframes: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotResult {
    pub tree: String,
    pub refs: HashMap<String, SnapshotRef>,
    pub url: String,
    pub snapshot_id: u64,
}

// --- Error codes ---

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    // Navigation
    NavTimeout,
    NavNetwork,

    // Element targeting
    RefStale,
    RefNotFound,
    ElementNotFound,
    ElementNotVisible,
    ElementObscured,
    ElementAmbiguous,
    ElementNotInteractive,

    // Control flow
    Timeout,
    WaitTimeout,

    // Infrastructure
    DaemonDown,
    ChromeCrashed,

    // Input
    BadInput,

    // Runtime session lifecycle
    SessionRequired,
    SessionInvalid,
    SessionTerminated,
    SessionConflict,

    Unknown,
}

// --- Daemon types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PidFileData {
    pub pid: u32,
    pub port: u16,

    /// Optional PID for an auxiliary Xvfb process (Linux only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xvfb_pid: Option<u32>,

    /// DISPLAY used when running Chrome under Xvfb (e.g. ":99").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum DaemonStatus {
    Running {
        pid: Option<u32>,
        port: u16,
        ws_url: Option<String>,
    },
    Stopped,
    Stale {
        pid: Option<u32>,
        port: Option<u16>,
    },
}
