use crate::types::{
    ErrorCategory, ErrorCode, ErrorEnvelope, RecoveryHint, ResponseMeta, ResultEnvelope, RetryHint,
    RetryStrategy,
};
use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct CliError {
    pub code: ErrorCode,
    pub message: String,
    pub hint: String,
    pub recoverable: bool,
    pub exit_code: i32,
    pub category: ErrorCategory,
    pub retry: Option<RetryHint>,
    pub recovery: Option<RecoveryHint>,
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
            category: default_error_category(code),
            retry: default_retry_hint(code, recoverable),
            recovery: default_recovery_hint(code),
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

    pub fn to_error_envelope(&self) -> ErrorEnvelope {
        ErrorEnvelope {
            code: self.code,
            message: self.message.clone(),
            hint: self.hint.clone(),
            recoverable: self.recoverable,
            exit_code: self.exit_code,
            category: self.category,
            retry: self.retry.clone(),
            recovery: self.recovery.clone(),
        }
    }
}

fn default_error_category(code: ErrorCode) -> ErrorCategory {
    match code {
        ErrorCode::BadInput => ErrorCategory::Input,
        ErrorCode::RefStale
        | ErrorCode::RefNotFound
        | ErrorCode::ElementNotFound
        | ErrorCode::ElementNotVisible
        | ErrorCode::ElementObscured
        | ErrorCode::ElementAmbiguous
        | ErrorCode::ElementNotInteractive
        | ErrorCode::SessionRequired
        | ErrorCode::SessionInvalid
        | ErrorCode::SessionTerminated
        | ErrorCode::SessionConflict => ErrorCategory::State,
        ErrorCode::NavTimeout | ErrorCode::Timeout | ErrorCode::WaitTimeout => {
            ErrorCategory::Timeout
        }
        ErrorCode::NavNetwork => ErrorCategory::Navigation,
        ErrorCode::DaemonDown | ErrorCode::ChromeCrashed => ErrorCategory::Infrastructure,
        ErrorCode::Unknown => ErrorCategory::Unknown,
    }
}

fn default_retry_hint(code: ErrorCode, recoverable: bool) -> Option<RetryHint> {
    if !recoverable {
        return None;
    }

    let (after_ms, strategy, requires) = match code {
        ErrorCode::RefStale => (
            0,
            RetryStrategy::AfterCommand,
            vec!["page.snapshot".to_string()],
        ),
        ErrorCode::NavNetwork => (250, RetryStrategy::AfterDelay, Vec::new()),
        ErrorCode::NavTimeout | ErrorCode::Timeout | ErrorCode::WaitTimeout => {
            (0, RetryStrategy::AfterCommand, Vec::new())
        }
        _ => (0, RetryStrategy::Manual, Vec::new()),
    };

    Some(RetryHint {
        retryable: true,
        after_ms,
        strategy,
        requires,
    })
}

fn default_recovery_hint(code: ErrorCode) -> Option<RecoveryHint> {
    let hint = match code {
        ErrorCode::RefStale => RecoveryHint {
            action: "resnapshot".to_string(),
            steps: vec![
                "Run snapshot to refresh refs".to_string(),
                "Re-resolve target with the new ref id".to_string(),
            ],
        },
        ErrorCode::SessionRequired | ErrorCode::SessionInvalid | ErrorCode::SessionTerminated => {
            RecoveryHint {
                action: "reacquire-session".to_string(),
                steps: vec![
                    "Start or auto-ensure a runtime session".to_string(),
                    "Retry the command with --session when needed".to_string(),
                ],
            }
        }
        ErrorCode::NavTimeout | ErrorCode::WaitTimeout | ErrorCode::Timeout => RecoveryHint {
            action: "retry-after-delay".to_string(),
            steps: vec![
                "Wait for page/network to settle".to_string(),
                "Retry with a longer timeout".to_string(),
            ],
        },
        ErrorCode::DaemonDown | ErrorCode::ChromeCrashed => RecoveryHint {
            action: "reopen-page".to_string(),
            steps: vec![
                "Start runtime to reconnect to browser".to_string(),
                "Retry navigation and target resolution".to_string(),
            ],
        },
        _ => RecoveryHint {
            action: "manual-intervention".to_string(),
            steps: vec![
                "Inspect the command context".to_string(),
                "Retry with adjusted inputs".to_string(),
            ],
        },
    };
    Some(hint)
}

fn default_response_meta() -> ResponseMeta {
    ResponseMeta::new(Uuid::now_v7().to_string(), Utc::now().to_rfc3339(), 0)
}

pub fn make_success_with_meta<T: Serialize>(
    command: &str,
    data: T,
    meta: ResponseMeta,
) -> ResultEnvelope<T> {
    ResultEnvelope::success(command, data, meta)
}

pub fn make_success<T: Serialize>(command: &str, data: T) -> ResultEnvelope<T> {
    make_success_with_meta(command, data, default_response_meta())
}

pub fn make_error_with_meta(
    command: &str,
    err: &CliError,
    meta: ResponseMeta,
) -> ResultEnvelope<serde_json::Value> {
    ResultEnvelope::error(command, err.to_error_envelope(), meta)
}

pub fn make_error(command: &str, err: &CliError) -> ResultEnvelope<serde_json::Value> {
    make_error_with_meta(command, err, default_response_meta())
}

pub fn print_result<T: Serialize>(result: &ResultEnvelope<T>) {
    let s = serde_json::to_string(result).unwrap_or_else(|e| {
        let fallback = make_error_with_meta(
            "internal",
            &CliError::unknown(e.to_string(), "Serialization error"),
            default_response_meta(),
        );
        serde_json::to_string(&fallback).unwrap_or_else(|_| {
            json!({
                "meta": {
                    "requestId": "internal",
                    "timestamp": Utc::now().to_rfc3339(),
                    "durationMs": 0
                },
                "result": {
                    "ok": false,
                    "command": "internal",
                    "error": {
                        "code": "UNKNOWN",
                        "message": "serialization error",
                        "hint": "Serialization error",
                        "recoverable": false,
                        "exitCode": 1,
                        "category": "unknown"
                    }
                }
            })
            .to_string()
        })
    });
    println!("{}", s);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn success_envelope_uses_v2_shape() {
        let meta = ResponseMeta::new("req-1", "2026-01-01T00:00:00Z", 12);
        let result =
            make_success_with_meta("page.goto", json!({ "url": "https://example.com" }), meta);
        let value = serde_json::to_value(result).expect("success envelope should serialize");

        assert_eq!(value["meta"]["requestId"], json!("req-1"));
        assert_eq!(value["result"]["ok"], json!(true));
        assert_eq!(value["result"]["command"], json!("page.goto"));
        assert_eq!(value["result"]["data"]["url"], json!("https://example.com"));
    }

    #[test]
    fn error_envelope_includes_v2_error_fields() {
        let meta = ResponseMeta::new("req-2", "2026-01-01T00:00:00Z", 18);
        let error = CliError::timeout("Timed out waiting for selector", "Retry after page settles");
        let result = make_error_with_meta("page.wait", &error, meta);
        let value = serde_json::to_value(result).expect("error envelope should serialize");

        assert_eq!(value["result"]["ok"], json!(false));
        assert_eq!(value["result"]["command"], json!("page.wait"));
        assert_eq!(value["result"]["error"]["exitCode"], json!(1));
        assert_eq!(value["result"]["error"]["category"], json!("timeout"));
        assert_eq!(value["result"]["error"]["retry"]["retryable"], json!(true));
    }
}
