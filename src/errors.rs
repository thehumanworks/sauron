use crate::types::{CommandResult, ErrorCode, ErrorEnvelope, ErrorResult, ResultEnvelope};
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct CliError {
    pub code: ErrorCode,
    pub message: String,
    pub hint: String,
    pub recoverable: bool,
    pub exit_code: i32,
}

impl CliError {
    pub fn new(
        code: ErrorCode,
        message: impl Into<String>,
        hint: impl Into<String>,
        recoverable: bool,
        exit_code: i32,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            hint: hint.into(),
            recoverable,
            exit_code,
        }
    }

    pub fn bad_input(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadInput, message, hint, false, 4)
    }

    pub fn daemon_down(message: impl Into<String>, hint: impl Into<String>) -> Self {
        // Match TS version: exit code 3 for daemon down
        Self::new(ErrorCode::DaemonDown, message, hint, false, 3)
    }

    pub fn timeout(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(ErrorCode::Timeout, message, hint, true, 1)
    }

    pub fn session_required(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(ErrorCode::SessionRequired, message, hint, false, 6)
    }

    pub fn session_invalid(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(ErrorCode::SessionInvalid, message, hint, false, 6)
    }

    pub fn session_terminated(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(ErrorCode::SessionTerminated, message, hint, false, 6)
    }

    pub fn unknown(message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unknown, message, hint, false, 1)
    }

    pub fn to_error_result(&self, command: &str) -> ErrorResult {
        ErrorResult {
            ok: false,
            command: command.to_string(),
            error: ErrorEnvelope {
                code: self.code,
                message: self.message.clone(),
                hint: self.hint.clone(),
                recoverable: self.recoverable,
            },
        }
    }
}

pub fn make_success<T: Serialize>(command: &str, data: T) -> ResultEnvelope<T> {
    ResultEnvelope::Ok(CommandResult {
        ok: true,
        command: command.to_string(),
        data,
    })
}

pub fn make_error(command: &str, err: &CliError) -> ResultEnvelope<serde_json::Value> {
    ResultEnvelope::Err(err.to_error_result(command))
}

pub fn print_result<T: Serialize>(result: &ResultEnvelope<T>) {
    // Always emit exactly one JSON object
    let s = serde_json::to_string(result).unwrap_or_else(|e| {
        // Last-resort fallback; keep it JSON.
        format!(
            "{{\"ok\":false,\"command\":\"internal\",\"error\":{{\"code\":\"UNKNOWN\",\"message\":{},\"hint\":\"Serialization error\",\"recoverable\":false}}}}",
            serde_json::to_string(&e.to_string()).unwrap_or_else(|_| "\"serialization error\"".to_string())
        )
    });
    println!("{}", s);
}
