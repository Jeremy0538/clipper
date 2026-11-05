//! Minimal obs-websocket v5 client: identify (with auth), requests, events.

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Event subscriptions: General (1) | Outputs (64) | InputVolumeMeters (65536).
const EVENT_SUBS: u64 = 1 | 64 | 65536;

/// Pseudo event sent on our own broadcast channel when the socket closes.
pub const DISCONNECTED: &str = "_Disconnected";

#[derive(Clone)]
pub struct ObsClient {
    tx: Arc<Mutex<Option<mpsc::UnboundedSender<Message>>>>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    events: broadcast::Sender<Value>,
    next_id: Arc<AtomicU64>,
}

fn sha_b64(s: &str) -> String {
    B64.encode(Sha256::digest(s.as_bytes()))
}

impl ObsClient {
    pub fn new() -> Self {
        Self {
            tx: Arc::default(),
            pending: Arc::default(),
            // Volume meters arrive ~20/s: room enough that a busy listener
            // never loses a "replay saved" or state event behind them.
            events: broadcast::channel(4096).0,
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Events as the `d` object of op 5 (`eventType`, `eventData`).
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    pub async fn is_connected(&self) -> bool {
        self.tx.lock().await.is_some()
    }

    pub async fn connect(&self, port: u16, password: &str) -> Result<()> {
        // Something that accepts the TCP connection but never finishes the
        // handshake (a half-started OBS, another program on the port) must not
        // hang the caller: its own retry/exit checks have to keep running.
        tokio::time::timeout(std::time::Duration::from_secs(10), self.connect_inner(port, password))
            .await
            .context("OBS websocket handshake timed out")?
    }

    async fn connect_inner(&self, port: u16, password: &str) -> Result<()> {
        let (ws, _) = connect_async(format!("ws://127.0.0.1:{port}")).await?;
        let (mut sink, mut stream) = ws.split();

        let hello = read_json(&mut stream).await.context("no Hello")?;
        if hello["op"] != 0 {
            bail!("expected Hello, got {hello}");
        }
        let mut identify = json!({ "rpcVersion": 1, "eventSubscriptions": EVENT_SUBS });
        if let Some(auth) = hello["d"].get("authentication") {
            let salt = auth["salt"].as_str().unwrap_or_default();
            let challenge = auth["challenge"].as_str().unwrap_or_default();
            let secret = sha_b64(&format!("{password}{salt}"));
            identify["authentication"] = sha_b64(&format!("{secret}{challenge}")).into();
        }
        sink.send(Message::Text(json!({ "op": 1, "d": identify }).to_string().into()))
            .await?;
        let ident = read_json(&mut stream).await.context("no Identified")?;
        if ident["op"] != 2 {
            bail!("identify rejected: {ident}");
        }

        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let mine = tx.clone();
        tokio::spawn(async move {
            while let Some(m) = rx.recv().await {
                if sink.send(m).await.is_err() {
                    break;
                }
            }
        });

        let (pending, events, slot) = (self.pending.clone(), self.events.clone(), self.tx.clone());
        tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                let Message::Text(t) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                match v["op"].as_u64() {
                    Some(5) => {
                        let _ = events.send(v["d"].clone());
                    }
                    Some(7) => {
                        if let Some(id) = v["d"]["requestId"].as_str() {
                            if let Some(s) = pending.lock().await.remove(id) {
                                let _ = s.send(v["d"].clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
            // Only if no newer connection took over meanwhile: a late-closing old
            // socket must not wipe the live one.
            let mut slot = slot.lock().await;
            if slot.as_ref().is_some_and(|t| t.same_channel(&mine)) {
                *slot = None;
                drop(slot);
                pending.lock().await.clear();
                let _ = events.send(json!({ "eventType": DISCONNECTED }));
            }
        });

        *self.tx.lock().await = Some(tx);
        Ok(())
    }

    /// Send a request; returns `responseData` (or Null) or an error with OBS's comment.
    pub async fn call(&self, request: &str, data: Value) -> Result<Value> {
        let tx = self.tx.lock().await.clone().ok_or_else(|| anyhow!("OBS is not connected"))?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let (s, r) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), s);
        let msg = json!({ "op": 6, "d": { "requestType": request, "requestId": id, "requestData": data } });
        if let Err(e) = tx.send(Message::Text(msg.to_string().into())) {
            self.pending.lock().await.remove(&id);
            return Err(e.into());
        }
        let d = match tokio::time::timeout(Duration::from_secs(10), r).await {
            Ok(r) => r.map_err(|_| anyhow!("{request}: connection closed"))?,
            Err(_) => {
                self.pending.lock().await.remove(&id);
                bail!("{request}: OBS did not answer");
            }
        };
        let st = &d["requestStatus"];
        if st["result"].as_bool() != Some(true) {
            bail!(
                "{request} failed (code {}): {}",
                st["code"],
                st["comment"].as_str().unwrap_or("")
            );
        }
        Ok(d.get("responseData").cloned().unwrap_or(Value::Null))
    }
}

async fn read_json<S>(stream: &mut S) -> Result<Value>
where
    S: StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(t))) => return Ok(serde_json::from_str(&t)?),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e.into()),
            None => bail!("socket closed"),
        }
    }
}
