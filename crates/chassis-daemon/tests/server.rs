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
async fn valid_client_gets_hello_snapshot_then_samples() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut ws = h.login().await;

    let hello = next_frame(&mut ws).await;
    assert_eq!(hello.seq, 2);
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
    assert_eq!(snapshot.seq, 3);
    match snapshot.body {
        ServerBody::AgentsSnapshot { sessions } => assert!(sessions.is_empty()),
        other => panic!("expected agents.snapshot, got {other:?}"),
    }

    h.publish(7);
    let first = next_frame(&mut ws).await;
    assert_eq!(first.seq, 4);
    assert_eq!(used_bytes(&first), 7);
    h.publish(8);
    let second = next_frame(&mut ws).await;
    assert_eq!(second.seq, 5);
    assert_eq!(used_bytes(&second), 8);
}

#[tokio::test]
async fn sample_published_before_connecting_is_sent_right_away() {
    let h = Harness::start(Duration::from_secs(5)).await;
    h.publish(42);
    let mut ws = h.login().await;
    next_frame(&mut ws).await;
    next_frame(&mut ws).await;
    assert_eq!(used_bytes(&next_frame(&mut ws).await), 42);
}

#[tokio::test]
async fn wrong_client_proof_gets_auth_failed_and_close_4401() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut ws = h.connect().await;
    h.publish(1);
    let nc = send_hello(&mut ws).await;
    let (ns, _) = challenge(&mut ws).await;
    send_auth(
        &mut ws,
        &auth::TokenHash::from_token("not-the-token"),
        &nc,
        &ns,
    )
    .await;
    let (frames, code) = drain(&mut ws).await;
    assert_only_error(&frames, ErrorCode::AuthFailed);
    assert_eq!(code, Some(4401));
}

#[tokio::test]
async fn hello_without_auth_uses_the_upgrade_deadline() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut ws = h.connect().await;
    let started = Instant::now();
    tokio::time::sleep(Duration::from_secs(2)).await;
    send_hello(&mut ws).await;
    challenge(&mut ws).await;
    let (frames, code) = drain(&mut ws).await;
    assert_only_error(&frames, ErrorCode::AuthFailed);
    assert_eq!(code, Some(4401));
    assert!(
        (Duration::from_millis(4700)..Duration::from_millis(6500)).contains(&started.elapsed())
    );
}

#[tokio::test]
async fn silent_client_is_closed_4401_without_telemetry() {
    let h = Harness::start(Duration::from_secs(5)).await;
    h.publish(1);
    let mut ws = h.connect().await;
    let started = Instant::now();
    let (frames, code) = drain(&mut ws).await;
    assert!(
        (Duration::from_millis(4700)..Duration::from_millis(6500)).contains(&started.elapsed())
    );
    assert_only_error(&frames, ErrorCode::AuthFailed);
    assert_eq!(code, Some(4401));
}

#[tokio::test]
async fn malformed_first_frames_get_auth_failed_and_close_4400() {
    let h = Harness::start(Duration::from_secs(5)).await;
    for message in [
        Message::Text("definitely not json".into()),
        Message::Text("[]".into()),
        Message::Text(r#"{"v":1,"ts_ms":0,"type":"client.auth","proof":"test"}"#.into()),
        Message::Binary(vec![0].into()),
        Message::Text(r#"{"v":1,"ts_ms":0,"type":"client.hello","protocol_major":1,"nonce":"AA","client":{"name":"test","version":"0"}}"#.into()),
    ] {
        let mut ws = h.connect().await;
        ws.send(message).await.unwrap();
        let (frames, code) = drain(&mut ws).await;
        assert_only_error(&frames, ErrorCode::AuthFailed);
        assert_eq!(code, Some(4400));
    }
}

#[tokio::test]
async fn malformed_second_frame_gets_auth_failed_and_close_4400() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut ws = h.connect().await;
    send_hello(&mut ws).await;
    challenge(&mut ws).await;
    send_hello(&mut ws).await;
    let (frames, code) = drain(&mut ws).await;
    assert_only_error(&frames, ErrorCode::AuthFailed);
    assert_eq!(code, Some(4400));
}

#[tokio::test]
async fn protocol_mismatch_gets_error_and_close_4426() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut ws = h.connect().await;
    ws.send(Message::Text(r#"{"v":2,"ts_ms":0,"type":"client.hello","protocol_major":2,"auth_token":"x","client":{"name":"test","version":"0"}}"#.into())).await.unwrap();
    let (frames, code) = drain(&mut ws).await;
    assert_only_error(&frames, ErrorCode::ProtocolMismatch);
    assert_eq!(code, Some(4426));
}

#[tokio::test]
async fn repairing_takes_effect_without_restart() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let new_token = auth::pair(h.dir.path()).unwrap();

    let mut old = h.connect().await;
    let nc = send_hello(&mut old).await;
    let (ns, _) = challenge(&mut old).await;
    send_auth(&mut old, &auth::TokenHash::from_token(&h.token), &nc, &ns).await;
    let (_, code) = drain(&mut old).await;
    assert_eq!(code, Some(4401));

    let mut fresh = h.connect().await;
    authenticate(&mut fresh, &new_token).await;
    assert_eq!(next_frame(&mut fresh).await.seq, 2);
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
async fn two_clients_have_independent_sequences() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut a = h.login().await;
    assert_eq!(next_frame(&mut a).await.seq, 2);
    assert_eq!(next_frame(&mut a).await.seq, 3);
    h.publish(1);
    assert_eq!(next_frame(&mut a).await.seq, 4);

    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut b = h.login().await;
    let first = next_frame(&mut b).await;
    assert_eq!(first.seq, 2);
    assert!(matches!(first.body, ServerBody::Hello(_)));
    assert_eq!(next_frame(&mut b).await.seq, 3);
    // `b` connected after sample 1, which it still gets as the latest, then sample 2 for both.
    assert_eq!(next_frame(&mut b).await.seq, 4);
    h.publish(2);
    assert_eq!(used_bytes(&next_frame(&mut a).await), 2);
    assert_eq!(used_bytes(&next_frame(&mut b).await), 2);
}

#[tokio::test]
async fn eight_concurrent_clients_all_get_telemetry() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let mut clients = join_all((0..8).map(|_| h.login())).await;
    // Wait until every client is past hello and snapshot, then publish.
    for ws in &mut clients {
        assert_eq!(next_frame(ws).await.seq, 2);
        assert_eq!(next_frame(ws).await.seq, 3);
    }
    h.publish(5);
    for ws in &mut clients {
        let frame = next_frame(ws).await;
        assert_eq!(frame.seq, 4);
        assert_eq!(used_bytes(&frame), 5);
    }
}

#[tokio::test]
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
