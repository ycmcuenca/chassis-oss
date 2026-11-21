//! WebSocket server: `/healthz`, and `/v1/ws` with mutual authentication.

use std::error::Error as _;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header::ORIGIN};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Router};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chassis_telemetry_protocol::{
    AgentCapability, AgentKind, AvailabilityStatus, Capabilities, ClientBody, ClientFrame,
    ClientIdentity, ErrorCode, Hello, PROTOCOL_MAJOR, ParseError, ProtocolVersion, ServerBody,
    ServerFrame, check_protocol_version,
};
use futures_util::{Sink, SinkExt, Stream, StreamExt};
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Error as WsError;
use tracing::{debug, error, info, warn};

use crate::auth::{self, Role, TOKEN_HASH_FILE, TokenHash};
use crate::sampler::{Liveness, SampleRx, StaticInfo};

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
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);
const WS_SIZE_LIMIT: usize = 16 * 1024;
pub const CONNECTION_LIMIT: usize = 64;
pub const PING_INTERVAL: Duration = Duration::from_secs(15);
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(45);
pub const SEND_TIMEOUT: Duration = Duration::from_secs(10);
const CLOSE_STALLED: u16 = 4408;

async fn send<T: Sink<Message> + Unpin>(
    tx: &mut T,
    frame: Message,
    limit: Duration,
) -> Result<(), Stop> {
    match tokio::time::timeout(limit, tx.send(frame)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(Stop::SinkFailed),
        Err(_) => Err(Stop::SendTimeout),
    }
}

async fn close<T: Sink<Message> + Unpin>(tx: &mut T, code: u16, reason: &str, limit: Duration) {
    let _ = send(tx, close_message(code, reason), limit.min(CLOSE_TIMEOUT)).await;
}

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
    pub ping_interval: Duration,
    pub idle_timeout: Duration,
    pub send_timeout: Duration,
    pub connection_limit: usize,
    pub allowed_origins: Vec<String>,
}

#[derive(Clone)]
struct AppState {
    config: Arc<ServerConfig>,
    samples: SampleRx,
    liveness: Liveness,
    shutdown: watch::Receiver<bool>,
    /// Held by every WebSocket task so `serve` can wait for them to finish.
    alive: mpsc::Sender<()>,
    permits: Arc<Semaphore>,
}

/// Build the router. Serve it with `into_make_service_with_connect_info::<SocketAddr>()`.
pub fn router(
    config: ServerConfig,
    samples: SampleRx,
    liveness: Liveness,
    shutdown: watch::Receiver<bool>,
) -> Router {
    let (alive, _) = mpsc::channel(1);
    build(config, samples, liveness, shutdown, alive)
}

fn build(
    config: ServerConfig,
    samples: SampleRx,
    liveness: Liveness,
    shutdown: watch::Receiver<bool>,
    alive: mpsc::Sender<()>,
) -> Router {
    let state = AppState {
        permits: Arc::new(Semaphore::new(config.connection_limit)),
        config: Arc::new(config),
        samples,
        liveness,
        shutdown,
        alive,
    };
    Router::new()
        .route("/healthz", get(health))
        .route("/v1/ws", get(ws_upgrade))
        .with_state(state)
}

async fn health(State(state): State<AppState>) -> (StatusCode, &'static str) {
    if state.liveness.healthy(state.config.interval_ms) {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "sampler stalled")
    }
}

/// Serve until `shutdown` resolves, then close every WebSocket with 1001 and return.
pub async fn serve(
    listener: tokio::net::TcpListener,
    config: ServerConfig,
    samples: SampleRx,
    liveness: Liveness,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let (flag_tx, flag_rx) = watch::channel(false);
    let (alive, mut alive_rx) = mpsc::channel::<()>(1);
    let mut ended = liveness.ended.clone();
    let app = build(config, samples, liveness, flag_rx.clone(), alive);
    let mut shutdown = pin!(shutdown);
    let mut tasks = JoinSet::new();
    let mut http = hyper::server::conn::http1::Builder::new();
    http.timer(TokioTimer::new())
        .header_read_timeout(HEADER_TIMEOUT);
    let failure = loop {
        tokio::select! {
            biased;
            reason = sampler_ended(&mut ended) => break Some(reason),
            () = &mut shutdown => break None,
            _ = tasks.join_next(), if !tasks.is_empty() => {},
            accepted = listener.accept() => {
                let (socket, peer) = match accepted {
                    Ok(pair) => pair,
                    Err(err) => {
                        warn!(%err, "cannot accept connection");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let service = TowerToHyperService::new(app.clone().layer(Extension(ConnectInfo(peer))));
                let http = http.clone();
                let mut stop = flag_rx.clone();
                tasks.spawn(async move {
                    let mut conn = pin!(http.serve_connection(TokioIo::new(socket), service).with_upgrades());
                    let result = tokio::select! {
                        result = conn.as_mut() => result,
                        _ = stop.changed() => {
                            conn.as_mut().graceful_shutdown();
                            conn.await
                        }
                    };
                    if let Err(err) = result {
                        debug!(%peer, %err, "HTTP connection ended");
                    }
                });
            }
        }
    };
    if let Some(reason) = &failure {
        error!(%reason, "shutting down after sampler failure");
    } else {
        info!("shutting down");
    }
    drop(listener);
    let _ = flag_tx.send(true);
    drop(app);
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, async {
        while tasks.join_next().await.is_some() {}
        // Upgraded connections are detached from hyper.
        alive_rx.recv().await;
    })
    .await;
    failure.map_or(Ok(()), |reason| Err(std::io::Error::other(reason)))
}

async fn sampler_ended(ended: &mut watch::Receiver<Option<String>>) -> String {
    match ended.wait_for(Option::is_some).await {
        Ok(reason) => reason.as_ref().unwrap().clone(),
        Err(_) => "sampler status channel closed".into(),
    }
}

async fn daemon_stop(state: &AppState) -> Stop {
    let mut ended = state.liveness.ended.clone();
    let mut shutdown = state.shutdown.clone();
    tokio::select! {
        biased;
        _ = sampler_ended(&mut ended) => Stop::SamplerEnded,
        // A dropped sender also means the daemon is going away.
        _ = shutdown.wait_for(|down| *down) => Stop::Shutdown,
    }
}
async fn ws_upgrade(
    ws: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let mut origins = headers.get_all(ORIGIN).iter();
    if let Some(origin) = origins.next()
        && (origins.next().is_some()
            || !state
                .config
                .allowed_origins
                .iter()
                .any(|allowed| origin.as_bytes() == allowed.as_bytes()))
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(permit) = state.permits.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    ws.max_message_size(WS_SIZE_LIMIT)
        .max_frame_size(WS_SIZE_LIMIT)
        .read_buffer_size(4096)
        .write_buffer_size(4096)
        .max_write_buffer_size(64 * 1024)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            handle_socket(socket, peer, state).await;
        })
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
    TooLarge,
    SendTimeout,
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
            Reject::TooLarge => "message too large",
            Reject::SendTimeout => "send timeout",
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
            Reject::Gone | Reject::TooLarge | Reject::SendTimeout => {
                unreachable!("no error frame for this rejection")
            }
        }
    }
}

async fn auth_frame(
    rx: &mut (impl Stream<Item = Result<Message, axum::Error>> + Unpin),
) -> Result<ClientBody, Reject> {
    let text = loop {
        match rx.next().await {
            Some(Err(err)) if is_capacity_error(&err) => return Err(Reject::TooLarge),
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

fn is_capacity_error(err: &axum::Error) -> bool {
    matches!(
        err.source()
            .and_then(|source| source.downcast_ref::<WsError>()),
        Some(WsError::Capacity(_))
    )
}

async fn handshake(
    tx: &mut (impl Sink<Message, Error = axum::Error> + Unpin),
    rx: &mut (impl Stream<Item = Result<Message, axum::Error>> + Unpin),
    seq: &mut u64,
    config_dir: PathBuf,
    send_timeout: Duration,
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
    send(tx, Message::Text(challenge.into()), send_timeout)
        .await
        .map_err(|err| {
            if err == Stop::SendTimeout {
                Reject::SendTimeout
            } else {
                Reject::Gone
            }
        })?;

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
    let limit = state.config.send_timeout;

    let auth = tokio::time::timeout_at(
        deadline,
        handshake(
            &mut tx,
            &mut rx,
            &mut seq,
            state.config.config_dir.clone(),
            limit,
        ),
    );
    let verdict = tokio::select! {
        biased;
        outcome = daemon_stop(&state) => {
            finish(&mut tx, &mut seq, outcome, limit, peer).await;
            return;
        }
        result = auth => result.unwrap_or(Err(Reject::Timeout)),
    };
    match verdict {
        Err(Reject::Gone) => {}
        Err(Reject::SendTimeout) => close(&mut tx, CLOSE_STALLED, "send timed out", limit).await,
        Err(Reject::TooLarge) => {
            close(&mut tx, 1009, "", limit).await;
        }
        Err(reject) => {
            warn!(%peer, reason = reject.category(), "authentication failed");
            let (code, message, close) = reject.response();
            if let Some(text) = encode(&mut seq, ServerBody::Error { code, message }) {
                let result = send(
                    &mut tx,
                    Message::Text(text.into()),
                    limit.min(CLOSE_TIMEOUT),
                )
                .await;
                if result == Err(Stop::SendTimeout) {
                    self::close(&mut tx, CLOSE_STALLED, "send timed out", limit).await;
                } else {
                    self::close(&mut tx, close, "", limit).await;
                }
            }
        }
        Ok(client) => {
            info!(
                %peer,
                client = ?loggable(&client.name),
                version = ?loggable(&client.version),
                "authenticated"
            );
            session(&mut tx, &mut rx, &mut seq, &state, peer).await;
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
async fn session<T, R>(tx: &mut T, rx: &mut R, seq: &mut u64, state: &AppState, peer: SocketAddr)
where
    T: Sink<Message> + Unpin,
    R: futures_util::Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    let mut samples = state.samples.clone();
    // Start with whatever the sampler published last, instead of waiting a full interval.
    samples.mark_changed();

    let stop = async {
        tokio::select! {
            biased;
            outcome = daemon_stop(state) => outcome,
            outcome = read_until_idle(rx, state.config.idle_timeout) => outcome,
        }
    };
    let mut stop = pin!(stop);
    let mut initial = [
        hello(&state.config),
        ServerBody::AgentsSnapshot { sessions: vec![] },
    ]
    .into_iter();
    let mut pings = tokio::time::interval_at(
        Instant::now() + state.config.ping_interval,
        state.config.ping_interval,
    );
    pings.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let limit = state.config.send_timeout;
    let outcome = loop {
        let frame = if let Some(body) = initial.next() {
            let Some(text) = encode(seq, body) else {
                break Stop::SinkFailed;
            };
            Message::Text(text.into())
        } else {
            tokio::select! {
                biased;
                reason = &mut stop => break reason,
                _ = pings.tick() => Message::Ping(Default::default()),
                changed = samples.changed() => {
                    if changed.is_err() {
                        break Stop::SamplerEnded;
                    }
                    let Some(sample) = samples.borrow_and_update().clone() else { continue };
                    let Some(text) = encode(seq, ServerBody::HostSample((*sample).clone())) else {
                        continue;
                    };
                    Message::Text(text.into())
                }
            }
        };
        tokio::select! {
            biased;
            reason = &mut stop => break reason,
            result = send(tx, frame, limit) => if let Err(reason) = result { break reason },
        }
    };
    finish(tx, seq, outcome, limit, peer).await;
}

async fn finish<T: Sink<Message> + Unpin>(
    tx: &mut T,
    seq: &mut u64,
    outcome: Stop,
    limit: Duration,
    peer: SocketAddr,
) {
    match outcome {
        Stop::Shutdown => close(tx, CLOSE_GOING_AWAY, "daemon shutting down", limit).await,
        Stop::Idle => close(tx, CLOSE_STALLED, "heartbeat timed out", limit).await,
        Stop::SendTimeout => {
            debug!(%peer, "socket send timed out");
            close(tx, CLOSE_STALLED, "send timed out", limit).await;
        }
        Stop::SamplerEnded => {
            if let Some(text) = encode(
                seq,
                ServerBody::Error {
                    code: ErrorCode::Internal,
                    message: "sampler stopped".into(),
                },
            ) && send(tx, Message::Text(text.into()), limit.min(CLOSE_TIMEOUT)).await
                == Err(Stop::SendTimeout)
            {
                close(tx, CLOSE_STALLED, "send timed out", limit).await;
                return;
            }
            close(tx, 1011, "sampler stopped", limit).await;
        }
        Stop::ClientGone | Stop::SinkFailed => {}
    }
}

/// Each received frame, including a Pong, restarts the idle window.
async fn read_until_idle<R>(rx: &mut R, idle: Duration) -> Stop
where
    R: futures_util::Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    loop {
        match tokio::time::timeout(idle, rx.next()).await {
            Err(_) => return Stop::Idle,
            Ok(None | Some(Err(_) | Ok(Message::Close(_)))) => return Stop::ClientGone,
            Ok(Some(Ok(_))) => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Shutdown,
    ClientGone,
    SamplerEnded,
    SinkFailed,
    Idle,
    SendTimeout,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chassis_telemetry_protocol::{
        Availability, CpuSample, DiskSample, HostIdentity, HostSample, MemorySample,
        MetricCapabilities, NetworkSample,
    };
    use std::pin::Pin;
    use std::task::{Context, Poll};

    fn static_info() -> StaticInfo {
        let unavailable = Availability {
            status: AvailabilityStatus::Unavailable,
            reason: None,
        };
        StaticInfo {
            host: HostIdentity {
                id: "11111111-1111-4111-8111-111111111111".into(),
                hostname: "test-host".into(),
                os_name: "test".into(),
                os_version: "1".into(),
                arch: "test".into(),
                cpu_brand: "test".into(),
                cpu_cores: 1,
                memory_total_bytes: 2,
            },
            metrics: MetricCapabilities {
                memory_pressure: None,
                gpu_usage: unavailable.clone(),
                gpu_memory: unavailable.clone(),
                gpu_power: unavailable.clone(),
                gpu_freq: unavailable.clone(),
                temperatures: unavailable,
            },
        }
    }

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

    struct TestSink {
        stall: bool,
        stalled: bool,
        frames: mpsc::UnboundedSender<Message>,
    }

    impl Sink<Message> for TestSink {
        type Error = std::convert::Infallible;

        fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(mut self: Pin<&mut Self>, frame: Message) -> Result<(), Self::Error> {
            self.stalled = self.stall
                && matches!(&frame, Message::Text(text) if text.contains("\"type\":\"host.sample\""));
            let _ = self.frames.send(frame);
            Ok(())
        }

        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            if self.stalled {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        }

        fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.poll_flush(cx)
        }
    }

    #[tokio::test]
    async fn blocked_flush_times_out_without_delaying_second_session() {
        let (publish, samples) = watch::channel(None);
        let (_ended, liveness) = Liveness::new();
        let (_shutdown, shutdown) = watch::channel(false);
        let (alive, _) = mpsc::channel(1);
        let state = AppState {
            config: Arc::new(ServerConfig {
                config_dir: PathBuf::new(),
                info: Arc::new(static_info()),
                interval_ms: 20,
                auth_timeout: Duration::from_secs(1),
                ping_interval: PING_INTERVAL,
                idle_timeout: IDLE_TIMEOUT,
                send_timeout: Duration::from_millis(150),
                connection_limit: 2,
                allowed_origins: vec![],
            }),
            samples,
            liveness,
            shutdown,
            alive,
            permits: Arc::new(Semaphore::new(2)),
        };
        let start = |stall| {
            let state = state.clone();
            let (frames, rx) = mpsc::unbounded_channel();
            let task = tokio::spawn(async move {
                let mut sink = TestSink {
                    stall,
                    stalled: false,
                    frames,
                };
                session(
                    &mut sink,
                    &mut futures_util::stream::pending(),
                    &mut 0,
                    &state,
                    "127.0.0.1:1".parse().unwrap(),
                )
                .await;
            });
            (rx, task)
        };
        let (mut slow, slow_task) = start(true);
        let (mut fast, fast_task) = start(false);
        for rx in [&mut slow, &mut fast] {
            for _ in 0..2 {
                assert!(matches!(rx.recv().await, Some(Message::Text(_))));
            }
        }
        let started = Instant::now();
        for n in 1..=10 {
            publish.send_replace(Some(Arc::new(sample(n))));
            let frame = tokio::time::timeout(Duration::from_millis(100), fast.recv())
                .await
                .unwrap();
            let Some(Message::Text(frame)) = frame else {
                panic!("expected sample");
            };
            assert_eq!(ServerFrame::parse_lenient(&frame).unwrap().seq, n + 2);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(matches!(slow.recv().await, Some(Message::Text(_))));
        assert!(matches!(
            tokio::time::timeout(Duration::from_millis(100), slow.recv())
                .await
                .unwrap(),
            Some(Message::Close(Some(CloseFrame { code: 4408, .. })))
        ));
        slow_task.await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        fast_task.abort();
    }
}
