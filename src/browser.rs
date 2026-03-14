use crate::cdp::{CdpClient, CdpError, CdpEvent};
use crate::context::{atomic_write, AppContext};
use crate::errors::CliError;
use crate::snapshot::{serialize_tree, should_assign_ref, AxNode, AxState};
use crate::types::{PersistedRefState, SnapshotOptions};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

const CDP_TIMEOUT: Duration = Duration::from_millis(10_000);
const WAIT_POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy)]
pub enum ScreenshotQuality {
    Low,
    Medium,
    High,
}

impl ScreenshotQuality {
    fn format(self) -> &'static str {
        match self {
            Self::High => "png",
            Self::Low | Self::Medium => "jpeg",
        }
    }

    fn jpeg_quality(self) -> Option<u8> {
        match self {
            Self::High => None,
            Self::Medium => Some(82),
            Self::Low => Some(60),
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Self::High => "image/png",
            Self::Low | Self::Medium => "image/jpeg",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::High => "png",
            Self::Low | Self::Medium => "jpg",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScreenshotData {
    pub data: String,
    pub mime: String,
    pub extension: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticLocatorKind {
    Text,
    Role,
    Label,
    Placeholder,
    AltText,
    Title,
    TestId,
}

// --- Browser connection ---

#[derive(Clone)]
pub struct BrowserClient {
    cdp: CdpClient,
}

impl BrowserClient {
    pub async fn connect(port: u16) -> Result<Self, CliError> {
        // Reuse daemon HTTP endpoint to locate the websocket URL.
        let ws_url = crate::daemon::get_ws_url(port).await.map_err(|_| {
            CliError::daemon_down(
                format!("Could not connect to Chrome on port {}", port),
                "Run 'sauron runtime start' to start the Chrome daemon",
            )
        })?;

        let cdp = CdpClient::connect(&ws_url).await.map_err(|e| {
            CliError::daemon_down(
                format!("Could not connect to Chrome on port {}: {}", port, e),
                "Run 'sauron runtime start' to start the Chrome daemon",
            )
        })?;

        Ok(Self { cdp })
    }

    pub async fn get_targets(&self) -> Result<Vec<TargetInfo>, CliError> {
        let res = self
            .cdp
            .call("Target.getTargets", json!({}), None, CDP_TIMEOUT)
            .await
            .map_err(map_cdp_error("Target.getTargets"))?;

        let infos = res
            .get("targetInfos")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                CliError::unknown("Unexpected Target.getTargets response".to_string(), "")
            })?;

        let mut out: Vec<TargetInfo> = Vec::new();
        for info in infos {
            if let Ok(ti) = serde_json::from_value::<TargetInfo>(info.clone()) {
                out.push(ti);
            }
        }
        Ok(out)
    }

    pub async fn create_target(&self, url: &str) -> Result<String, CliError> {
        let res = self
            .cdp
            .call(
                "Target.createTarget",
                json!({ "url": url }),
                None,
                CDP_TIMEOUT,
            )
            .await
            .map_err(map_cdp_error("Target.createTarget"))?;

        let tid = res
            .get("targetId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CliError::unknown("Target.createTarget missing targetId", ""))?;
        Ok(tid.to_string())
    }

    pub async fn activate_target(&self, target_id: &str) -> Result<(), CliError> {
        let _ = self
            .cdp
            .call(
                "Target.activateTarget",
                json!({ "targetId": target_id }),
                None,
                CDP_TIMEOUT,
            )
            .await
            .map_err(map_cdp_error("Target.activateTarget"))?;
        Ok(())
    }

    pub async fn close_target(&self, target_id: &str) -> Result<(), CliError> {
        let _ = self
            .cdp
            .call(
                "Target.closeTarget",
                json!({ "targetId": target_id }),
                None,
                CDP_TIMEOUT,
            )
            .await
            .map_err(map_cdp_error("Target.closeTarget"))?;
        Ok(())
    }

    pub async fn attach_to_target(&self, target_id: &str) -> Result<String, CliError> {
        let res = self
            .cdp
            .call(
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
                None,
                CDP_TIMEOUT,
            )
            .await
            .map_err(map_cdp_error("Target.attachToTarget"))?;

        let sid = res
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| CliError::unknown("Target.attachToTarget missing sessionId", ""))?;
        Ok(sid.to_string())
    }

    #[allow(dead_code)]
    pub async fn get_active_page(&self) -> Result<PageClient, CliError> {
        let pages: Vec<TargetInfo> = self
            .get_targets()
            .await?
            .into_iter()
            .filter(|t| t.target_type == "page")
            .collect();

        // Pick first non-blank if possible
        let preferred = pages
            .iter()
            .find(|p| p.url != "about:blank")
            .cloned()
            .or_else(|| pages.first().cloned());

        let target = if let Some(t) = preferred {
            t
        } else {
            let tid = self.create_target("about:blank").await?;
            TargetInfo {
                target_id: tid,
                target_type: "page".to_string(),
                title: Some("".to_string()),
                url: "about:blank".to_string(),
                attached: false,
            }
        };

        let session_id = self.attach_to_target(&target.target_id).await?;
        let page = PageClient {
            cdp: self.cdp.clone(),
            session_id,
            target_id: target.target_id,
        };
        page.enable_default_domains().await?;
        Ok(page)
    }

    pub async fn get_page_for_context(&self, ctx: &AppContext) -> Result<PageClient, CliError> {
        ctx.ensure_instance_dirs()?;
        let _client_lock = ctx.acquire_client_lock()?;

        let pages: Vec<TargetInfo> = self
            .get_targets()
            .await?
            .into_iter()
            .filter(|t| t.target_type == "page")
            .collect();

        let mut selected_target_id = None;
        if let Some(binding) = load_client_target_binding(ctx)? {
            if pages.iter().any(|p| p.target_id == binding.target_id) {
                selected_target_id = Some(binding.target_id);
            }
        }

        let mut target_id = match selected_target_id {
            Some(id) => id,
            None => {
                // If there are already tabs, reuse an existing non-blank page to avoid spawning
                // a new tab on every fresh client context (agent-friendly + faster).
                let preferred = pages
                    .iter()
                    .find(|p| p.url != "about:blank")
                    .cloned()
                    .or_else(|| pages.first().cloned());

                let id = if let Some(t) = preferred {
                    t.target_id
                } else {
                    self.create_target("about:blank").await?
                };

                save_client_target_binding(
                    ctx,
                    &ClientTargetBinding {
                        target_id: id.clone(),
                    },
                )?;
                id
            }
        };

        let session_id = match self.attach_to_target(&target_id).await {
            Ok(id) => id,
            Err(_) => {
                target_id = self.create_target("about:blank").await?;
                save_client_target_binding(
                    ctx,
                    &ClientTargetBinding {
                        target_id: target_id.clone(),
                    },
                )?;
                self.attach_to_target(&target_id).await?
            }
        };
        let page = PageClient {
            cdp: self.cdp.clone(),
            session_id,
            target_id,
        };
        page.enable_default_domains().await?;
        Ok(page)
    }

    #[allow(dead_code)]
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<CdpEvent> {
        self.cdp.subscribe()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetInfo {
    pub target_id: String,
    #[serde(rename = "type")]
    pub target_type: String,
    pub title: Option<String>,
    pub url: String,
    #[serde(default)]
    pub attached: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientTargetBinding {
    target_id: String,
}

#[derive(Clone)]
pub struct PageClient {
    cdp: CdpClient,
    pub session_id: String,
    #[allow(dead_code)]
    pub target_id: String,
}

impl PageClient {
    pub(crate) async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CliError> {
        self.cdp
            .call(method, params, Some(self.session_id.as_str()), timeout)
            .await
            .map_err(map_cdp_error(method))
    }

    pub async fn enable_default_domains(&self) -> Result<(), CliError> {
        // Best-effort enabling; some domains may already be enabled.
        let _ = self.call("Page.enable", json!({}), CDP_TIMEOUT).await;
        let _ = self.call("Runtime.enable", json!({}), CDP_TIMEOUT).await;
        let _ = self.call("DOM.enable", json!({}), CDP_TIMEOUT).await;
        let _ = self.call("Network.enable", json!({}), CDP_TIMEOUT).await;
        let _ = self
            .call("Accessibility.enable", json!({}), CDP_TIMEOUT)
            .await;
        Ok(())
    }

    pub async fn url(&self) -> Result<String, CliError> {
        let res = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": "window.location.href",
                    "returnByValue": true,
                    "awaitPromise": true
                }),
                CDP_TIMEOUT,
            )
            .await?;
        let v = res
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        Ok(v.to_string())
    }

    #[allow(dead_code)]
    pub async fn title(&self) -> Result<String, CliError> {
        let res = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": "document.title",
                    "returnByValue": true,
                    "awaitPromise": true
                }),
                CDP_TIMEOUT,
            )
            .await?;
        let v = res
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        Ok(v.to_string())
    }

    pub async fn navigate(
        &self,
        url: &str,
        wait_until: &str,
        timeout: Duration,
    ) -> Result<NavigateOutcome, CliError> {
        let mut events = self.cdp.subscribe();
        let mut last_document_status: Option<i64> = None;

        let res = self
            .call("Page.navigate", json!({ "url": url }), timeout)
            .await?;

        if let Some(err_text) = res.get("errorText").and_then(|v| v.as_str()) {
            return Err(CliError::new(
                crate::types::ErrorCode::NavNetwork,
                format!("Navigation failed: {}", err_text),
                "Check the URL is correct and network is available",
                false,
                1,
            ));
        }

        // Wait until the chosen readiness condition.
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() >= deadline {
                return Err(CliError::new(
                    crate::types::ErrorCode::NavTimeout,
                    format!("Navigation timed out after {:?}", timeout),
                    "Try a different --until value or increase --timeout-ms",
                    true,
                    1,
                ));
            }

            // Capture document response status (best-effort)
            while let Ok(ev) = events.try_recv() {
                if ev.session_id.as_deref() != Some(self.session_id.as_str()) {
                    continue;
                }
                if ev.method == "Network.responseReceived" {
                    if let Some(params) = ev.params.as_object() {
                        let typ = params.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        if typ == "Document" {
                            if let Some(resp) = params.get("response") {
                                if let Some(status) = resp.get("status").and_then(|v| v.as_i64()) {
                                    last_document_status = Some(status);
                                }
                            }
                        }
                    }
                }
            }

            match wait_until {
                "load" => {
                    if self.document_ready_state().await? == DocumentReady::Complete {
                        break;
                    }
                }
                "domcontentloaded" => {
                    let rs = self.document_ready_state().await?;
                    if matches!(rs, DocumentReady::Interactive | DocumentReady::Complete) {
                        break;
                    }
                }
                "networkidle0" => {
                    if self
                        .wait_for_network_idle(
                            Duration::from_millis(500),
                            0,
                            deadline.saturating_duration_since(Instant::now()),
                        )
                        .await?
                    {
                        break;
                    }
                }
                "networkidle2" => {
                    if self
                        .wait_for_network_idle(
                            Duration::from_millis(500),
                            2,
                            deadline.saturating_duration_since(Instant::now()),
                        )
                        .await?
                    {
                        break;
                    }
                }
                _ => {
                    // fallback to load
                    if self.document_ready_state().await? == DocumentReady::Complete {
                        break;
                    }
                }
            }

            tokio::time::sleep(WAIT_POLL).await;
        }

        Ok(NavigateOutcome {
            status: last_document_status,
        })
    }

    async fn document_ready_state(&self) -> Result<DocumentReady, CliError> {
        let res = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": "document.readyState",
                    "returnByValue": true,
                    "awaitPromise": true
                }),
                CDP_TIMEOUT,
            )
            .await?;

        let s = res
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("loading");

        Ok(match s {
            "interactive" => DocumentReady::Interactive,
            "complete" => DocumentReady::Complete,
            _ => DocumentReady::Loading,
        })
    }

    async fn wait_for_network_idle(
        &self,
        idle_for: Duration,
        max_inflight: i64,
        max_wait: Duration,
    ) -> Result<bool, CliError> {
        let mut events = self.cdp.subscribe();
        let start = Instant::now();
        let mut inflight: i64 = 0;
        let mut last_activity = Instant::now();

        loop {
            let elapsed = Instant::now().duration_since(start);
            if elapsed > max_wait {
                return Ok(false);
            }

            let remaining = max_wait.saturating_sub(elapsed);
            let tick = Duration::from_millis(200).min(remaining);

            match tokio::time::timeout(tick, events.recv()).await {
                Ok(Ok(ev)) => {
                    if ev.session_id.as_deref() != Some(self.session_id.as_str()) {
                        // Ignore events from other sessions.
                    } else {
                        match ev.method.as_str() {
                            "Network.requestWillBeSent" => {
                                inflight += 1;
                                last_activity = Instant::now();
                            }
                            "Network.loadingFinished" | "Network.loadingFailed" => {
                                inflight -= 1;
                                if inflight < 0 {
                                    inflight = 0;
                                }
                                last_activity = Instant::now();
                            }
                            _ => {}
                        }
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                    // Skip lagged events.
                }
                Ok(Err(_)) => return Ok(false),
                Err(_) => {
                    // tick timeout — no new events.
                }
            }

            if inflight <= max_inflight && Instant::now().duration_since(last_activity) >= idle_for
            {
                return Ok(true);
            }
        }
    }

    pub async fn set_viewport(
        &self,
        width: u32,
        height: u32,
        mobile: bool,
    ) -> Result<(), CliError> {
        let _ = self
            .call(
                "Emulation.setDeviceMetricsOverride",
                json!({
                    "width": width,
                    "height": height,
                    "deviceScaleFactor": 1,
                    "mobile": mobile,
                }),
                CDP_TIMEOUT,
            )
            .await?;

        let _ = self
            .call(
                "Emulation.setTouchEmulationEnabled",
                json!({
                    "enabled": mobile,
                    "maxTouchPoints": if mobile { 5 } else { 0 }
                }),
                CDP_TIMEOUT,
            )
            .await;

        Ok(())
    }

    pub async fn capture_screenshot(
        &self,
        full_page: bool,
        quality: ScreenshotQuality,
    ) -> Result<ScreenshotData, CliError> {
        let mut base_params = json!({
            "format": quality.format(),
            "optimizeForSpeed": false,
        });
        if let Some(q) = quality.jpeg_quality() {
            base_params["quality"] = json!(q);
        }

        if full_page {
            // Get content size
            let metrics = self
                .call("Page.getLayoutMetrics", json!({}), CDP_TIMEOUT)
                .await?;
            let content = metrics
                .get("contentSize")
                .or_else(|| metrics.get("cssContentSize"))
                .cloned()
                .unwrap_or(Value::Null);
            let width = content
                .get("width")
                .and_then(|v| v.as_f64())
                .unwrap_or(1280.0);
            let height = content
                .get("height")
                .and_then(|v| v.as_f64())
                .unwrap_or(720.0);

            let mut params = base_params.clone();
            params["captureBeyondViewport"] = json!(true);
            params["clip"] =
                json!({ "x": 0, "y": 0, "width": width, "height": height, "scale": 1 });

            let res = self
                .call("Page.captureScreenshot", params, CDP_TIMEOUT)
                .await?;

            let data = res
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or_else(|| CliError::unknown("Page.captureScreenshot missing data", ""))?;
            Ok(ScreenshotData {
                data: data.to_string(),
                mime: quality.mime_type().to_string(),
                extension: quality.extension().to_string(),
            })
        } else {
            let res = self
                .call("Page.captureScreenshot", base_params, CDP_TIMEOUT)
                .await?;
            let data = res
                .get("data")
                .and_then(|v| v.as_str())
                .ok_or_else(|| CliError::unknown("Page.captureScreenshot missing data", ""))?;
            Ok(ScreenshotData {
                data: data.to_string(),
                mime: quality.mime_type().to_string(),
                extension: quality.extension().to_string(),
            })
        }
    }

    // --- Accessibility tree ---

    pub async fn accessibility_tree(&self) -> Result<AxNode, CliError> {
        let res = self
            .call("Accessibility.getFullAXTree", json!({}), CDP_TIMEOUT)
            .await?;

        let nodes = res
            .get("nodes")
            .and_then(|v| v.as_array())
            .ok_or_else(|| CliError::unknown("Accessibility.getFullAXTree missing nodes", ""))?;

        let mut by_id: HashMap<String, RawAxNode> = HashMap::new();
        let mut child_ids: HashSet<String> = HashSet::new();

        for n in nodes {
            if let Some(id) = n.get("nodeId").and_then(|v| v.as_str()) {
                let raw = RawAxNode::from_value(n.clone());
                if let Some(children) = &raw.child_ids {
                    for cid in children {
                        child_ids.insert(cid.clone());
                    }
                }
                by_id.insert(id.to_string(), raw);
            }
        }

        // root = node not referenced as a child.
        let root_id = by_id
            .keys()
            .find(|id| !child_ids.contains(*id))
            .cloned()
            .or_else(|| by_id.keys().next().cloned())
            .ok_or_else(|| CliError::unknown("Accessibility tree was empty", ""))?;

        let mut visiting = HashSet::new();
        Ok(build_ax_tree(&root_id, &by_id, &mut visiting))
    }

    // --- Element targeting + interaction ---

    pub async fn resolve_target_backend_node_id(
        &self,
        ctx: &AppContext,
        target: &str,
        text_nth: Option<u32>,
    ) -> Result<u64, CliError> {
        let t = target.trim();
        let is_ref = t.starts_with('@')
            || (t.as_bytes().first() == Some(&b'e')
                && t.len() > 1
                && t.as_bytes()[1..].iter().all(|b| b.is_ascii_digit()));
        if is_ref {
            let state = load_ref_state(ctx).await?;
            let Some(state) = state else {
                return Err(CliError::new(
                    crate::types::ErrorCode::BadInput,
                    "No refs available. Run 'sauron page snapshot' first.",
                    "Run 'sauron page snapshot' to get fresh refs",
                    false,
                    4,
                ));
            };
            let normalized = t.strip_prefix('@').unwrap_or(t);
            let Some(r) = state.refs.get(normalized) else {
                return Err(CliError::new(
                    crate::types::ErrorCode::RefNotFound,
                    format!(
                        "Ref @{} not found. Available refs: {}",
                        normalized,
                        state
                            .refs
                            .keys()
                            .map(|k| format!("@{}", k))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    "Run 'sauron page snapshot' to get fresh refs",
                    false,
                    4,
                ));
            };

            // Extract nth=... from locator if present.
            let nth = parse_locator_nth(&r.locator).unwrap_or(0);

            let root = self.accessibility_tree().await?;
            let mut matches: Vec<&AxNode> = Vec::new();
            collect_matching_refs(&root, &r.role, r.name.as_deref(), &mut matches);

            let node = matches.get(nth as usize).ok_or_else(|| {
                CliError::new(
                    crate::types::ErrorCode::RefStale,
                    format!(
                        "Ref @{} could not be resolved on the current page",
                        normalized
                    ),
                    "Run 'sauron page snapshot' to get fresh refs",
                    true,
                    1,
                )
            })?;

            let backend = node.backend_dom_node_id.ok_or_else(|| {
                CliError::new(
                    crate::types::ErrorCode::ElementNotInteractive,
                    "Resolved ref has no backend DOM node id".to_string(),
                    "Run 'sauron page snapshot' and choose an interactive element",
                    true,
                    1,
                )
            })?;
            return Ok(backend);
        }

        // Text targeting — best effort via accessibility names.
        let root = self.accessibility_tree().await?;
        let mut primary: Vec<&AxNode> = Vec::new();
        collect_text_matches(&root, t, true, &mut primary);
        let candidates = if primary.is_empty() {
            let mut secondary: Vec<&AxNode> = Vec::new();
            collect_text_matches(&root, t, false, &mut secondary);
            secondary
        } else {
            primary
        };

        if candidates.is_empty() {
            return Err(CliError::new(
                crate::types::ErrorCode::ElementNotFound,
                format!("No element found matching text: {}", t),
                "Run 'sauron page snapshot' to inspect available elements",
                true,
                1,
            ));
        }

        let idx = text_nth.unwrap_or(0) as usize;
        if idx >= candidates.len() {
            return Err(CliError::new(
                crate::types::ErrorCode::ElementAmbiguous,
                format!(
                    "Text match index {} out of range; found {} matches",
                    idx,
                    candidates.len()
                ),
                "Use --match-index <n> with a valid match index",
                true,
                1,
            ));
        }

        let node = candidates[idx];
        let backend = node.backend_dom_node_id.ok_or_else(|| {
            CliError::new(
                crate::types::ErrorCode::ElementNotInteractive,
                "Matched node has no backend DOM node id".to_string(),
                "Run 'sauron page snapshot' and choose an interactive element",
                true,
                1,
            )
        })?;
        Ok(backend)
    }

    #[allow(dead_code)]
    pub async fn resolve_selector_backend_node_id(
        &self,
        selector: &str,
        match_index: Option<u32>,
    ) -> Result<u64, CliError> {
        let selector = selector.trim();
        if selector.is_empty() {
            return Err(CliError::bad_input(
                "Selector cannot be empty",
                "Pass a non-empty CSS selector such as '#submit' or '.button.primary'",
            ));
        }

        let doc = self
            .call(
                "DOM.getDocument",
                json!({ "depth": 0, "pierce": true }),
                CDP_TIMEOUT,
            )
            .await?;
        let root_node_id = doc
            .get("root")
            .and_then(|v| v.get("nodeId"))
            .and_then(|v| v.as_i64())
            .ok_or_else(|| {
                CliError::unknown(
                    "DOM.getDocument returned no root node id",
                    "Retry after navigation completes",
                )
            })?;

        let query = self
            .cdp
            .call(
                "DOM.querySelectorAll",
                json!({
                    "nodeId": root_node_id,
                    "selector": selector,
                }),
                Some(self.session_id.as_str()),
                CDP_TIMEOUT,
            )
            .await
            .map_err(|err| map_selector_query_error(selector, err))?;

        let node_ids = query
            .get("nodeIds")
            .and_then(|v| v.as_array())
            .ok_or_else(|| CliError::unknown("DOM.querySelectorAll missing nodeIds", ""))?;

        if node_ids.is_empty() {
            return Err(CliError::new(
                crate::types::ErrorCode::ElementNotFound,
                format!("No element matched selector: {}", selector),
                "Adjust the selector or run 'sauron page snapshot' to inspect available elements",
                true,
                1,
            ));
        }

        let idx = match_index.unwrap_or(0) as usize;
        if idx >= node_ids.len() {
            return Err(CliError::new(
                crate::types::ErrorCode::ElementAmbiguous,
                format!(
                    "Selector '{}' matched {} elements; index {} is out of range",
                    selector,
                    node_ids.len(),
                    idx
                ),
                format!(
                    "Use --match-index between 0 and {}",
                    node_ids.len().saturating_sub(1)
                ),
                true,
                1,
            ));
        }

        let node_id = node_ids.get(idx).and_then(|v| v.as_i64()).ok_or_else(|| {
            CliError::unknown("DOM.querySelectorAll returned invalid node id", "")
        })?;

        let described = self
            .call(
                "DOM.describeNode",
                json!({ "nodeId": node_id }),
                CDP_TIMEOUT,
            )
            .await?;

        let backend_node_id = described
            .get("node")
            .and_then(|v| v.get("backendNodeId"))
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                CliError::new(
                    crate::types::ErrorCode::ElementNotInteractive,
                    format!(
                        "Selector '{}' resolved to a node without backend DOM id",
                        selector
                    ),
                    "Ensure the selector points to a live DOM element and retry",
                    true,
                    1,
                )
            })?;

        Ok(backend_node_id)
    }

    pub async fn count_semantic_matches(
        &self,
        kind: SemanticLocatorKind,
        query: &str,
        secondary: Option<&str>,
    ) -> Result<u32, CliError> {
        let count_expr = semantic_locator_expression(kind, query, secondary, None, true)?;
        let value = self.eval(&count_expr).await?;
        Ok(value_to_u32(Some(&value)).unwrap_or(0))
    }

    pub async fn resolve_semantic_backend_node_id(
        &self,
        kind: SemanticLocatorKind,
        query: &str,
        secondary: Option<&str>,
        nth: Option<u32>,
    ) -> Result<(u64, u32), CliError> {
        let candidate_count = self.count_semantic_matches(kind, query, secondary).await?;
        if candidate_count == 0 {
            return Err(CliError::new(
                crate::types::ErrorCode::ElementNotFound,
                format!(
                    "No element matched semantic locator kind={} query='{}'",
                    semantic_kind_name(kind),
                    query
                ),
                "Inspect the page snapshot and adjust the locator",
                true,
                1,
            ));
        }

        let index = nth.unwrap_or(0);
        if index >= candidate_count {
            return Err(CliError::new(
                crate::types::ErrorCode::ElementAmbiguous,
                format!(
                    "Semantic locator matched {} elements; index {} is out of range",
                    candidate_count, index
                ),
                format!(
                    "Use --nth between 0 and {}",
                    candidate_count.saturating_sub(1)
                ),
                true,
                1,
            ));
        }
        if nth.is_none() && candidate_count > 1 {
            return Err(CliError::new(
                crate::types::ErrorCode::ElementAmbiguous,
                format!("Semantic locator matched {} elements", candidate_count),
                "Refine the locator or use --nth to pick a specific match",
                true,
                1,
            ));
        }

        let target_expr = semantic_locator_expression(kind, query, secondary, Some(index), false)?;
        let resolved = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": target_expr,
                    "returnByValue": false,
                    "awaitPromise": true
                }),
                CDP_TIMEOUT,
            )
            .await?;

        let subtype = resolved
            .get("result")
            .and_then(|v| v.get("subtype"))
            .and_then(|v| v.as_str());
        if subtype == Some("null") {
            return Err(CliError::new(
                crate::types::ErrorCode::ElementNotFound,
                "Semantic locator returned null element".to_string(),
                "Try a more specific locator",
                true,
                1,
            ));
        }

        let object_id = resolved
            .get("result")
            .and_then(|v| v.get("objectId"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                CliError::unknown(
                    "Runtime.evaluate did not return objectId for semantic locator",
                    "Retry once; if it persists, capture a fresh snapshot",
                )
            })?;

        let described = self
            .call(
                "DOM.describeNode",
                json!({ "objectId": object_id }),
                CDP_TIMEOUT,
            )
            .await?;
        let backend_node_id = described
            .get("node")
            .and_then(|v| v.get("backendNodeId"))
            .and_then(|v| v.as_u64())
            .ok_or_else(|| {
                CliError::new(
                    crate::types::ErrorCode::ElementNotInteractive,
                    "Semantic locator resolved to a node without backend DOM id".to_string(),
                    "Retry after the page settles or choose another element",
                    true,
                    1,
                )
            })?;

        Ok((backend_node_id, candidate_count))
    }

    pub async fn scroll_into_view(&self, backend_node_id: u64) -> Result<(), CliError> {
        let _ = self
            .call(
                "DOM.scrollIntoViewIfNeeded",
                json!({ "backendNodeId": backend_node_id }),
                CDP_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    pub async fn click(&self, backend_node_id: u64, double: bool) -> Result<(), CliError> {
        self.scroll_into_view(backend_node_id).await.ok();
        let (x, y) = self.box_center(backend_node_id).await?;

        if double {
            self.mouse_click_sequence(x, y, 1).await?;
            self.mouse_click_sequence(x, y, 2).await?;
        } else {
            self.mouse_click_sequence(x, y, 1).await?;
        }
        Ok(())
    }

    async fn mouse_click_sequence(&self, x: f64, y: f64, click_count: i64) -> Result<(), CliError> {
        let _ = self
            .call(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseMoved", "x": x, "y": y }),
                CDP_TIMEOUT,
            )
            .await;
        let _ = self
            .call(
                "Input.dispatchMouseEvent",
                json!({ "type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": click_count }),
                CDP_TIMEOUT,
            )
            .await?;
        let _ = self
            .call(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": click_count }),
                CDP_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    pub async fn hover(&self, backend_node_id: u64) -> Result<(), CliError> {
        self.scroll_into_view(backend_node_id).await.ok();
        let (x, y) = self.box_center(backend_node_id).await?;
        let _ = self
            .call(
                "Input.dispatchMouseEvent",
                json!({ "type": "mouseMoved", "x": x, "y": y }),
                CDP_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    pub async fn focus(&self, backend_node_id: u64) -> Result<(), CliError> {
        let _ = self
            .call(
                "DOM.focus",
                json!({ "backendNodeId": backend_node_id }),
                CDP_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    pub async fn fill(&self, backend_node_id: u64, value: &str) -> Result<String, CliError> {
        self.scroll_into_view(backend_node_id).await.ok();
        self.focus(backend_node_id).await.ok();

        // Resolve node to objectId
        let resolved = self
            .call(
                "DOM.resolveNode",
                json!({ "backendNodeId": backend_node_id }),
                CDP_TIMEOUT,
            )
            .await?;
        let object_id = resolved
            .get("object")
            .and_then(|o| o.get("objectId"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| CliError::unknown("DOM.resolveNode missing objectId", ""))?;

        let func = r#"function(val) {
  const el = this;
  const tag = (el.tagName || '').toLowerCase();
  if (tag === 'select') {
    el.value = val;
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
    return 'select';
  }
  try {
    el.focus();
  } catch (_) {}
  // Clear then set
  try { el.value = ''; } catch (_) {}
  el.dispatchEvent(new Event('input', { bubbles: true }));
  try { el.value = val; } catch (_) {}
  el.dispatchEvent(new Event('input', { bubbles: true }));
  el.dispatchEvent(new Event('change', { bubbles: true }));
  return 'input';
}"#;

        let res = self
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "objectId": object_id,
                    "functionDeclaration": func,
                    "arguments": [{ "value": value }],
                    "returnByValue": true
                }),
                CDP_TIMEOUT,
            )
            .await?;

        let typ = res
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("input");

        Ok(typ.to_string())
    }

    async fn object_id_from_backend(&self, backend_node_id: u64) -> Result<String, CliError> {
        let resolved = self
            .call(
                "DOM.resolveNode",
                json!({ "backendNodeId": backend_node_id }),
                CDP_TIMEOUT,
            )
            .await?;
        resolved
            .get("object")
            .and_then(|o| o.get("objectId"))
            .and_then(|v| v.as_str())
            .map(|value| value.to_string())
            .ok_or_else(|| CliError::unknown("DOM.resolveNode missing objectId", ""))
    }

    async fn call_node_function(
        &self,
        backend_node_id: u64,
        function_declaration: &str,
        arguments: Vec<Value>,
    ) -> Result<Value, CliError> {
        let object_id = self.object_id_from_backend(backend_node_id).await?;
        let response = self
            .call(
                "Runtime.callFunctionOn",
                json!({
                    "objectId": object_id,
                    "functionDeclaration": function_declaration,
                    "arguments": arguments,
                    "returnByValue": true
                }),
                CDP_TIMEOUT,
            )
            .await?;

        Ok(response
            .get("result")
            .and_then(|value| value.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub async fn get_node_text(&self, backend_node_id: u64) -> Result<String, CliError> {
        let value = self
            .call_node_function(
                backend_node_id,
                "function() { return (this.innerText || this.textContent || '').trim(); }",
                vec![],
            )
            .await?;
        Ok(value.as_str().unwrap_or_default().to_string())
    }

    pub async fn get_node_html(&self, backend_node_id: u64) -> Result<String, CliError> {
        let value = self
            .call_node_function(
                backend_node_id,
                "function() { return this.outerHTML || ''; }",
                vec![],
            )
            .await?;
        Ok(value.as_str().unwrap_or_default().to_string())
    }

    pub async fn get_node_value(&self, backend_node_id: u64) -> Result<String, CliError> {
        let value = self
            .call_node_function(
                backend_node_id,
                "function() { return (this.value ?? '').toString(); }",
                vec![],
            )
            .await?;
        Ok(value.as_str().unwrap_or_default().to_string())
    }

    pub async fn get_node_attr(
        &self,
        backend_node_id: u64,
        name: &str,
    ) -> Result<Option<String>, CliError> {
        let value = self
            .call_node_function(
                backend_node_id,
                "function(attr) { const v = this.getAttribute ? this.getAttribute(attr) : null; return v === undefined ? null : v; }",
                vec![json!({ "value": name })],
            )
            .await?;
        if value.is_null() {
            Ok(None)
        } else {
            Ok(value.as_str().map(|v| v.to_string()))
        }
    }

    pub async fn is_node_visible(&self, backend_node_id: u64) -> Result<bool, CliError> {
        let value = self
            .call_node_function(
                backend_node_id,
                "function() { const rect = this.getBoundingClientRect ? this.getBoundingClientRect() : null; if (!rect) return false; const style = window.getComputedStyle(this); if (!style) return false; if (style.display === 'none' || style.visibility === 'hidden' || style.visibility === 'collapse') return false; return rect.width > 0 && rect.height > 0; }",
                vec![],
            )
            .await?;
        Ok(value.as_bool().unwrap_or(false))
    }

    pub async fn is_node_enabled(&self, backend_node_id: u64) -> Result<bool, CliError> {
        let value = self
            .call_node_function(
                backend_node_id,
                "function() { return !this.disabled; }",
                vec![],
            )
            .await?;
        Ok(value.as_bool().unwrap_or(false))
    }

    pub async fn is_node_checked(&self, backend_node_id: u64) -> Result<bool, CliError> {
        let value = self
            .call_node_function(
                backend_node_id,
                "function() { return !!this.checked; }",
                vec![],
            )
            .await?;
        Ok(value.as_bool().unwrap_or(false))
    }

    pub async fn set_node_checked(
        &self,
        backend_node_id: u64,
        checked: bool,
    ) -> Result<(), CliError> {
        let value = self
            .call_node_function(
                backend_node_id,
                "function(next) { if (this.checked === undefined) return false; this.checked = !!next; this.dispatchEvent(new Event('input', { bubbles: true })); this.dispatchEvent(new Event('change', { bubbles: true })); return true; }",
                vec![json!({ "value": checked })],
            )
            .await?;
        if value.as_bool().unwrap_or(false) {
            Ok(())
        } else {
            Err(CliError::new(
                crate::types::ErrorCode::ElementNotInteractive,
                "Target element does not support checked state".to_string(),
                "Use a checkbox or radio target",
                true,
                1,
            ))
        }
    }

    pub async fn select_node_value(
        &self,
        backend_node_id: u64,
        value: &str,
    ) -> Result<(), CliError> {
        let selected = self
            .call_node_function(
                backend_node_id,
                "function(nextValue) { if ((this.tagName || '').toLowerCase() !== 'select') return false; this.value = nextValue; this.dispatchEvent(new Event('input', { bubbles: true })); this.dispatchEvent(new Event('change', { bubbles: true })); return true; }",
                vec![json!({ "value": value })],
            )
            .await?;
        if selected.as_bool().unwrap_or(false) {
            Ok(())
        } else {
            Err(CliError::new(
                crate::types::ErrorCode::ElementNotInteractive,
                "Target element is not a <select>".to_string(),
                "Use select on a select dropdown element",
                true,
                1,
            ))
        }
    }

    pub async fn upload_files(
        &self,
        backend_node_id: u64,
        files: &[String],
    ) -> Result<(), CliError> {
        let _ = self
            .call(
                "DOM.setFileInputFiles",
                json!({
                    "backendNodeId": backend_node_id,
                    "files": files
                }),
                CDP_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    pub async fn print_to_pdf(&self) -> Result<String, CliError> {
        let response = self
            .call("Page.printToPDF", json!({}), Duration::from_millis(30_000))
            .await?;
        let data = response
            .get("data")
            .and_then(|value| value.as_str())
            .ok_or_else(|| CliError::unknown("Page.printToPDF missing data", ""))?;
        Ok(data.to_string())
    }

    pub async fn go_back(&self) -> Result<(), CliError> {
        self.navigate_history(-1).await
    }

    pub async fn go_forward(&self) -> Result<(), CliError> {
        self.navigate_history(1).await
    }

    pub async fn reload(&self) -> Result<(), CliError> {
        let _ = self.call("Page.reload", json!({}), CDP_TIMEOUT).await?;
        Ok(())
    }

    async fn navigate_history(&self, delta: i32) -> Result<(), CliError> {
        let history = self
            .call("Page.getNavigationHistory", json!({}), CDP_TIMEOUT)
            .await?;
        let current_index = history
            .get("currentIndex")
            .and_then(|value| value.as_i64())
            .ok_or_else(|| {
                CliError::unknown("Page.getNavigationHistory missing currentIndex", "")
            })?;
        let entries = history
            .get("entries")
            .and_then(|value| value.as_array())
            .ok_or_else(|| CliError::unknown("Page.getNavigationHistory missing entries", ""))?;

        let target_index = current_index + i64::from(delta);
        if target_index < 0 || target_index >= entries.len() as i64 {
            return Err(CliError::bad_input(
                "No navigation history entry for requested direction",
                "Navigate to more pages before using back/forward",
            ));
        }

        let entry_id = entries[target_index as usize]
            .get("id")
            .and_then(|value| value.as_i64())
            .ok_or_else(|| CliError::unknown("Navigation history entry missing id", ""))?;
        let _ = self
            .call(
                "Page.navigateToHistoryEntry",
                json!({ "entryId": entry_id }),
                CDP_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    pub async fn press_key(&self, combo: &str) -> Result<(), CliError> {
        let (modifiers, key_token) = parse_key_combo(combo);
        let key_info = key_to_cdp(&key_token);

        let _ = self
            .call(
                "Input.dispatchKeyEvent",
                json!({
                    "type": "keyDown",
                    "modifiers": modifiers,
                    "key": key_info.key,
                    "code": key_info.code,
                    "windowsVirtualKeyCode": key_info.vk,
                    "nativeVirtualKeyCode": key_info.vk,
                    "text": key_info.text
                }),
                CDP_TIMEOUT,
            )
            .await?;
        let _ = self
            .call(
                "Input.dispatchKeyEvent",
                json!({
                    "type": "keyUp",
                    "modifiers": modifiers,
                    "key": key_info.key,
                    "code": key_info.code,
                    "windowsVirtualKeyCode": key_info.vk,
                    "nativeVirtualKeyCode": key_info.vk
                }),
                CDP_TIMEOUT,
            )
            .await?;
        Ok(())
    }

    pub async fn eval(&self, expression: &str) -> Result<Value, CliError> {
        let res = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true
                }),
                CDP_TIMEOUT,
            )
            .await?;

        // Return result.value (if present) else a best-effort preview.
        if let Some(v) = res.get("result").and_then(|r| r.get("value")) {
            Ok(v.clone())
        } else {
            Ok(res.get("result").cloned().unwrap_or(Value::Null))
        }
    }

    /// Extract the visible page content as Markdown.
    ///
    /// Notes:
    /// - This uses a DOM-aware page-side extractor so headings and list-like card grids are
    ///   preserved instead of flattened into `innerText`.
    /// - Decorative chrome such as nav/aside regions and aria-hidden shimmer text is skipped.
    pub async fn extract_markdown(&self) -> Result<String, CliError> {
        let v = self.eval(markdown_extract_expression()).await?;
        if let Value::String(s) = v {
            return Ok(s);
        }

        let payload = serde_json::from_value::<MarkdownExtractPayload>(v).map_err(|e| {
            CliError::unknown(
                format!("Unexpected markdown extraction payload: {}", e),
                "Retry after the page finishes rendering",
            )
        })?;
        Ok(render_markdown_payload(&payload))
    }

    pub async fn wait_for_text(&self, text: &str, timeout: Duration) -> Result<(), CliError> {
        let deadline = Instant::now() + timeout;
        let needle = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
        let expr = format!(
            "document.body && document.body.innerText && document.body.innerText.includes({})",
            needle
        );

        while Instant::now() < deadline {
            let ok = self.eval_bool(&expr).await.unwrap_or(false);
            if ok {
                return Ok(());
            }
            tokio::time::sleep(WAIT_POLL).await;
        }

        Err(CliError::new(
            crate::types::ErrorCode::WaitTimeout,
            format!("Timed out waiting for text: {}", text),
            "Run 'sauron page snapshot' to inspect the current page state",
            true,
            1,
        ))
    }

    pub async fn wait_for_url(&self, pattern: &str, timeout: Duration) -> Result<(), CliError> {
        let deadline = Instant::now() + timeout;
        let escaped = regex::escape(pattern);
        let wildcard_pattern = escaped.replace("\\*", ".*");
        let anchored = format!("^{}$", wildcard_pattern);
        let re = Regex::new(&anchored).map_err(|_| {
            CliError::bad_input(
                format!("Invalid URL pattern: {}", pattern),
                "Use '*' as a wildcard only",
            )
        })?;

        while Instant::now() < deadline {
            let cur = self.url().await.unwrap_or_default();
            if re.is_match(&cur) {
                return Ok(());
            }
            tokio::time::sleep(WAIT_POLL).await;
        }

        Err(CliError::new(
            crate::types::ErrorCode::WaitTimeout,
            format!("Timed out waiting for url: {}", pattern),
            "Run 'sauron page snapshot' to inspect the current page state",
            true,
            1,
        ))
    }

    #[allow(dead_code)]
    pub async fn wait_for_selector(
        &self,
        selector: &str,
        timeout: Duration,
    ) -> Result<(), CliError> {
        let _ = self
            .wait_for_selector_state(selector, SelectorWaitState::Attached, None, timeout)
            .await?;
        Ok(())
    }

    pub async fn wait_for_selector_state(
        &self,
        selector: &str,
        state: SelectorWaitState,
        expected_count: Option<u32>,
        timeout: Duration,
    ) -> Result<SelectorWaitOutcome, CliError> {
        let selector = selector.trim();
        if selector.is_empty() {
            return Err(CliError::bad_input(
                "Selector cannot be empty",
                "Pass a non-empty CSS selector such as '#submit' or '.button.primary'",
            ));
        }

        let deadline = Instant::now() + timeout;
        let mut last = SelectorStats::default();
        let mut last_error: Option<String> = None;

        loop {
            match self.selector_stats(selector).await {
                Ok(stats) => {
                    last = stats;
                }
                Err(err) => {
                    if matches!(err.code, crate::types::ErrorCode::BadInput) {
                        return Err(err);
                    }

                    last_error = Some(err.message.clone());
                    if Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(WAIT_POLL).await;
                    continue;
                }
            }

            let count_matches = expected_count
                .map(|expected| last.count == expected)
                .unwrap_or(true);
            if count_matches && state.is_satisfied(last.count, last.visible_count) {
                return Ok(SelectorWaitOutcome {
                    selector: selector.to_string(),
                    state,
                    count: last.count,
                    visible_count: last.visible_count,
                    expected_count,
                });
            }

            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(WAIT_POLL).await;
        }

        let expectation = expected_count
            .map(|count| format!("state={} and count={}", state.as_str(), count))
            .unwrap_or_else(|| format!("state={}", state.as_str()));
        let error_context = last_error
            .as_deref()
            .map(|msg| format!("; last transient error={}", msg))
            .unwrap_or_default();

        Err(CliError::new(
            crate::types::ErrorCode::WaitTimeout,
            format!(
                "Timed out waiting for selector '{}' ({}) [last count={}, visible={}{}]",
                selector, expectation, last.count, last.visible_count, error_context
            ),
            "Check selector/state, or increase --timeout-ms",
            true,
            1,
        ))
    }

    pub async fn wait_for_idle(&self, timeout: Duration) -> Result<(), CliError> {
        let ok = self
            .wait_for_network_idle(Duration::from_millis(500), 0, timeout)
            .await?;
        if ok {
            Ok(())
        } else {
            Err(CliError::new(
                crate::types::ErrorCode::WaitTimeout,
                "Timed out waiting for network idle".to_string(),
                "Run 'sauron page snapshot' to inspect the current page state",
                true,
                1,
            ))
        }
    }

    pub async fn wait_for_function(
        &self,
        expression: &str,
        timeout: Duration,
    ) -> Result<(), CliError> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.eval_bool(expression).await.unwrap_or(false) {
                return Ok(());
            }
            tokio::time::sleep(WAIT_POLL).await;
        }

        Err(CliError::new(
            crate::types::ErrorCode::WaitTimeout,
            "Timed out waiting for function condition".to_string(),
            "Check the expression or increase --timeout-ms",
            true,
            1,
        ))
    }

    pub async fn wait_for_load_state(
        &self,
        load_state: &str,
        timeout: Duration,
    ) -> Result<(), CliError> {
        match load_state {
            "load" | "domcontentloaded" => {
                let deadline = Instant::now() + timeout;
                while Instant::now() < deadline {
                    let state = self.document_ready_state().await?;
                    let ready = match load_state {
                        "load" => matches!(state, DocumentReady::Complete),
                        _ => matches!(state, DocumentReady::Interactive | DocumentReady::Complete),
                    };
                    if ready {
                        return Ok(());
                    }
                    tokio::time::sleep(WAIT_POLL).await;
                }
                Err(CliError::new(
                    crate::types::ErrorCode::WaitTimeout,
                    format!("Timed out waiting for load state '{}'", load_state),
                    "Increase --timeout-ms or wait for page resources to settle",
                    true,
                    1,
                ))
            }
            "networkidle" | "networkidle0" => self.wait_for_idle(timeout).await,
            "networkidle2" => {
                let ok = self
                    .wait_for_network_idle(Duration::from_millis(500), 2, timeout)
                    .await?;
                if ok {
                    Ok(())
                } else {
                    Err(CliError::new(
                        crate::types::ErrorCode::WaitTimeout,
                        "Timed out waiting for networkidle2".to_string(),
                        "Increase --timeout-ms or loosen wait condition",
                        true,
                        1,
                    ))
                }
            }
            _ => Err(CliError::bad_input(
                format!("Unsupported load state '{}'", load_state),
                "Use one of: load, domcontentloaded, networkidle, networkidle2",
            )),
        }
    }

    async fn eval_bool(&self, expression: &str) -> Result<bool, CliError> {
        let v = self.eval(expression).await?;
        Ok(v.as_bool().unwrap_or(false))
    }

    async fn selector_stats(&self, selector: &str) -> Result<SelectorStats, CliError> {
        let sel = serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".to_string());
        let expr = format!(
            r#"(() => {{
  const selector = {selector};
  const result = {{ count: 0, visibleCount: 0, error: null }};
  const isVisible = (el) => {{
    if (!el || !el.isConnected) return false;
    const style = window.getComputedStyle(el);
    if (!style) return false;
    if (style.display === "none" || style.visibility === "hidden" || style.visibility === "collapse") return false;
    const rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  }};
  try {{
    const nodes = Array.from(document.querySelectorAll(selector));
    result.count = nodes.length;
    for (const node of nodes) {{
      if (isVisible(node)) result.visibleCount += 1;
    }}
  }} catch (err) {{
    result.error = String((err && err.message) || err || "Invalid selector");
  }}
  return result;
}})()"#,
            selector = sel
        );

        let value = self.eval(&expr).await?;
        let obj = value.as_object().ok_or_else(|| {
            CliError::unknown(
                "Failed to evaluate selector state in page context",
                "Retry after navigation completes",
            )
        })?;

        if let Some(err) = obj.get("error").and_then(|v| v.as_str()) {
            if !err.trim().is_empty() {
                return Err(CliError::bad_input(
                    format!("Invalid CSS selector '{}': {}", selector, err),
                    "Check selector syntax (for example: '#login', '.btn.primary', 'form input[name=\"email\"]')",
                ));
            }
        }

        let count = value_to_u32(obj.get("count")).unwrap_or(0);
        let visible_count = value_to_u32(obj.get("visibleCount")).unwrap_or(0);

        Ok(SelectorStats {
            count,
            visible_count,
        })
    }

    #[allow(dead_code)]
    pub async fn capture_console_for(
        &self,
        duration: Duration,
    ) -> Result<Vec<ConsoleCaptureEntry>, CliError> {
        let mut events = self.cdp.subscribe();
        let _ = self.call("Runtime.enable", json!({}), CDP_TIMEOUT).await;
        let _ = self.call("Log.enable", json!({}), CDP_TIMEOUT).await;

        let deadline = Instant::now() + duration;
        let mut out = Vec::new();

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let tick = Duration::from_millis(250).min(remaining);

            match tokio::time::timeout(tick, events.recv()).await {
                Ok(Ok(ev)) => {
                    if ev.session_id.as_deref() != Some(self.session_id.as_str()) {
                        continue;
                    }

                    let entry = match ev.method.as_str() {
                        "Runtime.consoleAPICalled" => {
                            console_entry_from_runtime_console(&ev.params)
                        }
                        "Runtime.exceptionThrown" => {
                            console_entry_from_runtime_exception(&ev.params)
                        }
                        "Log.entryAdded" => console_entry_from_log_entry(&ev.params),
                        _ => None,
                    };

                    if let Some(entry) = entry {
                        out.push(entry);
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                    return Err(CliError::daemon_down(
                        "CDP event stream closed during console capture",
                        "Run 'sauron runtime start' and retry console capture",
                    ));
                }
                Err(_) => {}
            }
        }

        Ok(out)
    }

    #[allow(dead_code)]
    pub async fn capture_network_for(
        &self,
        duration: Duration,
    ) -> Result<Vec<NetworkCaptureEntry>, CliError> {
        let mut events = self.cdp.subscribe();
        let _ = self.call("Network.enable", json!({}), CDP_TIMEOUT).await;

        let deadline = Instant::now() + duration;
        let mut out = Vec::new();
        let mut by_request: HashMap<String, NetworkRequestContext> = HashMap::new();

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let tick = Duration::from_millis(250).min(remaining);

            match tokio::time::timeout(tick, events.recv()).await {
                Ok(Ok(ev)) => {
                    if ev.session_id.as_deref() != Some(self.session_id.as_str()) {
                        continue;
                    }

                    match ev.method.as_str() {
                        "Network.requestWillBeSent" => {
                            if let Some(entry) =
                                network_request_entry_from_event(&ev.params, &mut by_request)
                            {
                                out.push(entry);
                            }
                        }
                        "Network.responseReceived" => {
                            if let Some(entry) =
                                network_response_entry_from_event(&ev.params, &by_request)
                            {
                                out.push(entry);
                            }
                        }
                        "Network.loadingFailed" => {
                            if let Some(entry) =
                                network_failure_entry_from_event(&ev.params, &by_request)
                            {
                                out.push(entry);
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                    return Err(CliError::daemon_down(
                        "CDP event stream closed during network capture",
                        "Run 'sauron runtime start' and retry network capture",
                    ));
                }
                Err(_) => {}
            }
        }

        Ok(out)
    }

    pub async fn next_dialog(&self, timeout: Duration) -> Result<Option<DialogEvent>, CliError> {
        let mut events = self.cdp.subscribe();
        let start = Instant::now();
        while Instant::now().duration_since(start) < timeout {
            match tokio::time::timeout(Duration::from_millis(250), events.recv()).await {
                Ok(Ok(ev)) => {
                    if ev.session_id.as_deref() != Some(self.session_id.as_str()) {
                        continue;
                    }
                    if ev.method == "Page.javascriptDialogOpening" {
                        if let Ok(d) = serde_json::from_value::<DialogEvent>(ev.params.clone()) {
                            return Ok(Some(d));
                        }
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) => break,
                Err(_) => {}
            }
        }
        Ok(None)
    }

    pub async fn handle_dialog(
        &self,
        accept: bool,
        prompt_text: Option<&str>,
    ) -> Result<(), CliError> {
        let mut params = json!({ "accept": accept });
        if let Some(t) = prompt_text {
            params["promptText"] = Value::String(t.to_string());
        }
        let _ = self
            .call("Page.handleJavaScriptDialog", params, CDP_TIMEOUT)
            .await?;
        Ok(())
    }

    // --- Snapshot + persistence ---

    pub async fn snapshot_and_persist(
        &self,
        ctx: &AppContext,
        opts: SnapshotOptions,
    ) -> Result<crate::types::SnapshotResult, CliError> {
        let _client_lock = ctx.acquire_client_lock()?;
        let url = self.url().await.unwrap_or_default();
        let ax = self.accessibility_tree().await?;

        let prev = load_ref_state(ctx).await?;
        let next_id = prev.as_ref().map(|s| s.snapshot_id + 1).unwrap_or(1);

        let result = serialize_tree(&ax, opts, next_id, url.clone());

        // Save snapshot text first so refs never point to a missing snapshot file.
        save_snapshot(ctx, result.snapshot_id, &result.tree).await?;

        // Persist refs
        save_ref_state(
            ctx,
            &PersistedRefState {
                snapshot_id: result.snapshot_id,
                url: result.url.clone(),
                last_snapshot: result.tree.clone(),
                refs: result.refs.clone(),
            },
        )
        .await?;

        Ok(result)
    }

    // --- Helpers ---

    async fn box_center(&self, backend_node_id: u64) -> Result<(f64, f64), CliError> {
        let res = self
            .call(
                "DOM.getBoxModel",
                json!({ "backendNodeId": backend_node_id }),
                CDP_TIMEOUT,
            )
            .await?;

        let content = res
            .get("model")
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                CliError::new(
                    crate::types::ErrorCode::ElementNotVisible,
                    "Element has no box model",
                    "Try scrolling it into view",
                    true,
                    1,
                )
            })?;

        if content.len() < 8 {
            return Err(CliError::new(
                crate::types::ErrorCode::ElementNotVisible,
                "Unexpected box model".to_string(),
                "Try scrolling it into view",
                true,
                1,
            ));
        }

        let xs = [
            content[0].as_f64().unwrap_or(0.0),
            content[2].as_f64().unwrap_or(0.0),
            content[4].as_f64().unwrap_or(0.0),
            content[6].as_f64().unwrap_or(0.0),
        ];
        let ys = [
            content[1].as_f64().unwrap_or(0.0),
            content[3].as_f64().unwrap_or(0.0),
            content[5].as_f64().unwrap_or(0.0),
            content[7].as_f64().unwrap_or(0.0),
        ];

        let (min_x, max_x) = xs
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(mn, mx), v| {
                (mn.min(*v), mx.max(*v))
            });
        let (min_y, max_y) = ys
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(mn, mx), v| {
                (mn.min(*v), mx.max(*v))
            });

        Ok(((min_x + max_x) / 2.0, (min_y + max_y) / 2.0))
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct MarkdownExtractPayload {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    fallback_text: String,
    #[serde(default)]
    blocks: Vec<MarkdownBlock>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MarkdownBlock {
    Heading { level: u8, text: String },
    Paragraph { text: String },
    OrderedList { items: Vec<MarkdownListItem> },
    UnorderedList { items: Vec<MarkdownListItem> },
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct MarkdownListItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    meta: String,
    #[serde(default)]
    body: String,
}

fn render_markdown_payload(payload: &MarkdownExtractPayload) -> String {
    let mut sections: Vec<String> = Vec::new();

    let title = collapse_markdown_text(&payload.title);
    if !title.is_empty() {
        sections.push(format!("# {}", title));
    }

    let url = payload.url.trim();
    if !url.is_empty() {
        sections.push(format!("_Source_: {}", url));
    }

    let body = render_markdown_body(payload);
    if !body.is_empty() {
        sections.push(body);
    }

    sections.join("\n\n")
}

fn render_markdown_body(payload: &MarkdownExtractPayload) -> String {
    let blocks = payload
        .blocks
        .iter()
        .filter_map(render_markdown_block)
        .collect::<Vec<_>>();
    if !blocks.is_empty() {
        return blocks.join("\n\n");
    }

    let fallback = preserve_markdown_lines(&payload.fallback_text);
    if fallback.is_empty() {
        String::new()
    } else {
        fallback.replace('\n', "  \n")
    }
}

fn render_markdown_block(block: &MarkdownBlock) -> Option<String> {
    match block {
        MarkdownBlock::Heading { level, text } => {
            let text = collapse_markdown_text(text);
            if text.is_empty() {
                None
            } else {
                let depth = (*level).clamp(1, 6);
                Some(format!("{} {}", "#".repeat(depth as usize), text))
            }
        }
        MarkdownBlock::Paragraph { text } => {
            let text = preserve_markdown_lines(text);
            if text.is_empty() {
                None
            } else {
                Some(text.replace('\n', "  \n"))
            }
        }
        MarkdownBlock::OrderedList { items } => render_markdown_list(items, true),
        MarkdownBlock::UnorderedList { items } => render_markdown_list(items, false),
    }
}

fn render_markdown_list(items: &[MarkdownListItem], ordered: bool) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        let head = render_markdown_list_head(item);
        let body = preserve_markdown_lines(&item.body);
        if head.is_empty() && body.is_empty() {
            continue;
        }

        let marker = if ordered {
            format!("{}.", idx + 1)
        } else {
            "-".to_string()
        };

        if head.is_empty() {
            let mut body_lines = body.lines();
            if let Some(first) = body_lines.next() {
                lines.push(format!("{} {}", marker, first));
                for line in body_lines {
                    lines.push(format!("   {}", line));
                }
            }
            continue;
        }

        lines.push(format!("{} {}", marker, head));
        if !body.is_empty() {
            for line in body.lines() {
                lines.push(format!("   {}", line));
            }
        }
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn render_markdown_list_head(item: &MarkdownListItem) -> String {
    let title = collapse_markdown_text(&item.title);
    let meta = collapse_markdown_text(&item.meta);

    match (title.is_empty(), meta.is_empty()) {
        (false, false) => format!("**{}** ({})", title, meta),
        (false, true) => format!("**{}**", title),
        (true, false) => meta,
        (true, true) => String::new(),
    }
}

fn collapse_markdown_text(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn preserve_markdown_lines(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DocumentReady {
    Loading,
    Interactive,
    Complete,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigateOutcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DialogEvent {
    pub url: Option<String>,
    pub message: String,
    #[serde(rename = "type")]
    pub dialog_type: String,
    pub default_prompt: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SelectorWaitState {
    Attached,
    Visible,
    Hidden,
    Detached,
}

impl SelectorWaitState {
    #[allow(dead_code)]
    pub fn parse(value: &str) -> Result<Self, CliError> {
        match value.to_ascii_lowercase().as_str() {
            "attached" => Ok(Self::Attached),
            "visible" => Ok(Self::Visible),
            "hidden" => Ok(Self::Hidden),
            "detached" => Ok(Self::Detached),
            _ => Err(CliError::bad_input(
                format!("Unsupported selector wait state: {}", value),
                "Use one of: attached, visible, hidden, detached",
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Attached => "attached",
            Self::Visible => "visible",
            Self::Hidden => "hidden",
            Self::Detached => "detached",
        }
    }

    fn is_satisfied(self, count: u32, visible_count: u32) -> bool {
        match self {
            Self::Attached => count > 0,
            Self::Visible => visible_count > 0,
            Self::Hidden => visible_count == 0,
            Self::Detached => count == 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectorWaitOutcome {
    pub selector: String,
    pub state: SelectorWaitState,
    pub count: u32,
    pub visible_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct ConsoleCaptureEntry {
    pub kind: String,
    pub level: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct NetworkCaptureEntry {
    pub kind: String,
    pub request_id: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canceled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_cache: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default)]
struct SelectorStats {
    count: u32,
    visible_count: u32,
}

// --- AX parsing helpers ---

#[derive(Debug, Clone)]
struct RawAxNode {
    role: String,
    name: Option<String>,
    child_ids: Option<Vec<String>>,
    backend_dom_node_id: Option<u64>,

    level: Option<i64>,
    disabled: bool,
    expanded: Option<bool>,
    checked: Option<AxState>,
    selected: bool,
    required: bool,
    focused: bool,
    pressed: Option<AxState>,
    value: Option<String>,
    url: Option<String>,
}

impl RawAxNode {
    fn from_value(v: Value) -> Self {
        let role = ax_value_str(v.get("role")).unwrap_or_else(|| "unknown".to_string());
        let name = ax_value_str(v.get("name")).filter(|s| !s.is_empty());
        let value = ax_value_str(v.get("value"));
        let child_ids = v.get("childIds").and_then(|c| c.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        });
        let backend_dom_node_id = v.get("backendDOMNodeId").and_then(|x| x.as_u64());

        // properties array
        let mut props: HashMap<String, Value> = HashMap::new();
        if let Some(arr) = v.get("properties").and_then(|p| p.as_array()) {
            for p in arr {
                if let Some(name) = p.get("name").and_then(|n| n.as_str()) {
                    let val = p.get("value").cloned().unwrap_or(Value::Null);
                    props.insert(name.to_string(), val);
                }
            }
        }

        let level = props.get("level").and_then(|v| ax_value_i64(Some(v)));
        let disabled = props
            .get("disabled")
            .and_then(|v| ax_value_bool(Some(v)))
            .unwrap_or(false);
        let expanded = props.get("expanded").and_then(|v| ax_value_bool(Some(v)));

        let checked = props.get("checked").and_then(|v| ax_value_state(Some(v)));
        let selected = props
            .get("selected")
            .and_then(|v| ax_value_bool(Some(v)))
            .unwrap_or(false);
        let required = props
            .get("required")
            .and_then(|v| ax_value_bool(Some(v)))
            .unwrap_or(false);
        let focused = props
            .get("focused")
            .and_then(|v| ax_value_bool(Some(v)))
            .unwrap_or(false);
        let pressed = props.get("pressed").and_then(|v| ax_value_state(Some(v)));

        let url = props.get("url").and_then(|v| ax_value_str(Some(v)));

        Self {
            role,
            name,
            child_ids,
            backend_dom_node_id,
            level,
            disabled,
            expanded,
            checked,
            selected,
            required,
            focused,
            pressed,
            value,
            url,
        }
    }
}

fn build_ax_tree(
    id: &str,
    map: &HashMap<String, RawAxNode>,
    visiting: &mut HashSet<String>,
) -> AxNode {
    if visiting.contains(id) {
        // cycle guard
        return AxNode {
            role: "cycle".to_string(),
            name: None,
            children: vec![],
            level: None,
            disabled: false,
            expanded: None,
            checked: None,
            selected: false,
            required: false,
            focused: false,
            pressed: None,
            value: None,
            url: None,
            backend_dom_node_id: None,
        };
    }
    visiting.insert(id.to_string());

    let raw = map.get(id).cloned().unwrap_or(RawAxNode {
        role: "unknown".to_string(),
        name: None,
        child_ids: None,
        backend_dom_node_id: None,
        level: None,
        disabled: false,
        expanded: None,
        checked: None,
        selected: false,
        required: false,
        focused: false,
        pressed: None,
        value: None,
        url: None,
    });

    let mut children: Vec<AxNode> = Vec::new();
    if let Some(cids) = raw.child_ids.clone() {
        for cid in cids {
            if map.contains_key(&cid) {
                children.push(build_ax_tree(&cid, map, visiting));
            }
        }
    }

    visiting.remove(id);

    AxNode {
        role: raw.role,
        name: raw.name,
        children,
        level: raw.level,
        disabled: raw.disabled,
        expanded: raw.expanded,
        checked: raw.checked,
        selected: raw.selected,
        required: raw.required,
        focused: raw.focused,
        pressed: raw.pressed,
        value: raw.value,
        url: raw.url,
        backend_dom_node_id: raw.backend_dom_node_id,
    }
}

fn ax_value_str(v: Option<&Value>) -> Option<String> {
    let v = v?;
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    if let Some(obj) = v.as_object() {
        if let Some(val) = obj.get("value") {
            if let Some(s) = val.as_str() {
                return Some(s.to_string());
            }
            if val.is_number() {
                return Some(val.to_string());
            }
            if let Some(b) = val.as_bool() {
                return Some(b.to_string());
            }
        }
    }
    None
}

fn ax_value_bool(v: Option<&Value>) -> Option<bool> {
    let v = v?;
    if let Some(b) = v.as_bool() {
        return Some(b);
    }
    if let Some(obj) = v.as_object() {
        if let Some(val) = obj.get("value") {
            if let Some(b) = val.as_bool() {
                return Some(b);
            }
            if let Some(s) = val.as_str() {
                if s == "true" {
                    return Some(true);
                }
                if s == "false" {
                    return Some(false);
                }
            }
        }
    }
    None
}

fn ax_value_i64(v: Option<&Value>) -> Option<i64> {
    let v = v?;
    if let Some(n) = v.as_i64() {
        return Some(n);
    }
    if let Some(obj) = v.as_object() {
        if let Some(val) = obj.get("value") {
            if let Some(n) = val.as_i64() {
                return Some(n);
            }
            if let Some(s) = val.as_str() {
                return s.parse::<i64>().ok();
            }
        }
    }
    None
}

fn ax_value_state(v: Option<&Value>) -> Option<AxState> {
    let v = v?;
    if let Some(obj) = v.as_object() {
        if let Some(val) = obj.get("value") {
            if let Some(s) = val.as_str() {
                if s == "mixed" {
                    return Some(AxState::Mixed);
                }
                if s == "true" {
                    return Some(AxState::True);
                }
                if s == "false" {
                    return None;
                }
            }
            if let Some(b) = val.as_bool() {
                if b {
                    return Some(AxState::True);
                }
                return None;
            }
        }
    }
    if let Some(s) = v.as_str() {
        if s == "mixed" {
            return Some(AxState::Mixed);
        }
        if s == "true" {
            return Some(AxState::True);
        }
    }
    None
}

// --- Persistence ---

pub async fn save_ref_state(ctx: &AppContext, state: &PersistedRefState) -> Result<(), CliError> {
    ctx.ensure_instance_dirs()?;
    let data = serde_json::to_string_pretty(state)
        .map_err(|e| CliError::unknown(format!("Failed to serialize ref state: {}", e), ""))?;

    atomic_write(&ctx.refs_path, data.as_bytes())?;
    Ok(())
}

pub async fn load_ref_state(ctx: &AppContext) -> Result<Option<PersistedRefState>, CliError> {
    match tokio::fs::read_to_string(&ctx.refs_path).await {
        Ok(text) => {
            let parsed = serde_json::from_str::<PersistedRefState>(&text).ok();
            Ok(parsed)
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(CliError::unknown(
                    format!("Failed to read {}: {}", ctx.refs_path.display(), e),
                    "Check filesystem permissions",
                ))
            }
        }
    }
}

pub async fn save_snapshot(ctx: &AppContext, snapshot_id: u64, text: &str) -> Result<(), CliError> {
    std::fs::create_dir_all(&ctx.snapshots_dir).map_err(|e| {
        CliError::unknown(
            format!("Failed to create snapshots dir: {}", e),
            "Check filesystem permissions",
        )
    })?;

    let path = ctx.snapshots_dir.join(format!("{}.txt", snapshot_id));
    atomic_write(&path, text.as_bytes())?;
    Ok(())
}

fn load_client_target_binding(ctx: &AppContext) -> Result<Option<ClientTargetBinding>, CliError> {
    match std::fs::read_to_string(&ctx.target_path) {
        Ok(text) => {
            let parsed = serde_json::from_str::<ClientTargetBinding>(&text).ok();
            Ok(parsed)
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(CliError::unknown(
                    format!("Failed to read {}: {}", ctx.target_path.display(), e),
                    "Check filesystem permissions",
                ))
            }
        }
    }
}

fn save_client_target_binding(
    ctx: &AppContext,
    binding: &ClientTargetBinding,
) -> Result<(), CliError> {
    ctx.ensure_instance_dirs()?;
    let text = serde_json::to_string_pretty(binding).map_err(|e| {
        CliError::unknown(
            format!("Failed to serialize target binding: {}", e),
            "This should not happen",
        )
    })?;
    atomic_write(&ctx.target_path, text.as_bytes())
}

pub fn get_bound_target_id(ctx: &AppContext) -> Result<Option<String>, CliError> {
    Ok(load_client_target_binding(ctx)?.map(|b| b.target_id))
}

pub fn set_bound_target_id(ctx: &AppContext, target_id: &str) -> Result<(), CliError> {
    save_client_target_binding(
        ctx,
        &ClientTargetBinding {
            target_id: target_id.to_string(),
        },
    )
}

#[allow(dead_code)]
pub fn clear_bound_target_id(ctx: &AppContext) -> Result<(), CliError> {
    match std::fs::remove_file(&ctx.target_path) {
        Ok(_) => Ok(()),
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(CliError::unknown(
                    format!("Failed to remove {}: {}", ctx.target_path.display(), e),
                    "Check filesystem permissions",
                ))
            }
        }
    }
}

pub async fn load_snapshot(ctx: &AppContext, snapshot_id: u64) -> Result<Option<String>, CliError> {
    let path = ctx.snapshots_dir.join(format!("{}.txt", snapshot_id));
    match tokio::fs::read_to_string(path).await {
        Ok(text) => Ok(Some(text)),
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(None)
            } else {
                Err(CliError::unknown(
                    format!("Failed to read snapshot: {}", e),
                    "Check filesystem permissions",
                ))
            }
        }
    }
}

// --- Ref resolution helpers ---

fn parse_locator_nth(locator: &str) -> Option<u32> {
    let idx = locator.find("nth=")?;
    let start = idx + "nth=".len();
    let bytes = locator.as_bytes();
    let mut end = start;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == start {
        return None;
    }
    locator[start..end].parse::<u32>().ok()
}

fn collect_matching_refs<'a>(
    node: &'a AxNode,
    role: &str,
    name: Option<&str>,
    out: &mut Vec<&'a AxNode>,
) {
    if should_assign_ref(&node.role, node.name.as_deref()) && node.role == role {
        let name_ok = match name {
            Some(n) if !n.is_empty() => node.name.as_deref() == Some(n),
            _ => true,
        };
        if name_ok {
            out.push(node);
        }
    }
    for c in &node.children {
        collect_matching_refs(c, role, name, out);
    }
}

fn collect_text_matches<'a>(
    node: &'a AxNode,
    needle: &str,
    interactive_only: bool,
    out: &mut Vec<&'a AxNode>,
) {
    let name = node.name.as_deref().unwrap_or("");
    let name_match = !needle.is_empty() && name.contains(needle);
    let interactive_match = crate::snapshot::is_interactive_role(&node.role);

    if name_match {
        if interactive_only {
            if interactive_match {
                out.push(node);
            }
        } else if should_assign_ref(&node.role, node.name.as_deref()) {
            out.push(node);
        }
    }

    for c in &node.children {
        collect_text_matches(c, needle, interactive_only, out);
    }
}

fn semantic_kind_name(kind: SemanticLocatorKind) -> &'static str {
    match kind {
        SemanticLocatorKind::Text => "text",
        SemanticLocatorKind::Role => "role",
        SemanticLocatorKind::Label => "label",
        SemanticLocatorKind::Placeholder => "placeholder",
        SemanticLocatorKind::AltText => "altText",
        SemanticLocatorKind::Title => "title",
        SemanticLocatorKind::TestId => "testId",
    }
}

fn semantic_locator_expression(
    kind: SemanticLocatorKind,
    query: &str,
    secondary: Option<&str>,
    nth: Option<u32>,
    count_only: bool,
) -> Result<String, CliError> {
    let kind_json = serde_json::to_string(semantic_kind_name(kind))
        .map_err(|e| CliError::unknown(format!("Failed to encode locator kind: {}", e), ""))?;
    let query_json = serde_json::to_string(query)
        .map_err(|e| CliError::unknown(format!("Failed to encode locator query: {}", e), ""))?;
    let secondary_json = serde_json::to_string(secondary.unwrap_or(""))
        .map_err(|e| CliError::unknown(format!("Failed to encode locator secondary: {}", e), ""))?;
    let index = nth.unwrap_or(0);

    let tail = if count_only {
        "return matches.length;".to_string()
    } else {
        format!("return matches[{}] || null;", index)
    };

    Ok(format!(
        r#"(() => {{
  const kind = {kind};
  const query = ({query} || "").trim();
  const secondary = ({secondary} || "").trim();
  if (!query && kind !== "role") return {empty_return};
  const lowerQuery = query.toLowerCase();
  const lowerSecondary = secondary.toLowerCase();
  const all = Array.from(document.querySelectorAll("*"));

  const textOf = (el) => {{
    const aria = (el.getAttribute("aria-label") || "").trim();
    const txt = (el.innerText || el.textContent || "").trim();
    const val = (el.value || "").toString().trim();
    return [aria, txt, val].find((v) => !!v) || "";
  }};

  const roleOf = (el) => {{
    const explicit = (el.getAttribute("role") || "").trim().toLowerCase();
    if (explicit) return explicit;
    const tag = (el.tagName || "").toLowerCase();
    if (tag === "a") return "link";
    if (tag === "button") return "button";
    if (tag === "input") {{
      const t = (el.getAttribute("type") || "text").toLowerCase();
      if (t === "checkbox") return "checkbox";
      if (t === "radio") return "radio";
      return "textbox";
    }}
    return tag;
  }};

  const nameMatches = (value, needle) => value.toLowerCase().includes(needle);
  const byText = () => all.filter((el) => nameMatches(textOf(el), lowerQuery));
  const byRole = () => all.filter((el) => {{
    if (roleOf(el) !== lowerQuery) return false;
    if (!lowerSecondary) return true;
    return nameMatches(textOf(el), lowerSecondary);
  }});
  const byLabel = () => {{
    const labels = Array.from(document.querySelectorAll("label"));
    const matches = [];
    for (const label of labels) {{
      const txt = (label.innerText || label.textContent || "").toLowerCase();
      if (!txt.includes(lowerQuery)) continue;
      if (label.control) matches.push(label.control);
      const forAttr = label.getAttribute("for");
      if (forAttr) {{
        const target = document.getElementById(forAttr);
        if (target) matches.push(target);
      }}
    }}
    return matches;
  }};
  const byPlaceholder = () => all.filter((el) => (el.getAttribute("placeholder") || "").toLowerCase().includes(lowerQuery));
  const byAltText = () => all.filter((el) => (el.getAttribute("alt") || "").toLowerCase().includes(lowerQuery));
  const byTitle = () => all.filter((el) => (el.getAttribute("title") || "").toLowerCase().includes(lowerQuery));
  const byTestId = () => all.filter((el) => {{
    const value = (el.getAttribute("data-testid") || el.getAttribute("data-test-id") || "").toLowerCase();
    return value === lowerQuery || value.includes(lowerQuery);
  }});

  let matches = [];
  switch (kind) {{
    case "text":
      matches = byText();
      break;
    case "role":
      matches = byRole();
      break;
    case "label":
      matches = byLabel();
      break;
    case "placeholder":
      matches = byPlaceholder();
      break;
    case "altText":
      matches = byAltText();
      break;
    case "title":
      matches = byTitle();
      break;
    case "testId":
      matches = byTestId();
      break;
    default:
      matches = [];
  }}

  matches = matches.filter((el, idx) => el && matches.indexOf(el) === idx);
  {tail}
}})()"#,
        kind = kind_json,
        query = query_json,
        secondary = secondary_json,
        empty_return = if count_only { "0" } else { "null" },
        tail = tail
    ))
}

// --- CDP error mapping ---

fn map_cdp_error(_method: &str) -> impl FnOnce(CdpError) -> CliError {
    move |e| match e {
        CdpError::Timeout => CliError::timeout(
            "CDP call timed out",
            "Run 'sauron page snapshot' to inspect the current page state",
        ),
        CdpError::WebSocket(msg) => CliError::daemon_down(
            format!("Chrome websocket error: {}", msg),
            "Run 'sauron runtime start' to start the Chrome daemon",
        ),
        CdpError::Protocol(msg) => CliError::unknown(
            format!("CDP protocol error: {}", msg),
            "Run 'sauron page snapshot' to inspect the current page state",
        ),
    }
}

#[allow(dead_code)]
fn map_selector_query_error(selector: &str, err: CdpError) -> CliError {
    match err {
        CdpError::Protocol(msg) if is_invalid_selector_protocol_message(&msg) => CliError::bad_input(
            format!("Invalid CSS selector: {}", selector),
            "Check selector syntax (for example: '#login', '.btn.primary', 'form input[name=\"email\"]')",
        ),
        other => map_cdp_error("DOM.querySelectorAll")(other),
    }
}

#[allow(dead_code)]
fn is_invalid_selector_protocol_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("syntaxerror")
        || lower.contains("invalid selector")
        || lower.contains("not a valid selector")
        || lower.contains("queryselector")
}

fn value_to_u32(value: Option<&Value>) -> Option<u32> {
    let value = value?;
    if let Some(v) = value.as_u64() {
        return u32::try_from(v).ok();
    }
    if let Some(v) = value.as_i64() {
        if v >= 0 {
            return u32::try_from(v as u64).ok();
        }
    }
    if let Some(v) = value.as_f64() {
        if v.is_finite() && v >= 0.0 {
            return u32::try_from(v.round() as u64).ok();
        }
    }
    None
}

#[allow(dead_code)]
fn value_to_i64(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(v) = value.as_i64() {
        return Some(v);
    }
    if let Some(v) = value.as_u64() {
        return i64::try_from(v).ok();
    }
    if let Some(v) = value.as_f64() {
        if v.is_finite() {
            return Some(v.round() as i64);
        }
    }
    None
}

#[allow(dead_code)]
fn value_to_f64(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    if let Some(v) = value.as_f64() {
        return Some(v);
    }
    if let Some(v) = value.as_i64() {
        return Some(v as f64);
    }
    if let Some(v) = value.as_u64() {
        return Some(v as f64);
    }
    None
}

#[allow(dead_code)]
fn runtime_arg_text(arg: &Value) -> String {
    if let Some(value) = arg.get("value") {
        if let Some(s) = value.as_str() {
            return s.to_string();
        }
        if value.is_number() || value.is_boolean() {
            return value.to_string();
        }
        if !value.is_null() {
            return value.to_string();
        }
    }

    if let Some(v) = arg.get("unserializableValue").and_then(|v| v.as_str()) {
        return v.to_string();
    }
    if let Some(v) = arg.get("description").and_then(|v| v.as_str()) {
        return v.to_string();
    }
    if let Some(v) = arg.get("type").and_then(|v| v.as_str()) {
        return format!("[{}]", v);
    }
    String::new()
}

#[allow(dead_code)]
fn console_entry_from_runtime_console(params: &Value) -> Option<ConsoleCaptureEntry> {
    let level = params
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("log")
        .to_string();

    let text = params
        .get("args")
        .and_then(|v| v.as_array())
        .map(|args| {
            args.iter()
                .map(runtime_arg_text)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "[console]".to_string());

    let (url, line, column) = params
        .get("stackTrace")
        .and_then(|v| v.get("callFrames"))
        .and_then(|v| v.as_array())
        .and_then(|frames| frames.first())
        .map(|frame| {
            (
                frame
                    .get("url")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string()),
                value_to_i64(frame.get("lineNumber")),
                value_to_i64(frame.get("columnNumber")),
            )
        })
        .unwrap_or((None, None, None));

    Some(ConsoleCaptureEntry {
        kind: "runtime.console".to_string(),
        level,
        text,
        source: params
            .get("context")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        url,
        line,
        column,
        timestamp: value_to_f64(params.get("timestamp")),
    })
}

#[allow(dead_code)]
fn console_entry_from_runtime_exception(params: &Value) -> Option<ConsoleCaptureEntry> {
    let details = params.get("exceptionDetails")?;
    let text = details
        .get("exception")
        .and_then(|v| v.get("description"))
        .and_then(|v| v.as_str())
        .or_else(|| details.get("text").and_then(|v| v.as_str()))
        .unwrap_or("Uncaught exception")
        .to_string();

    Some(ConsoleCaptureEntry {
        kind: "runtime.exception".to_string(),
        level: "error".to_string(),
        text,
        source: Some("runtime".to_string()),
        url: details
            .get("url")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        line: value_to_i64(details.get("lineNumber")),
        column: value_to_i64(details.get("columnNumber")),
        timestamp: value_to_f64(params.get("timestamp")),
    })
}

#[allow(dead_code)]
fn console_entry_from_log_entry(params: &Value) -> Option<ConsoleCaptureEntry> {
    let entry = params.get("entry")?;
    Some(ConsoleCaptureEntry {
        kind: "log.entry".to_string(),
        level: entry
            .get("level")
            .and_then(|v| v.as_str())
            .unwrap_or("info")
            .to_string(),
        text: entry
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        source: entry
            .get("source")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        url: entry
            .get("url")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        line: value_to_i64(entry.get("lineNumber")),
        column: None,
        timestamp: value_to_f64(entry.get("timestamp")),
    })
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct NetworkRequestContext {
    url: String,
    method: Option<String>,
    resource_type: Option<String>,
}

#[allow(dead_code)]
fn network_request_entry_from_event(
    params: &Value,
    by_request: &mut HashMap<String, NetworkRequestContext>,
) -> Option<NetworkCaptureEntry> {
    let request_id = params.get("requestId")?.as_str()?.to_string();
    let request = params.get("request").unwrap_or(&Value::Null);
    let url = request
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let method = request
        .get("method")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    let resource_type = params
        .get("type")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());

    by_request.insert(
        request_id.clone(),
        NetworkRequestContext {
            url: url.clone(),
            method: method.clone(),
            resource_type: resource_type.clone(),
        },
    );

    Some(NetworkCaptureEntry {
        kind: "request".to_string(),
        request_id,
        url,
        method,
        resource_type,
        status: None,
        status_text: None,
        ok: None,
        mime_type: None,
        error_text: None,
        canceled: None,
        blocked_reason: None,
        from_cache: None,
        timestamp: value_to_f64(params.get("timestamp")),
    })
}

#[allow(dead_code)]
fn network_response_entry_from_event(
    params: &Value,
    by_request: &HashMap<String, NetworkRequestContext>,
) -> Option<NetworkCaptureEntry> {
    let request_id = params.get("requestId")?.as_str()?.to_string();
    let response = params.get("response").unwrap_or(&Value::Null);
    let existing = by_request.get(&request_id);

    let url = response
        .get("url")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
        .or_else(|| existing.map(|ctx| ctx.url.clone()))
        .unwrap_or_default();

    let status = value_to_i64(response.get("status"));
    let from_disk_cache = response.get("fromDiskCache").and_then(|v| v.as_bool());
    let from_service_worker = response.get("fromServiceWorker").and_then(|v| v.as_bool());
    let from_cache = match (from_disk_cache, from_service_worker) {
        (Some(disk), Some(sw)) => Some(disk || sw),
        (Some(disk), None) => Some(disk),
        (None, Some(sw)) => Some(sw),
        (None, None) => None,
    };

    Some(NetworkCaptureEntry {
        kind: "response".to_string(),
        request_id,
        url,
        method: existing.and_then(|ctx| ctx.method.clone()),
        resource_type: params
            .get("type")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
            .or_else(|| existing.and_then(|ctx| ctx.resource_type.clone())),
        status,
        status_text: response
            .get("statusText")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        ok: status.map(|s| (200..400).contains(&s)),
        mime_type: response
            .get("mimeType")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        error_text: None,
        canceled: None,
        blocked_reason: None,
        from_cache,
        timestamp: value_to_f64(params.get("timestamp")),
    })
}

#[allow(dead_code)]
fn network_failure_entry_from_event(
    params: &Value,
    by_request: &HashMap<String, NetworkRequestContext>,
) -> Option<NetworkCaptureEntry> {
    let request_id = params.get("requestId")?.as_str()?.to_string();
    let existing = by_request.get(&request_id);

    Some(NetworkCaptureEntry {
        kind: "failure".to_string(),
        request_id,
        url: existing.map(|ctx| ctx.url.clone()).unwrap_or_default(),
        method: existing.and_then(|ctx| ctx.method.clone()),
        resource_type: params
            .get("type")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
            .or_else(|| existing.and_then(|ctx| ctx.resource_type.clone())),
        status: None,
        status_text: None,
        ok: Some(false),
        mime_type: None,
        error_text: params
            .get("errorText")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        canceled: params.get("canceled").and_then(|v| v.as_bool()),
        blocked_reason: params
            .get("blockedReason")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        from_cache: None,
        timestamp: value_to_f64(params.get("timestamp")),
    })
}

// --- Key helpers ---

fn parse_key_combo(combo: &str) -> (i64, String) {
    let parts: Vec<&str> = combo.split('+').collect();
    if parts.len() == 1 {
        return (0, combo.to_string());
    }

    let mut modifiers: i64 = 0;
    for p in &parts[0..parts.len() - 1] {
        match p.to_lowercase().as_str() {
            "alt" => modifiers |= 1,
            "control" | "ctrl" => modifiers |= 2,
            "meta" | "command" | "cmd" => modifiers |= 4,
            "shift" => modifiers |= 8,
            _ => {}
        }
    }

    (modifiers, parts[parts.len() - 1].to_string())
}

struct KeyInfo {
    key: String,
    code: String,
    vk: i64,
    text: String,
}

fn key_to_cdp(token: &str) -> KeyInfo {
    let t = token.trim();

    // Common special keys
    match t {
        "Enter" => {
            return KeyInfo {
                key: "Enter".to_string(),
                code: "Enter".to_string(),
                vk: 13,
                text: "".to_string(),
            }
        }
        "Tab" => {
            return KeyInfo {
                key: "Tab".to_string(),
                code: "Tab".to_string(),
                vk: 9,
                text: "".to_string(),
            }
        }
        "Backspace" => {
            return KeyInfo {
                key: "Backspace".to_string(),
                code: "Backspace".to_string(),
                vk: 8,
                text: "".to_string(),
            }
        }
        "Escape" | "Esc" => {
            return KeyInfo {
                key: "Escape".to_string(),
                code: "Escape".to_string(),
                vk: 27,
                text: "".to_string(),
            }
        }
        "ArrowUp" => {
            return KeyInfo {
                key: "ArrowUp".to_string(),
                code: "ArrowUp".to_string(),
                vk: 38,
                text: "".to_string(),
            }
        }
        "ArrowDown" => {
            return KeyInfo {
                key: "ArrowDown".to_string(),
                code: "ArrowDown".to_string(),
                vk: 40,
                text: "".to_string(),
            }
        }
        "ArrowLeft" => {
            return KeyInfo {
                key: "ArrowLeft".to_string(),
                code: "ArrowLeft".to_string(),
                vk: 37,
                text: "".to_string(),
            }
        }
        "ArrowRight" => {
            return KeyInfo {
                key: "ArrowRight".to_string(),
                code: "ArrowRight".to_string(),
                vk: 39,
                text: "".to_string(),
            }
        }
        _ => {}
    }

    // Single ASCII letter
    if t.len() == 1 {
        let ch = t.chars().next().unwrap();
        if ch.is_ascii_alphabetic() {
            let up = ch.to_ascii_uppercase();
            let key = ch.to_ascii_lowercase().to_string();
            let code = format!("Key{}", up);
            let vk = up as i64;
            return KeyInfo {
                key,
                code,
                vk,
                text: ch.to_string(),
            };
        }
        if ch.is_ascii_digit() {
            let code = format!("Digit{}", ch);
            let vk = ch as i64;
            return KeyInfo {
                key: ch.to_string(),
                code,
                vk,
                text: ch.to_string(),
            };
        }
    }

    // F1..F12
    if let Some(f_suffix) = t.strip_prefix('F') {
        if let Ok(n) = f_suffix.parse::<i64>() {
            if (1..=24).contains(&n) {
                // Virtual key codes for F1..F24 start at 112
                let vk = 111 + n;
                return KeyInfo {
                    key: t.to_string(),
                    code: t.to_string(),
                    vk,
                    text: "".to_string(),
                };
            }
        }
    }

    // Fallback
    KeyInfo {
        key: t.to_string(),
        code: t.to_string(),
        vk: 0,
        text: "".to_string(),
    }
}

fn markdown_extract_expression() -> &'static str {
    r##"(() => {
  const title = (document.title || "").trim();
  const url = (location && location.href) ? String(location.href) : "";
  const fallbackText = (document.body && document.body.innerText) ? document.body.innerText.trim() : "";

  const HEADING_LEVELS = { h1: 1, h2: 2, h3: 3, h4: 4, h5: 5, h6: 6 };
  const IGNORED_TAGS = new Set([
    "script",
    "style",
    "template",
    "noscript",
    "svg",
    "canvas"
  ]);

  const isElement = (node) => !!node && node.nodeType === Node.ELEMENT_NODE;

  const tagName = (el) => (isElement(el) ? el.tagName.toLowerCase() : "");

  const normalizeText = (value, preserveLines = false) => {
    const text = String(value || "").replace(/\u00a0/g, " ");
    if (preserveLines) {
      return text
        .split(/\r?\n+/)
        .map((line) => line.replace(/\s+/g, " ").trim())
        .filter(Boolean)
        .join("\n");
    }
    return text.replace(/\s+/g, " ").trim();
  };

  const isHidden = (el) => {
    if (!isElement(el)) return true;
    if (el.hidden) return true;
    if (el.getAttribute("aria-hidden") === "true") return true;
    const style = window.getComputedStyle(el);
    if (!style) return false;
    return (
      style.display === "none" ||
      style.visibility === "hidden" ||
      style.visibility === "collapse"
    );
  };

  const isIgnoredElement = (el) => {
    if (!isElement(el)) return true;
    const tag = tagName(el);
    return IGNORED_TAGS.has(tag) || tag === "nav" || tag === "aside" || isHidden(el);
  };

  const elementText = (el, preserveLines = false) =>
    normalizeText(isElement(el) ? el.innerText : "", preserveLines);

  const hasMeaningfulText = (el) => elementText(el, true) !== "";

  const visibleChildren = (el) =>
    Array.from(el.children || []).filter(
      (child) => !isIgnoredElement(child) && hasMeaningfulText(child)
    );

  const visibleMatches = (root, selector) =>
    Array.from(root.querySelectorAll(selector))
      .filter((el) => !isIgnoredElement(el))
      .map((el) => elementText(el, true))
      .filter(Boolean);

  const cardLines = (el) => elementText(el, true).split("\n").filter(Boolean);

  const headingElements = (el) =>
    Array.from(el.querySelectorAll("h1,h2,h3,h4,h5,h6")).filter(
      (node) => !isIgnoredElement(node) && elementText(node)
    );

  const extractSequentialCard = (el) => {
    const children = visibleChildren(el);
    if (children.length < 2) return null;

    const numberText = elementText(children[0]);
    if (!/^\d+$/.test(numberText)) return null;

    const number = Number.parseInt(numberText, 10);
    const headings = headingElements(el);
    if (headings.length !== 1) return null;
    const heading = headings[0];
    const title = heading ? elementText(heading) : "";
    if (!title) return null;

    const lines = cardLines(el);
    if (lines.length < 2 || lines.length > 4) return null;
    const titleIndex = lines.findIndex((line) => line === title);
    let body = titleIndex >= 0 ? lines.slice(titleIndex + 1).join(" ") : "";
    if (!body) {
      const paragraphs = visibleMatches(el, "p").filter(
        (text) => text !== title && text !== numberText
      );
      body = paragraphs.at(-1) || "";
    }

    return { number, title, body };
  };

  const extractFactCard = (el) => {
    const lines = cardLines(el);
    if (lines.length < 2 || lines.length > 4) return null;
    if (!/^\d+(?:\+)?$/.test(lines[0])) return null;
    return {
      title: lines[1] || "",
      meta: lines[0],
      body: lines.slice(2).join(" ")
    };
  };

  const extractInfoCard = (el) => {
    const headings = headingElements(el);
    if (headings.length !== 1) return null;
    const titleEl = headings[0];

    const title = elementText(titleEl);
    if (!title) return null;

    const lines = cardLines(el);
    if (lines.length < 2 || lines.length > 4) return null;
    const titleIndex = lines.findIndex((line) => line === title);
    if (titleIndex < 0) return null;

    const meta = lines.slice(0, titleIndex).join(" ");
    const body = lines.slice(titleIndex + 1).join(" ");
    if (!meta && !body) return null;

    return { title, meta, body };
  };

  const renderSemanticList = (el, ordered) => {
    const items = Array.from(el.children || [])
      .filter((child) => tagName(child) === "li" && !isIgnoredElement(child))
      .map((child) => ({ body: elementText(child, true) }))
      .filter((item) => item.body);

    if (!items.length) return null;
    return { kind: ordered ? "ordered_list" : "unordered_list", items };
  };

  const renderSequentialGroup = (el) => {
    const children = visibleChildren(el);
    if (children.length < 2) return null;
    const items = children.map(extractSequentialCard);
    if (items.some((item) => !item)) return null;
    if (!items.every((item, index) => item.number === index + 1)) return null;

    return {
      kind: "ordered_list",
      items: items.map((item) => ({ title: item.title, body: item.body }))
    };
  };

  const renderFactGroup = (el) => {
    const children = visibleChildren(el);
    if (children.length < 2) return null;
    const items = children.map(extractFactCard);
    if (items.some((item) => !item)) return null;

    const sequential = items.every((item, index) => Number.parseInt(item.meta, 10) === index + 1);
    if (sequential) return null;

    return { kind: "unordered_list", items };
  };

  const renderInfoGroup = (el) => {
    const children = visibleChildren(el);
    if (children.length < 2) return null;
    const items = children.map(extractInfoCard);
    if (items.some((item) => !item)) return null;
    return { kind: "unordered_list", items };
  };

  const renderStructuredGroup = (el) =>
    renderSequentialGroup(el) || renderFactGroup(el) || renderInfoGroup(el);

  const pushParagraph = (blocks, text) => {
    const normalized = normalizeText(text, true);
    if (normalized) {
      blocks.push({ kind: "paragraph", text: normalized });
    }
  };

  const walk = (node, blocks) => {
    if (!isElement(node) || isIgnoredElement(node)) return;

    const tag = tagName(node);
    if (tag in HEADING_LEVELS) {
      const text = elementText(node, true);
      if (text) {
        blocks.push({ kind: "heading", level: HEADING_LEVELS[tag], text });
      }
      return;
    }

    if (tag === "p") {
      pushParagraph(blocks, elementText(node, true));
      return;
    }

    if (tag === "ul") {
      const block = renderSemanticList(node, false);
      if (block) blocks.push(block);
      return;
    }

    if (tag === "ol") {
      const block = renderSemanticList(node, true);
      if (block) blocks.push(block);
      return;
    }

    if (tag === "li") {
      pushParagraph(blocks, elementText(node, true));
      return;
    }

    const structured = renderStructuredGroup(node);
    if (structured) {
      blocks.push(structured);
      return;
    }

    const children = visibleChildren(node);
    if (!children.length) {
      const text = elementText(node, true);
      if (text && tag !== "img" && tag !== "body") {
        pushParagraph(blocks, text);
      }
      return;
    }

    for (const child of children) {
      walk(child, blocks);
    }
  };

  const blocks = [];
  const roots = Array.from(document.querySelectorAll("main"))
    .filter((root) => !isIgnoredElement(root) && hasMeaningfulText(root));
  const startNodes = roots.length ? roots : visibleChildren(document.body || document.documentElement);
  for (const root of startNodes) {
    walk(root, blocks);
  }

  return { title, url, fallbackText, blocks };
})()"##
}

#[cfg(test)]
mod tests {
    use super::{
        markdown_extract_expression, render_markdown_payload, MarkdownBlock,
        MarkdownExtractPayload, MarkdownListItem,
    };

    #[test]
    fn render_markdown_payload_preserves_heading_spacing() {
        let payload = MarkdownExtractPayload {
            title: "Create Next App".to_string(),
            url: "http://localhost:3000/about".to_string(),
            fallback_text: String::new(),
            blocks: vec![
                MarkdownBlock::Heading {
                    level: 2,
                    text: "Services".to_string(),
                },
                MarkdownBlock::Paragraph {
                    text: "Daycare by Melissa Harding".to_string(),
                },
            ],
        };

        assert_eq!(
            render_markdown_payload(&payload),
            "# Create Next App\n\n_Source_: http://localhost:3000/about\n\n## Services\n\nDaycare by Melissa Harding"
        );
    }

    #[test]
    fn render_markdown_payload_formats_ordered_cards_inline() {
        let payload = MarkdownExtractPayload {
            title: String::new(),
            url: String::new(),
            fallback_text: String::new(),
            blocks: vec![MarkdownBlock::OrderedList {
                items: vec![
                    MarkdownListItem {
                        title: "Get in Touch".to_string(),
                        meta: String::new(),
                        body: "Send us an enquiry with your dog's details.".to_string(),
                    },
                    MarkdownListItem {
                        title: "Meet & Greet".to_string(),
                        meta: String::new(),
                        body: "A mandatory in-person meeting.".to_string(),
                    },
                ],
            }],
        };

        assert_eq!(
            render_markdown_payload(&payload),
            "1. **Get in Touch**\n   Send us an enquiry with your dog's details.\n2. **Meet & Greet**\n   A mandatory in-person meeting."
        );
    }

    #[test]
    fn render_markdown_payload_formats_unordered_cards_with_meta() {
        let payload = MarkdownExtractPayload {
            title: String::new(),
            url: String::new(),
            fallback_text: String::new(),
            blocks: vec![MarkdownBlock::UnorderedList {
                items: vec![
                    MarkdownListItem {
                        title: "Daycare".to_string(),
                        meta: "7:45 AM – 6:00 PM".to_string(),
                        body: "Full-day care in a home environment.".to_string(),
                    },
                    MarkdownListItem {
                        title: "Maximum dogs".to_string(),
                        meta: "4".to_string(),
                        body: "Quality over quantity — always".to_string(),
                    },
                ],
            }],
        };

        assert_eq!(
            render_markdown_payload(&payload),
            "- **Daycare** (7:45 AM – 6:00 PM)\n   Full-day care in a home environment.\n- **Maximum dogs** (4)\n   Quality over quantity — always"
        );
    }

    #[test]
    fn markdown_expression_returns_structured_blocks() {
        let expr = markdown_extract_expression();

        assert!(expr.contains("kind: \"ordered_list\""));
        assert!(expr.contains("kind: \"unordered_list\""));
        assert!(expr.contains("fallbackText"));
    }
}
