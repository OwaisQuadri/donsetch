//! Minimal CDP client — JSON over WebSocket.
//!
//! Contract: Target / Page / Network / Storage / DOM / Input
//! domains ONLY. Never Runtime, never Console, never
//! Debugger, never script injection. The CDP serialization
//! trap (console.log of an object with a stack getter) only
//! fires when the Runtime domain is enabled — we never
//! enable it, so the trap is dead.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

use crate::error::FetchError;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct Cdp {
    write: Mutex<futures_util::stream::SplitSink<Ws, Message>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    /// Event stream (targetInfoChanged = title/url
    /// changes — challenge progression without Runtime).
    /// Consumed by the daemon's smarter wait loop.
    #[allow(dead_code)]
    events: broadcast::Sender<Value>,
    next_id: AtomicU64,
}

impl Cdp {
    /// Connect to a browser-level ws endpoint and spawn the
    /// demux reader task.
    pub async fn connect(ws_url: &str) -> Result<Self, FetchError> {
        // The only unguarded network primitive in the ghost stack —
        // a browser that accepts TCP but stalls the WS handshake
        // would hang the tool call forever.
        let (ws, _) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio_tungstenite::connect_async(ws_url),
        )
        .await
        .map_err(|_| FetchError::ghost("cdp connect: ws handshake timeout"))?
        .map_err(|e| FetchError::ghost(format!("cdp connect: {e}")))?;
        let (write, mut read) = ws.split();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (events_tx, _) = broadcast::channel(256);
        let pending_task = Arc::clone(&pending);
        let events_task = events_tx.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = read.next().await {
                let Message::Text(text) = msg else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if let Some(id) = v.get("id").and_then(Value::as_u64) {
                    let mut map = pending_task.lock().await;
                    if let Some(tx) = map.remove(&id) {
                        let _ = tx.send(v);
                    }
                } else {
                    let _ = events_task.send(v);
                }
            }
        });
        Ok(Self {
            write: Mutex::new(write),
            pending,
            events: events_tx,
            next_id: AtomicU64::new(1),
        })
    }

    /// Call a method. `session` scopes it to an attached
    /// target (page); None = browser-level.
    pub async fn call(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, FetchError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        {
            let mut w = self.write.lock().await;
            w.send(Message::Text(msg.to_string().into()))
                .await
                .map_err(|e| FetchError::ghost(format!("cdp send: {e}")))?;
        }
        let resp = tokio::time::timeout(std::time::Duration::from_secs(20), rx)
            .await
            .map_err(|_| FetchError::ghost(format!("cdp timeout: {method}")))?
            .map_err(|_| FetchError::ghost(format!("cdp dropped: {method}")))?;
        if let Some(err) = resp.get("error") {
            return Err(FetchError::ghost(format!(
                "cdp {method}: {}",
                err.get("message").and_then(Value::as_str).unwrap_or("?")
            )));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Subscribe to CDP events (targetInfoChanged, loadEvent).
    #[allow(dead_code)] // daemon wait loop (MCP milestone)
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }
}
