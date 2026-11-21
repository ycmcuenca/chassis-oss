mod common;

use std::net::SocketAddr;
use std::process::Stdio;
use std::time::Duration;

#[cfg(unix)]
use chassis_telemetry_protocol::ServerBody;
use common::{Harness, STEP, Ws, challenge, send_hello};
#[cfg(unix)]
use common::{authenticate, next_frame};
#[cfg(unix)]
use futures_util::future::join_all;
use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::time::Instant;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{
    Error, Message,
    client::IntoClientRequest,
    protocol::frame::{
        Frame,
        coding::{Data, OpCode},
    },
};

struct Daemon {
    child: std::process::Child,
    _dir: tempfile::TempDir,
    addr: SocketAddr,
    #[cfg(unix)]
    token: String,
    #[cfg(unix)]
    log_path: std::path::PathBuf,
}

impl Daemon {
    async fn start(extra: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let _token = chassis_daemon::auth::pair(dir.path()).unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = reservation.local_addr().unwrap();
        drop(reservation);
        let binary = env!("CARGO_BIN_EXE_chassis-daemon");
        #[cfg(unix)]
        let mut command = {
            // Exercise the daemon's startup raise even on hosts with a generous soft limit.
            let mut shell = std::process::Command::new("sh");
            shell.args(["-c", "ulimit -S -n 256 && exec \"$@\"", "sh", binary]);
            shell
        };
        #[cfg(not(unix))]
        let mut command = std::process::Command::new(binary);
        command
            .args(["serve", "--bind", &addr.to_string(), "--interval-ms", "250"])
            .args(extra)
            .env("CHASSIS_DAEMON_CONFIG_DIR", dir.path())
            .env_remove("CHASSIS_TOKEN")
            .stdout(Stdio::null());
        #[cfg(unix)]
        let log_path = dir.path().join("daemon.log");
        #[cfg(unix)]
        command.stderr(std::fs::File::create(&log_path).unwrap());
        let child = command.spawn().unwrap();
        let mut daemon = Self {
            child,
            _dir: dir,
            addr,
            #[cfg(unix)]
            token: _token,
            #[cfg(unix)]
            log_path,
        };
        timeout(STEP, async {
            while TcpStream::connect(addr).await.is_err() {
                assert!(
                    daemon.child.try_wait().unwrap().is_none(),
                    "daemon exited at startup"
                );
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("daemon did not start");
        daemon
    }
}

#[cfg(unix)]
#[tokio::test]
async fn sigint_and_sigterm_close_clients_and_exit_successfully() {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let mut daemon = Daemon::start(&["--log-level", "info"]).await;
        let mut ws = connect(daemon.addr, None).await.unwrap();
        authenticate(&mut ws, &daemon.token).await;
        assert_eq!(next_frame(&mut ws).await.seq, 2);
        // SAFETY: this PID belongs to the child held by `daemon`.
        assert_eq!(
            unsafe { libc::kill(daemon.child.id().try_into().unwrap(), signal) },
            0
        );
        let (_, code) = common::drain(&mut ws).await;
        assert_eq!(code, Some(1001), "signal {signal}");
        let status = timeout(STEP, async {
            loop {
                if let Some(status) = daemon.child.try_wait().unwrap() {
                    break status;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(status.code(), Some(0), "signal {signal}");
        assert!(
            std::fs::read_to_string(&daemon.log_path)
                .unwrap()
                .contains("shutting down")
        );
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn connect(addr: SocketAddr, origin: Option<&str>) -> Result<Ws, Error> {
    let mut request = format!("ws://{addr}/v1/ws").into_client_request()?;
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
    }
    timeout(STEP, connect_async(request))
        .await
        .unwrap()
        .map(|(ws, _)| ws)
}

async fn expect_too_large(
    ws: &mut (impl futures_util::Stream<Item = Result<Message, Error>> + Unpin),
) {
    let message = timeout(STEP, ws.next()).await.unwrap().unwrap().unwrap();
    let Message::Close(Some(frame)) = message else {
        panic!("expected a WebSocket close");
    };
    assert_eq!(u16::from(frame.code), 1009);
}

#[cfg(unix)]
#[tokio::test]
async fn eight_oversized_frames_close_1009_with_bounded_rss() {
    fn rss(pid: u32) -> u64 {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
    let daemon = Daemon::start(&[]).await;
    let clients = join_all((0..8).map(|_| connect(daemon.addr, None))).await;
    let pid = daemon.child.id();
    let before = rss(pid);
    let mut peak = before;
    let sends = join_all(clients.into_iter().map(|client| async {
        let (mut tx, mut rx) = client.unwrap().split();
        let send = async {
            // Early rejection can reset the write before all payload reaches the server.
            let _ = tx
                .send(Message::Text("x".repeat(8 * 1024 * 1024).into()))
                .await;
        };
        let receive = async { expect_too_large(&mut rx).await };
        tokio::pin!(send, receive);
        tokio::select! {
            () = &mut receive => {},
            () = &mut send => receive.await,
        }
    }));
    tokio::pin!(sends);
    loop {
        tokio::select! {
            _ = &mut sends => break,
            () = sleep(Duration::from_millis(5)) => peak = peak.max(rss(pid)),
        }
    }
    let after = rss(pid);
    peak = peak.max(after);
    eprintln!("daemon RSS (KiB): before={before}, peak observed={peak}, after={after}");
    assert!(peak - before < 16 * 1024);
}

#[tokio::test]
async fn fragmented_message_closes_1009() {
    let harness = Harness::start(STEP).await;
    let mut ws = harness.connect().await;
    for (data, final_frame) in [
        (Data::Text, false),
        (Data::Continue, false),
        (Data::Continue, true),
    ] {
        ws.send(Message::Frame(Frame::message(
            vec![b'x'; 6 * 1024],
            OpCode::Data(data),
            final_frame,
        )))
        .await
        .unwrap();
    }
    expect_too_large(&mut ws).await;
}

#[tokio::test]
async fn connection_cap_releases_permit_after_close() {
    let harness = Harness::start(Duration::from_secs(15)).await;
    let mut sockets = Vec::new();
    for _ in 0..64 {
        sockets.push(harness.connect().await);
    }
    assert!(matches!(connect(harness.addr, None).await, Err(Error::Http(r)) if r.status() == 503));
    // The Origin check must precede the permit check even when the server is full.
    assert!(
        matches!(connect(harness.addr, Some("null")).await, Err(Error::Http(r)) if r.status() == 403)
    );
    sockets.pop().unwrap().close(None).await.unwrap();
    timeout(STEP, async {
        loop {
            match connect(harness.addr, None).await {
                Ok(_) => break,
                Err(Error::Http(r)) if r.status() == 503 => sleep(Duration::from_millis(10)).await,
                result => panic!("unexpected upgrade result: {result:?}"),
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn idle_tcp_is_closed_within_six_seconds() {
    let harness = Harness::start(STEP).await;
    let mut socket = TcpStream::connect(harness.addr).await.unwrap();
    let mut byte = [0];
    assert_eq!(
        timeout(Duration::from_secs(6), socket.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
}

#[cfg(unix)]
#[tokio::test]
async fn idle_tcp_sockets_do_not_block_telemetry() {
    let daemon = Daemon::start(&[]).await;
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limit points to a writable rlimit and the hard limit is kept unchanged.
    unsafe {
        assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit), 0);
        if limit.rlim_cur < 4096 {
            limit.rlim_cur = limit.rlim_max.min(4096);
            assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
        }
    }
    let start = Instant::now();
    let mut sockets = Vec::with_capacity(2000);
    for _ in 0..2000 {
        sockets.push(TcpStream::connect(daemon.addr).await.unwrap());
    }
    let mut ws = connect(daemon.addr, None).await.unwrap();
    authenticate(&mut ws, &daemon.token).await;
    loop {
        if matches!(next_frame(&mut ws).await.body, ServerBody::HostSample(_)) {
            break;
        }
    }
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "idle sockets may have expired"
    );
    drop(sockets);
}

#[tokio::test]
async fn origin_allowlist_is_exact_and_native_clients_need_no_origin() {
    let denied = Daemon::start(&[]).await;
    assert!(
        matches!(connect(denied.addr, Some("https://evil.example")).await, Err(Error::Http(r)) if r.status() == 403)
    );
    let allowed = Daemon::start(&[
        "--allow-origin",
        "http://localhost:1420",
        "--allow-origin",
        "https://other.example",
    ])
    .await;
    assert!(
        matches!(connect(allowed.addr, Some("http://localhost:1420/")).await, Err(Error::Http(r)) if r.status() == 403)
    );
    for origin in [
        Some("http://localhost:1420"),
        Some("https://other.example"),
        None,
    ] {
        let mut ws = connect(allowed.addr, origin).await.unwrap();
        send_hello(&mut ws).await;
        challenge(&mut ws).await;
    }
}
