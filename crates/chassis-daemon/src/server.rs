//! WebSocket server: `/healthz`, and `/v1/ws` with mutual authentication.

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::response::Response;
use axum::routing::get;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chassis_telemetry_protocol::{
    AgentCapability, AgentKind, AvailabilityStatus, Capabilities, ClientBody, ClientFrame,
    ClientIdentity, ErrorCode, Hello, PROTOCOL_MAJOR, ParseError, ProtocolVersion, ServerBody,
    ServerFrame, check_protocol_version,
};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

use crate::auth::{self, Role, TOKEN_HASH_FILE, TokenHash};
use crate::sampler::{SampleRx, StaticInfo};

/// Close code: authentication failed (bad or missing token, or no hello in time).
const CLOSE_AUTH_FAILED: u16 = 4401;
/// Close code: the first frame was not a valid `client.hello`.
const CLOSE_MALFORMED: u16 = 4400;
/// Close code: the client speaks a different protocol major.
const CLOSE_PROTOCOL: u16 = 4426;
/// Close code: going away (daemon shutting down).
const CLOSE_GOING_AWAY: u16 = 1001;

/// How long to wait for a peer to accept a close or error frame.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// How long `serve` waits for open WebSocket tasks after shutdown.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

const AGENTS_UNAVAILABLE: &str = "agent adapters not installed";

pub struct ServerConfig {
    /// Directory holding `token.sha256`; re-read on every auth attempt.
    pub config_dir: PathBuf,
    /// Host identity and metric capabilities.
    pub info: Arc<StaticInfo>,
    /// Announced in `hello.sample_interval_ms`.
    pub interval_ms: u32,
    /// How long a client has to complete authentication after the upgrade.
    pub auth_timeout: Duration,
}

#[derive(Clone)]
struct AppState {
    config: Arc<ServerConfig>,
    samples: SampleRx,
    shutdown: watch::Receiver<bool>,
    /// Held by every WebSocket task so `serve` can wait for them to finish.
    alive: mpsc::Sender<()>,
}

/// Build the router. Serve it with `into_make_service_with_connect_info::<SocketAddr>()`.
pub fn router(config: ServerConfig, samples: SampleRx, shutdown: watch::Receiver<bool>) -> Router {
    let (alive, _) = mpsc::channel(1);
    build(config, samples, shutdown, alive)
}

fn build(
    config: ServerConfig,
    samples: SampleRx,
    shutdown: watch::Receiver<bool>,
    alive: mpsc::Sender<()>,
) -> Router {
    let state = AppState {
        config: Arc::new(config),
        samples,
        shutdown,
        alive,
    };
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/ws", get(ws_upgrade))
        .with_state(state)
}

/// Serve until `shutdown` resolves, then close every WebSocket with 1001 and return.
pub async fn serve(
    listener: tokio::net::TcpListener,
    config: ServerConfig,
    samples: SampleRx,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let (flag_tx, flag_rx) = watch::channel(false);
    let (alive, mut alive_rx) = mpsc::channel::<()>(1);
    let app =
        build(config, samples, flag_rx, alive).into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            info!("shutting down");
            let _ = flag_tx.send(true);
        })
        .await?;
    // Upgraded connections are detached from hyper: wait for their tasks to send Close.
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, alive_rx.recv()).await;
    Ok(())
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, peer, state))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Serialize one server frame, advancing the per-connection sequence number.
fn encode(seq: &mut u64, body: ServerBody) -> Option<String> {
    *seq += 1;
    let frame = ServerFrame {
        v: PROTOCOL_MAJOR,
        ts_ms: now_ms(),
        seq: *seq,
        body,
    };
    match serde_json::to_string(&frame) {
        Ok(text) => Some(text),
        Err(e) => {
            warn!("cannot encode a {} frame: {e}", frame_kind(&frame.body));
            None
        }
    }
}

fn frame_kind(body: &ServerBody) -> &'static str {
    match body {
        ServerBody::AuthChallenge { .. } => "auth.challenge",
        ServerBody::Hello(_) => "hello",
        ServerBody::HostSample(_) => "host.sample",
        ServerBody::AgentsSnapshot { .. } => "agents.snapshot",
        ServerBody::AgentUpsert { .. } => "agent.upsert",
        ServerBody::AgentRemove { .. } => "agent.remove",
        ServerBody::Error { .. } => "error",
    }
}

/// Why a client was turned away before any telemetry.
enum Reject {
    Timeout,
    Malformed,
    InvalidProof,
    ProtocolMismatch,
    Internal,
    Gone,
}

impl Reject {
    fn category(&self) -> &'static str {
        match self {
            Reject::Timeout => "timeout",
            Reject::Malformed => "malformed",
            Reject::InvalidProof => "invalid proof",
            Reject::ProtocolMismatch => "protocol mismatch",
            Reject::Internal => "internal",
            Reject::Gone => "disconnected",
        }
    }

    /// The `error` frame code and message, and the close code.
    fn response(&self) -> (ErrorCode, String, u16) {
        match self {
            Reject::Timeout => (
                ErrorCode::AuthFailed,
                "authentication deadline expired".to_string(),
                CLOSE_AUTH_FAILED,
            ),
            Reject::Malformed => (
                ErrorCode::AuthFailed,
                "invalid authentication frame".to_string(),
                CLOSE_MALFORMED,
            ),
            Reject::InvalidProof => (
                ErrorCode::AuthFailed,
                "invalid client proof".to_string(),
                CLOSE_AUTH_FAILED,
            ),
            Reject::ProtocolMismatch => (
                ErrorCode::ProtocolMismatch,
                format!("daemon speaks protocol {PROTOCOL_MAJOR}"),
                CLOSE_PROTOCOL,
            ),
            Reject::Internal => (
                ErrorCode::Internal,
                "pairing is unavailable".to_string(),
                1011,
            ),
            Reject::Gone => unreachable!("disconnected peer cannot receive a response"),
        }
    }
}

async fn auth_frame(
    rx: &mut (impl Stream<Item = Result<Message, axum::Error>> + Unpin),
) -> Result<ClientBody, Reject> {
    let text = loop {
        match rx.next().await {
            None | Some(Err(_) | Ok(Message::Close(_))) => return Err(Reject::Gone),
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Binary(_))) => return Err(Reject::Malformed),
            Some(Ok(Message::Text(text))) => break text,
        }
    };
    check_protocol_version(text.as_str()).map_err(|error| match error {
        ParseError::ProtocolMismatch => Reject::ProtocolMismatch,
        _ => Reject::Malformed,
    })?;
    ClientFrame::parse(text.as_str())
        .map(|frame| frame.body)
        .map_err(|_| Reject::Malformed)
}

async fn handshake(
    tx: &mut (impl Sink<Message, Error = axum::Error> + Unpin),
    rx: &mut (impl Stream<Item = Result<Message, axum::Error>> + Unpin),
    seq: &mut u64,
    config_dir: PathBuf,
) -> Result<ClientIdentity, Reject> {
    let ClientBody::Hello { nonce, client, .. } = auth_frame(rx).await? else {
        return Err(Reject::Malformed);
    };
    let nc = auth::decode(&nonce).ok_or(Reject::Malformed)?;
    let path = config_dir.join(TOKEN_HASH_FILE);
    let key = match tokio::task::spawn_blocking(move || TokenHash::load(&config_dir)).await {
        Ok(Ok(Some(key))) => key,
        Ok(Ok(None)) => {
            error!(path = %path.display(), kind = "not found", "pairing token file is unusable");
            return Err(Reject::Internal);
        }
        Ok(Err(err)) => {
            error!(%err, "pairing token file is unusable");
            return Err(Reject::Internal);
        }
        Err(err) => {
            error!(path = %path.display(), %err, "pairing token file load task failed");
            return Err(Reject::Internal);
        }
    };
    let ns = rand::random::<[u8; 32]>();
    let challenge = encode(
        seq,
        ServerBody::AuthChallenge {
            nonce: URL_SAFE_NO_PAD.encode(ns),
            proof: URL_SAFE_NO_PAD.encode(key.proof(Role::Server, &nc, &ns)),
        },
    )
    .ok_or(Reject::Internal)?;
    tx.send(Message::Text(challenge.into()))
        .await
        .map_err(|_| Reject::Gone)?;

    let ClientBody::Auth { proof } = auth_frame(rx).await? else {
        return Err(Reject::Malformed);
    };
    let proof = auth::decode(&proof).ok_or(Reject::Malformed)?;
    if !key.verify(Role::Client, &nc, &ns, &proof) {
        return Err(Reject::InvalidProof);
    }
    Ok(client)
}

/// A client-supplied string made safe and short enough to log.
fn loggable(value: &str) -> String {
    value.chars().take(64).collect()
}

async fn handle_socket(socket: WebSocket, peer: SocketAddr, state: AppState) {
    // Dropped when this task ends; `serve` waits for all of them.
    let _alive = state.alive.clone();
    debug!(%peer, "connection opened");
    let deadline = Instant::now() + state.config.auth_timeout;
    let (mut tx, mut rx) = socket.split();
    let mut seq = 0u64;

    let verdict = tokio::time::timeout_at(
        deadline,
        handshake(&mut tx, &mut rx, &mut seq, state.config.config_dir.clone()),
    )
    .await
    .unwrap_or(Err(Reject::Timeout));
    match verdict {
        Err(Reject::Gone) => {}
        Err(reject) => {
            warn!(%peer, reason = reject.category(), "authentication failed");
            let (code, message, close) = reject.response();
            if let Some(text) = encode(&mut seq, ServerBody::Error { code, message }) {
                let reject_send = async {
                    let _ = tx.send(Message::Text(text.into())).await;
                    let _ = tx.send(close_message(close, "")).await;
                };
                let _ = tokio::time::timeout(CLOSE_TIMEOUT, reject_send).await;
            }
        }
        Ok(client) => {
            info!(
                %peer,
                client = ?loggable(&client.name),
                version = ?loggable(&client.version),
                "authenticated"
            );
            session(&mut tx, &mut rx, &mut seq, &state).await;
        }
    }
    info!(%peer, frames_sent = seq, "disconnected");
}

fn close_message(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: reason.to_string().into(),
    }))
}

fn hello(config: &ServerConfig) -> ServerBody {
    ServerBody::Hello(Hello {
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        protocol: ProtocolVersion {
            major: PROTOCOL_MAJOR,
            minor: 0,
        },
        sample_interval_ms: config.interval_ms,
        host: config.info.host.clone(),
        capabilities: Capabilities {
            metrics: config.info.metrics.clone(),
            agents: [AgentKind::ClaudeCode, AgentKind::Opencode, AgentKind::Codex]
                .into_iter()
                .map(|kind| AgentCapability {
                    kind,
                    availability: AvailabilityStatus::Unavailable,
                    reason: Some(AGENTS_UNAVAILABLE.to_string()),
                })
                .collect(),
        },
    })
}

/// An authenticated connection: hello, snapshot, then samples until something ends it.
async fn session<T, R>(tx: &mut T, rx: &mut R, seq: &mut u64, state: &AppState)
where
    T: Sink<Message> + Unpin,
    R: futures_util::Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    let mut samples = state.samples.clone();
    // Start with whatever the sampler published last, instead of waiting a full interval.
    samples.mark_changed();

    let mut shutdown = state.shutdown.clone();
    let stop = async {
        tokio::select! {
            // A dropped sender also means the daemon is going away.
            _ = shutdown.wait_for(|down| *down) => Stop::Shutdown,
            () = until_client_gone(rx) => Stop::ClientGone,
        }
    };
    let mut stop = pin!(stop);

    let mut text_tx = (&mut *tx).with(|text: String| {
        futures_util::future::ready(Ok::<_, T::Error>(Message::Text(text.into())))
    });
    for body in [
        hello(&state.config),
        ServerBody::AgentsSnapshot { sessions: vec![] },
    ] {
        let Some(text) = encode(seq, body) else {
            return;
        };
        let sent = tokio::select! {
            sent = text_tx.send(text) => sent.is_ok(),
            _ = &mut stop => false,
        };
        if !sent {
            return;
        }
    }

    let outcome = pump(&mut text_tx, &mut samples, seq, &mut stop).await;
    drop(text_tx);
    if outcome == Stop::Shutdown {
        let close = tx.send(close_message(CLOSE_GOING_AWAY, "daemon shutting down"));
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, close).await;
    }
}

/// Returns when the peer closes or the socket fails; everything else it sends is ignored.
async fn until_client_gone<R>(rx: &mut R)
where
    R: futures_util::Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    loop {
        match rx.next().await {
            None | Some(Err(_) | Ok(Message::Close(_))) => return,
            Some(Ok(_)) => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Shutdown,
    ClientGone,
    SamplerEnded,
    SinkFailed,
}

/// Send one `host.sample` frame per published sample until `stop` resolves.
///
/// A slow sink only ever delays this connection: the watch channel keeps the latest value, so
/// when the sink catches up the next frame is the newest sample, not a backlog.
async fn pump<S, F>(sink: &mut S, samples: &mut SampleRx, seq: &mut u64, stop: &mut F) -> Stop
where
    S: Sink<String> + Unpin,
    F: Future<Output = Stop> + Unpin,
{
    loop {
        tokio::select! {
            reason = &mut *stop => return reason,
            changed = samples.changed() => {
                if changed.is_err() {
                    return Stop::SamplerEnded;
                }
                let Some(sample) = samples.borrow_and_update().clone() else { continue };
                let Some(text) = encode(seq, ServerBody::HostSample((*sample).clone())) else {
                    continue;
                };
                tokio::select! {
                    sent = sink.send(text) => {
                        if sent.is_err() {
                            return Stop::SinkFailed;
                        }
                    }
                    reason = &mut *stop => return reason,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chassis_telemetry_protocol::{
        CpuSample, DiskSample, HostSample, MemorySample, NetworkSample,
    };
    use std::sync::Mutex;

    fn sample(n: u64) -> HostSample {
        HostSample {
            cpu: CpuSample {
                usage_pct: 1.0,
                per_core_pct: vec![1.0],
            },
            memory: MemorySample {
                pressure: None,
                used_bytes: n,
                available_bytes: 1,
                total_bytes: 2,
                swap_used_bytes: 0,
                swap_total_bytes: 0,
            },
            network: NetworkSample {
                rx_bytes_per_s: 0.0,
                tx_bytes_per_s: 0.0,
                interfaces: vec![],
            },
            disk: DiskSample {
                read_bytes_per_s: 0.0,
                write_bytes_per_s: 0.0,
            },
            gpus: vec![],
            top_processes: vec![],
            uptime_s: 1,
        }
    }

    #[tokio::test]
    async fn slow_sink_skips_to_the_latest_sample() {
        let (tx, mut rx) = watch::channel(None);
        let received: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = received.clone();
        let mut sink = Box::pin(futures_util::sink::unfold((), move |(), text: String| {
            let log = log.clone();
            async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                log.lock().unwrap().push(text);
                Ok::<_, std::convert::Infallible>(())
            }
        }));

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let publisher = tokio::spawn(async move {
            for n in 1..=10u64 {
                tx.send_replace(Some(Arc::new(sample(n))));
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            // Let the sink drain the newest sample before stopping.
            tokio::time::sleep(Duration::from_millis(800)).await;
            let _ = stop_tx.send(());
            tx
        });
        let mut stop = pin!(async move {
            let _ = stop_rx.await;
            Stop::ClientGone
        });

        let mut seq = 0;
        let outcome = pump(&mut sink, &mut rx, &mut seq, &mut stop).await;
        let _tx = publisher.await.unwrap();
        assert_eq!(outcome, Stop::ClientGone);

        let frames: Vec<ServerFrame> = received
            .lock()
            .unwrap()
            .iter()
            .map(|t| ServerFrame::parse_lenient(t).unwrap())
            .collect();
        assert!(frames.len() < 10, "sent {} frames", frames.len());
        assert!(frames.len() >= 2);
        assert!(frames.windows(2).all(|w| w[0].seq < w[1].seq));
        let used: Vec<u64> = frames
            .iter()
            .map(|f| match &f.body {
                ServerBody::HostSample(s) => s.memory.used_bytes,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(used.last(), Some(&10));
    }
}
