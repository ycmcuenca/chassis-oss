//! In-process integration tests for the WebSocket server.

mod common;

use std::time::{Duration, Instant};

use chassis_daemon::{auth, sampler};
use chassis_telemetry_protocol::{AvailabilityStatus, ErrorCode, PROTOCOL_MAJOR, ServerBody};
use common::*;
use futures_util::{SinkExt, future::join_all};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
#[ignore = "handshake lands in 5.2"]
async fn valid_client_gets_hello_snapshot_then_samples() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut ws = h.login().await;

    let hello = next_frame(&mut ws).await;
    assert_eq!(hello.seq, 1);
    match hello.body {
        ServerBody::Hello(hello) => {
            assert_eq!(hello.protocol.major, PROTOCOL_MAJOR);
            assert_eq!(hello.sample_interval_ms, 1000);
            assert_eq!(hello.host.hostname, "test-host");
            assert_eq!(hello.capabilities.agents.len(), 3);
            assert!(hello.capabilities.agents.iter().all(|a| {
                a.availability == AvailabilityStatus::Unavailable
                    && a.reason.as_deref() == Some("agent adapters not installed")
            }));
        }
        other => panic!("expected hello, got {other:?}"),
    }
    let snapshot = next_frame(&mut ws).await;
    assert_eq!(snapshot.seq, 2);
    match snapshot.body {
        ServerBody::AgentsSnapshot { sessions } => assert!(sessions.is_empty()),
        other => panic!("expected agents.snapshot, got {other:?}"),
    }

    h.publish(7);
    let first = next_frame(&mut ws).await;
    assert_eq!(first.seq, 3);
    assert_eq!(used_bytes(&first), 7);
    h.publish(8);
    let second = next_frame(&mut ws).await;
    assert_eq!(second.seq, 4);
    assert_eq!(used_bytes(&second), 8);
}

#[tokio::test]
#[ignore = "handshake lands in 5.2"]
async fn sample_published_before_connecting_is_sent_right_away() {
    let h = Harness::start(Duration::from_secs(5)).await;
    h.publish(42);
    let mut ws = h.login().await;
    next_frame(&mut ws).await;
    next_frame(&mut ws).await;
    assert_eq!(used_bytes(&next_frame(&mut ws).await), 42);
}

#[tokio::test]
#[ignore = "handshake lands in 5.2"]
async fn wrong_token_gets_auth_failed_and_close_4401() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut ws = h.connect().await;
    send_hello(&mut ws, "not-the-token", PROTOCOL_MAJOR).await;
    h.publish(1);
    let (frames, code) = drain(&mut ws).await;
    assert_only_error(&frames, ErrorCode::AuthFailed);
    assert_eq!(code, Some(4401));
}

#[tokio::test]
#[ignore = "handshake lands in 5.2"]
async fn token_is_checked_before_protocol_major() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut ws = h.connect().await;
    send_hello(&mut ws, "not-the-token", 2).await;
    let (frames, code) = drain(&mut ws).await;
    assert_only_error(&frames, ErrorCode::AuthFailed);
    assert_eq!(code, Some(4401));
}

#[tokio::test]
async fn silent_client_is_closed_4401_without_telemetry() {
    let h = Harness::start(Duration::from_millis(300)).await;
    h.publish(1);
    let mut ws = h.connect().await;
    let started = Instant::now();
    let (frames, code) = drain(&mut ws).await;
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_only_error(&frames, ErrorCode::AuthFailed);
    assert_eq!(code, Some(4401));
}

#[tokio::test]
async fn garbage_first_frame_gets_auth_failed_and_close_4400() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut ws = h.connect().await;
    ws.send(Message::Text("definitely not json".into()))
        .await
        .unwrap();
    let (frames, code) = drain(&mut ws).await;
    assert_only_error(&frames, ErrorCode::AuthFailed);
    assert_eq!(code, Some(4400));
}

#[tokio::test]
async fn protocol_mismatch_gets_error_and_close_4426() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut ws = h.connect().await;
    send_hello(&mut ws, &h.token, 2).await;
    let (frames, code) = drain(&mut ws).await;
    assert_only_error(&frames, ErrorCode::ProtocolMismatch);
    assert_eq!(code, Some(4426));
}

#[tokio::test]
#[ignore = "handshake lands in 5.2"]
async fn repairing_takes_effect_without_restart() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let new_token = auth::pair(h.dir.path()).unwrap();

    let mut old = h.connect().await;
    send_hello(&mut old, &h.token, PROTOCOL_MAJOR).await;
    let (_, code) = drain(&mut old).await;
    assert_eq!(code, Some(4401));

    let mut fresh = h.connect().await;
    send_hello(&mut fresh, &new_token, PROTOCOL_MAJOR).await;
    assert_eq!(next_frame(&mut fresh).await.seq, 1);
}

#[tokio::test]
async fn healthz_answers_ok_without_host_data() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut stream = TcpStream::connect(h.addr).await.unwrap();
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    assert_eq!(body.trim(), "ok");
    assert!(!response.contains("test-host"));
}

#[tokio::test]
#[ignore = "handshake lands in 5.2"]
async fn two_clients_have_independent_sequences() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut a = h.login().await;
    assert_eq!(next_frame(&mut a).await.seq, 1);
    assert_eq!(next_frame(&mut a).await.seq, 2);
    h.publish(1);
    assert_eq!(next_frame(&mut a).await.seq, 3);

    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut b = h.login().await;
    let first = next_frame(&mut b).await;
    assert_eq!(first.seq, 1);
    assert!(matches!(first.body, ServerBody::Hello(_)));
    assert_eq!(next_frame(&mut b).await.seq, 2);
    // `b` connected after sample 1, which it still gets as the latest, then sample 2 for both.
    assert_eq!(next_frame(&mut b).await.seq, 3);
    h.publish(2);
    assert_eq!(used_bytes(&next_frame(&mut a).await), 2);
    assert_eq!(used_bytes(&next_frame(&mut b).await), 2);
}

#[tokio::test]
#[ignore = "handshake lands in 5.2"]
async fn eight_concurrent_clients_all_get_telemetry() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut clients = join_all((0..8).map(|_| h.login())).await;
    // Wait until every client is past hello and snapshot, then publish.
    for ws in &mut clients {
        assert_eq!(next_frame(ws).await.seq, 1);
        assert_eq!(next_frame(ws).await.seq, 2);
    }
    h.publish(5);
    for ws in &mut clients {
        let frame = next_frame(ws).await;
        assert_eq!(frame.seq, 3);
        assert_eq!(used_bytes(&frame), 5);
    }
}

#[tokio::test]
#[ignore = "handshake lands in 5.2"]
async fn stalled_client_does_not_delay_another() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut stalled = h.login().await;
    let mut reader = h.login().await;
    for ws in [&mut stalled, &mut reader] {
        next_frame(ws).await;
        next_frame(ws).await;
    }

    let driver = async {
        for n in 1..=10u64 {
            h.publish(n);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    let read_all = async {
        for n in 1..=10u64 {
            let frame = tokio::time::timeout(Duration::from_millis(900), next_frame(&mut reader))
                .await
                .expect("reader was delayed");
            assert_eq!(used_bytes(&frame), n);
        }
    };
    tokio::join!(driver, read_all);

    // The stalled client resumes after 2 s and catches up to the latest sample.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let mut last = 0;
    while last != 10 {
        last = used_bytes(&next_frame(&mut stalled).await);
    }
}

#[tokio::test]
#[ignore = "handshake lands in 5.2"]
async fn real_sampler_frames_all_parse_leniently() {
    let dir = tempfile::tempdir().unwrap();
    let token = auth::pair(dir.path()).unwrap();
    let handle = sampler::spawn(
        Duration::from_millis(250),
        "22222222-2222-4222-8222-222222222222".to_string(),
    )
    .expect("sampler");
    let (tx, _) = watch::channel(None);
    let h = Harness::start_with(
        dir,
        token,
        tx,
        handle.samples.clone(),
        handle.info.clone(),
        250,
        Duration::from_secs(5),
    )
    .await;

    let mut ws = h.login().await;
    let deadline = Instant::now() + Duration::from_millis(1500);
    let mut samples = 0;
    let mut frames = 0;
    while Instant::now() < deadline {
        let Ok(frame) =
            tokio::time::timeout(Duration::from_millis(1600), next_frame(&mut ws)).await
        else {
            break;
        };
        frames += 1;
        if matches!(frame.body, ServerBody::HostSample(_)) {
            samples += 1;
        }
    }
    assert!(frames >= 3, "only {frames} frames");
    assert!(samples >= 1, "no host.sample arrived");
    drop(ws);
    drop(h);
    handle.stop();
}

#[tokio::test]
#[ignore = "handshake lands in 5.2"]
async fn shutdown_closes_clients_with_1001_and_serve_returns() {
    let mut h = Harness::start(Duration::from_secs(5)).await;
    let mut ws = h.login().await;
    next_frame(&mut ws).await;
    next_frame(&mut ws).await;

    let elapsed = h.shutdown().await;
    assert!(elapsed < Duration::from_secs(1), "took {elapsed:?}");
    let (frames, code) = drain(&mut ws).await;
    assert!(frames.is_empty());
    assert_eq!(code, Some(1001));
}
