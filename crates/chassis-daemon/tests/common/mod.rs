//! Shared helpers for the server integration tests.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chassis_daemon::server::{self, ServerConfig};
use chassis_daemon::{auth, sampler};
use chassis_telemetry_protocol::{
    Availability, AvailabilityStatus, ClientBody, ClientFrame, ClientIdentity, CpuSample,
    DiskSample, ErrorCode, HostIdentity, HostSample, MemorySample, MetricCapabilities,
    NetworkSample, PROTOCOL_MAJOR, ServerBody, ServerFrame,
};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{oneshot, watch};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

pub type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;
pub type SampleTx = watch::Sender<Option<Arc<HostSample>>>;

pub const STEP: Duration = Duration::from_secs(3);

pub fn unavailable() -> Availability {
    Availability {
        status: AvailabilityStatus::Unavailable,
        reason: Some("test".to_string()),
    }
}

pub fn static_info() -> sampler::StaticInfo {
    sampler::StaticInfo {
        host: HostIdentity {
            id: "11111111-1111-4111-8111-111111111111".to_string(),
            hostname: "test-host".to_string(),
            os_name: "TestOS".to_string(),
            os_version: "1".to_string(),
            arch: "arm64".to_string(),
            cpu_brand: "Test CPU".to_string(),
            cpu_cores: 4,
            memory_total_bytes: 1 << 30,
        },
        metrics: MetricCapabilities {
            memory_pressure: None,
            gpu_usage: unavailable(),
            gpu_memory: unavailable(),
            gpu_power: unavailable(),
            gpu_freq: unavailable(),
            temperatures: unavailable(),
        },
    }
}

pub fn sample(n: u64) -> Arc<HostSample> {
    Arc::new(HostSample {
        cpu: CpuSample {
            usage_pct: 12.5,
            per_core_pct: vec![10.0, 15.0],
        },
        memory: MemorySample {
            pressure: None,
            used_bytes: n,
            available_bytes: 1,
            total_bytes: 1 << 30,
            swap_used_bytes: 0,
            swap_total_bytes: 0,
        },
        network: NetworkSample {
            rx_bytes_per_s: 1.5,
            tx_bytes_per_s: 2.5,
            interfaces: vec![],
        },
        disk: DiskSample {
            read_bytes_per_s: 0.0,
            write_bytes_per_s: 0.0,
        },
        gpus: vec![],
        top_processes: vec![],
        uptime_s: 100,
    })
}

pub struct Harness {
    pub addr: SocketAddr,
    pub token: String,
    pub tx: SampleTx,
    pub dir: tempfile::TempDir,
    stop: Option<oneshot::Sender<()>>,
    server: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl Harness {
    pub async fn start(auth_timeout: Duration) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let token = auth::pair(dir.path()).unwrap();
        let (tx, rx) = watch::channel(None);
        Harness::start_with(dir, token, tx, rx, static_info(), 1000, auth_timeout).await
    }

    pub async fn start_with(
        dir: tempfile::TempDir,
        token: String,
        tx: SampleTx,
        rx: watch::Receiver<Option<Arc<HostSample>>>,
        info: sampler::StaticInfo,
        interval_ms: u32,
        auth_timeout: Duration,
    ) -> Harness {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let config = ServerConfig {
            config_dir: dir.path().to_path_buf(),
            info: Arc::new(info),
            interval_ms,
            auth_timeout,
            connection_limit: server::CONNECTION_LIMIT,
            allowed_origins: Vec::new(),
        };
        let (stop, stopped) = oneshot::channel::<()>();
        let server = tokio::spawn(server::serve(listener, config, rx, async move {
            let _ = stopped.await;
        }));
        Harness {
            addr,
            token,
            tx,
            stop: Some(stop),
            server: Some(server),
            dir,
        }
    }

    pub async fn connect(&self) -> Ws {
        let (ws, _) = connect_async(format!("ws://{}/v1/ws", self.addr))
            .await
            .unwrap();
        ws
    }

    /// Connect and authenticate; leaves telemetry unread.
    pub async fn login(&self) -> Ws {
        let mut ws = self.connect().await;
        authenticate(&mut ws, &self.token).await;
        ws
    }

    pub fn publish(&self, n: u64) {
        self.tx.send_replace(Some(sample(n)));
    }

    /// Trigger shutdown and wait for `serve` to return.
    pub async fn shutdown(&mut self) -> Duration {
        let started = Instant::now();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(server) = self.server.take() {
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .expect("serve did not return")
                .unwrap()
                .unwrap();
        }
        started.elapsed()
    }
}

pub async fn send_hello(ws: &mut Ws) -> [u8; 32] {
    let nc = rand::random::<[u8; 32]>();
    let frame = ClientFrame {
        v: PROTOCOL_MAJOR,
        ts_ms: 0,
        body: ClientBody::Hello {
            protocol_major: PROTOCOL_MAJOR,
            nonce: base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, nc),
            client: ClientIdentity {
                name: "test".to_string(),
                version: "0".to_string(),
            },
        },
    };
    ws.send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
        .await
        .unwrap();
    nc
}

pub async fn challenge(ws: &mut Ws) -> ([u8; 32], String) {
    let frame = next_frame(ws).await;
    assert_eq!(frame.seq, 1);
    let ServerBody::AuthChallenge { nonce, proof } = frame.body else {
        panic!("expected auth.challenge");
    };
    (auth::decode(&nonce).unwrap(), proof)
}

pub async fn send_auth(ws: &mut Ws, key: &auth::TokenHash, nc: &[u8; 32], ns: &[u8; 32]) -> String {
    let proof = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        key.proof(auth::Role::Client, nc, ns),
    );
    let frame = ClientFrame {
        v: PROTOCOL_MAJOR,
        ts_ms: 0,
        body: ClientBody::Auth {
            proof: proof.clone(),
        },
    };
    ws.send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
        .await
        .unwrap();
    proof
}

pub async fn authenticate(ws: &mut Ws, token: &str) -> (String, String) {
    let nc = send_hello(ws).await;
    let (ns, server_proof) = challenge(ws).await;
    let key = auth::TokenHash::from_token(token);
    assert!(key.verify(
        auth::Role::Server,
        &nc,
        &ns,
        &auth::decode(&server_proof).unwrap()
    ));
    let client_proof = send_auth(ws, &key, &nc, &ns).await;
    (server_proof, client_proof)
}

/// The next server frame; every frame must pass `parse_lenient`.
pub async fn next_frame(ws: &mut Ws) -> ServerFrame {
    loop {
        let msg = tokio::time::timeout(STEP, ws.next())
            .await
            .expect("timed out waiting for a frame")
            .expect("connection ended")
            .expect("websocket error");
        match msg {
            Message::Text(text) => {
                return ServerFrame::parse_lenient(text.as_str())
                    .unwrap_or_else(|e| panic!("bad frame {text}: {e:?}"));
            }
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("expected a text frame, got {other:?}"),
        }
    }
}

/// Everything until the connection ends: parsed text frames and the close code, if any.
pub async fn drain(ws: &mut Ws) -> (Vec<ServerFrame>, Option<u16>) {
    let mut frames = Vec::new();
    let mut code = None;
    loop {
        match tokio::time::timeout(Duration::from_secs(7), ws.next()).await {
            Err(_) => panic!("connection stayed open"),
            Ok(None | Some(Err(_))) => return (frames, code),
            Ok(Some(Ok(Message::Close(frame)))) => code = frame.map(|f| u16::from(f.code)),
            Ok(Some(Ok(Message::Text(text)))) => {
                frames.push(ServerFrame::parse_lenient(text.as_str()).unwrap());
            }
            Ok(Some(Ok(_))) => {}
        }
    }
}

pub fn used_bytes(frame: &ServerFrame) -> u64 {
    match &frame.body {
        ServerBody::HostSample(s) => s.memory.used_bytes,
        other => panic!("expected host.sample, got {other:?}"),
    }
}

pub fn assert_only_error(frames: &[ServerFrame], expected: ErrorCode) {
    assert_eq!(frames.len(), 1, "{frames:?}");
    match &frames[0].body {
        ServerBody::Error { code, .. } => assert_eq!(*code, expected),
        other => panic!("expected error, got {other:?}"),
    }
}
