use crate::browser::PageClient;
use crate::context::{atomic_write, AppContext};
use crate::errors::CliError;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionData {
    pub version: u32,
    pub name: String,
    pub saved_at: String,
    pub url: String,
    pub cookies: Vec<Value>,
    /// origin -> (key -> value)
    pub local_storage: HashMap<String, HashMap<String, String>>,
}

fn validate_session_name(name: &str) -> Result<(), CliError> {
    let re = Regex::new(r"^[a-zA-Z0-9_-]+$").unwrap();
    if !re.is_match(name) {
        return Err(CliError::bad_input(
            format!("Invalid session name: \"{}\"", name),
            "Use only letters, numbers, hyphens, and underscores",
        ));
    }
    Ok(())
}

fn sessions_dir(ctx: &AppContext) -> PathBuf {
    ctx.sessions_dir.clone()
}

pub async fn save_session(
    ctx: &AppContext,
    name: &str,
    page: &PageClient,
) -> Result<SessionData, CliError> {
    validate_session_name(name)?;
    std::fs::create_dir_all(sessions_dir(ctx)).map_err(|e| {
        CliError::unknown(
            format!("Failed to create sessions dir: {}", e),
            "Check filesystem permissions",
        )
    })?;

    // Cookies
    let res = page
        .call(
            "Network.getAllCookies",
            json!({}),
            Duration::from_millis(10_000),
        )
        .await?;
    let cookies = res
        .get("cookies")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // localStorage
    let local_storage_val = page
        .eval(
            "(() => { const o = {}; for (let i = 0; i < window.localStorage.length; i++) { const k = window.localStorage.key(i); o[k] = window.localStorage.getItem(k); } return o; })()",
        )
        .await
        .unwrap_or(Value::Object(serde_json::Map::new()));

    let mut local: HashMap<String, String> = HashMap::new();
    if let Some(obj) = local_storage_val.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                local.insert(k.clone(), s.to_string());
            } else if v.is_null() {
                local.insert(k.clone(), "".to_string());
            } else {
                local.insert(k.clone(), v.to_string());
            }
        }
    }

    let current_url = page.url().await.unwrap_or_default();
    let origin = url::Url::parse(&current_url)
        .ok()
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_else(|| current_url.clone());

    let mut local_storage: HashMap<String, HashMap<String, String>> = HashMap::new();
    local_storage.insert(origin.clone(), local);

    let data = SessionData {
        version: 1,
        name: name.to_string(),
        saved_at: chrono::Utc::now().to_rfc3339(),
        url: current_url,
        cookies,
        local_storage,
    };

    let path = sessions_dir(ctx).join(format!("{}.json", name));
    let text = serde_json::to_string_pretty(&data)
        .map_err(|e| CliError::unknown(format!("Failed to serialize session JSON: {}", e), ""))?;

    atomic_write(&path, text.as_bytes())?;

    Ok(data)
}

pub async fn load_session(
    ctx: &AppContext,
    name: &str,
    page: &PageClient,
) -> Result<SessionData, CliError> {
    validate_session_name(name)?;

    let path = sessions_dir(ctx).join(format!("{}.json", name));
    let text = std::fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CliError::bad_input(
                format!("Session not found: {}", name),
                "Use 'sauron session list' to see available sessions",
            )
        } else {
            CliError::unknown(
                format!("Failed to read session file: {}", e),
                "Check filesystem permissions",
            )
        }
    })?;

    let data: SessionData = serde_json::from_str(&text).map_err(|e| {
        CliError::unknown(
            format!("Failed to parse session JSON: {}", e),
            "The session file may be corrupted",
        )
    })?;

    // Restore cookies
    let _ = page
        .call(
            "Network.setCookies",
            json!({ "cookies": data.cookies }),
            Duration::from_millis(10_000),
        )
        .await?;

    // Restore localStorage (same-origin only)
    let current_url = page.url().await.unwrap_or_default();
    let current_origin = url::Url::parse(&current_url)
        .ok()
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_else(|| current_url.clone());

    for (origin, storage) in &data.local_storage {
        if origin != &current_origin {
            continue;
        }
        let json_entries = serde_json::to_string(storage).unwrap_or_else(|_| "{}".to_string());
        let expr = format!(
            "(() => {{ const entries = {}; for (const [k,v] of Object.entries(entries)) {{ window.localStorage.setItem(k, v); }} }})()",
            json_entries
        );
        let _ = page.eval(&expr).await;
    }

    Ok(data)
}

pub async fn list_sessions(ctx: &AppContext) -> Result<Vec<SessionSummary>, CliError> {
    let dir = sessions_dir(ctx);
    let mut out: Vec<SessionSummary> = Vec::new();

    let rd = std::fs::read_dir(&dir);
    let mut rd = match rd {
        Ok(r) => r,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                return Ok(vec![]);
            }
            return Err(CliError::unknown(
                format!("Failed to read sessions dir: {}", e),
                "Check filesystem permissions",
            ));
        }
    };

    while let Some(Ok(entry)) = rd.next() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(data) = serde_json::from_str::<SessionData>(&text) {
                out.push(SessionSummary {
                    name: data.name,
                    saved_at: data.saved_at,
                    url: data.url,
                    cookie_count: data.cookies.len() as u64,
                });
            }
        }
    }

    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub name: String,
    pub saved_at: String,
    pub url: String,
    pub cookie_count: u64,
}

pub async fn delete_session(ctx: &AppContext, name: &str) -> Result<bool, CliError> {
    validate_session_name(name)?;
    let path = sessions_dir(ctx).join(format!("{}.json", name));
    match std::fs::remove_file(path) {
        Ok(_) => Ok(true),
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(false)
            } else {
                Err(CliError::unknown(
                    format!("Failed to delete session: {}", e),
                    "Check filesystem permissions",
                ))
            }
        }
    }
}
