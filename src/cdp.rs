use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use thiserror::Error;
use tokio::sync::{broadcast, oneshot, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Debug, Clone)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

#[derive(Debug, Error)]
pub enum CdpError {
    #[error("websocket error: {0}")]
    WebSocket(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("timeout")]
    Timeout,
}

struct Inner {
    sink: Mutex<
        futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    >,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, CdpError>>>>,
    events: broadcast::Sender<CdpEvent>,
    next_id: AtomicU64,
}

#[derive(Clone)]
pub struct CdpClient {
    inner: Arc<Inner>,
}

impl CdpClient {
    pub async fn connect(ws_url: &str) -> Result<Self, CdpError> {
        let (ws, _resp) = connect_async(ws_url)
            .await
            .map_err(|e| CdpError::WebSocket(e.to_string()))?;

        let (sink, mut stream) = ws.split();
        let (events_tx, _events_rx) = broadcast::channel(512);

        let inner = Arc::new(Inner {
            sink: Mutex::new(sink),
            pending: Mutex::new(HashMap::new()),
            events: events_tx,
            next_id: AtomicU64::new(1),
        });

        let inner_clone = inner.clone();
        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&text) {
                            // Response
                            if let Some(id) = v.get("id").and_then(|x| x.as_u64()) {
                                let result = if v.get("error").is_some() {
                                    Err(CdpError::Protocol(v.get("error").unwrap().to_string()))
                                } else {
                                    Ok(v.get("result").cloned().unwrap_or(Value::Null))
                                };

                                let mut pending = inner_clone.pending.lock().await;
                                if let Some(tx) = pending.remove(&id) {
                                    let _ = tx.send(result);
                                }
                                continue;
                            }

                            // Event
                            if let Some(method) = v.get("method").and_then(|m| m.as_str()) {
                                let params = v.get("params").cloned().unwrap_or(Value::Null);
                                let session_id = v
                                    .get("sessionId")
                                    .and_then(|s| s.as_str())
                                    .map(|s| s.to_string());
                                let _ = inner_clone.events.send(CdpEvent {
                                    method: method.to_string(),
                                    params,
                                    session_id,
                                });
                            }
                        }
                    }
                    Ok(Message::Binary(_)) => {}
                    Ok(Message::Frame(_)) => {}
                    Ok(Message::Ping(_)) => {}
                    Ok(Message::Pong(_)) => {}
                    Ok(Message::Close(_)) => break,
                    Err(_) => break,
                }
            }

            // If the stream ends, fail all pending requests.
            let mut pending = inner_clone.pending.lock().await;
            for (_id, tx) in pending.drain() {
                let _ = tx.send(Err(CdpError::WebSocket("disconnected".to_string())));
            }
        });

        Ok(Self { inner })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.inner.events.subscribe()
    }

    pub async fn call(
        &self,
        method: &str,
        params: Value,
        session_id: Option<&str>,
        timeout: std::time::Duration,
    ) -> Result<Value, CdpError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel::<Result<Value, CdpError>>();

        {
            let mut pending = self.inner.pending.lock().await;
            pending.insert(id, tx);
        }

        let mut msg = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        if let Some(sid) = session_id {
            msg["sessionId"] = Value::String(sid.to_string());
        }

        let text = serde_json::to_string(&msg).map_err(|e| CdpError::Protocol(e.to_string()))?;

        let send_result = {
            let mut sink = self.inner.sink.lock().await;
            sink.send(Message::Text(text)).await
        };
        if let Err(e) = send_result {
            let mut pending = self.inner.pending.lock().await;
            pending.remove(&id);
            return Err(CdpError::WebSocket(e.to_string()));
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_canceled)) => Err(CdpError::WebSocket("response channel closed".to_string())),
            Err(_) => {
                // Timeout — remove pending.
                let mut pending = self.inner.pending.lock().await;
                pending.remove(&id);
                Err(CdpError::Timeout)
            }
        }
    }
}
