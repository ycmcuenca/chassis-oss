//! `tail` against an in-process server.

mod common;

use std::time::Duration;
use std::{fs, process::Command};

use chassis_daemon::{auth, tail};
use chassis_telemetry_protocol::{ClientBody, ClientFrame, ServerBody, ServerFrame};
use common::Harness;
use futures_util::{SinkExt, StreamExt};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};

#[tokio::test]
async fn tail_binary_warns_only_for_command_line_token_and_connects() {
    let mut h = Harness::start(Duration::from_secs(5)).await;
    h.publish(1 << 20);
    let url = format!("ws://{}/v1/ws", h.addr);
    for explicit in [true, false] {
        let stdout = tempfile::NamedTempFile::new().unwrap();
        let stderr = tempfile::NamedTempFile::new().unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_chassis-daemon"));
        cmd.args(["tail", "--url", &url])
            .stdout(stdout.reopen().unwrap())
            .stderr(stderr.reopen().unwrap())
            .env("CHASSIS_TOKEN", &h.token);
        if explicit {
            cmd.args(["--token", &h.token]);
        }
        let mut child = cmd.spawn().unwrap();
        let connected = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if fs::read_to_string(stdout.path()).is_ok_and(|text| text.contains("host.sample"))
                {
                    return true;
                }
                if child.try_wait().ok().flatten().is_some() {
                    return false;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or(false);
        let _ = child.kill();
        child.wait().unwrap();
        assert!(connected, "tail did not receive a sample");
        let warning = fs::read_to_string(stderr.path()).unwrap();
        assert_eq!(
            warning.contains("command-line arguments are visible to other users"),
            explicit
        );
        assert!(!warning.contains(&h.token), "tail leaked the token");
    }
    h.shutdown().await;
}

#[tokio::test]
async fn tail_prints_hello_snapshot_and_sample_lines() {
    let h = Harness::start(Duration::from_secs(5)).await;
    h.publish(1 << 20);
    let url = format!("ws://{}/v1/ws", h.addr);

    let mut out: Vec<u8> = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        tail::run(&url, &h.token, &mut out, Some(3)),
    )
    .await
    .expect("tail timed out")
    .expect("tail failed");

    let text = String::from_utf8(out).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines[0].starts_with("#2 hello chassis-daemon "), "{text}");
    assert!(
        lines[1].starts_with("#3 agents.snapshot 0 sessions"),
        "{text}"
    );
    assert!(lines[2].starts_with("#4 host.sample cpu 12.5%"), "{text}");
}

#[tokio::test]
async fn tail_with_wrong_token_cannot_verify_the_host() {
    let h = Harness::start(Duration::from_secs(5)).await;
    let url = format!("ws://{}/v1/ws", h.addr);

    let mut out: Vec<u8> = Vec::new();
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        tail::run(&url, "not-the-token", &mut out, None),
    )
    .await
    .expect("tail timed out")
    .expect_err("tail should be refused");

    assert!(
        err.to_string()
            .contains("the host could not prove this pairing"),
        "{err}"
    );
    assert!(out.is_empty());
}

#[tokio::test]
async fn tail_sends_only_hello_to_an_unproven_host() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/v1/ws", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(socket).await.unwrap();
        let mut received = vec![ws.next().await.unwrap().unwrap()];
        let encode = |bytes: [u8; 32]| {
            base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes)
        };
        let challenge = ServerFrame {
            v: 1,
            ts_ms: 0,
            seq: 1,
            body: ServerBody::AuthChallenge {
                nonce: encode(rand::random()),
                proof: encode([0; 32]),
            },
        };
        ws.send(Message::Text(
            serde_json::to_string(&challenge).unwrap().into(),
        ))
        .await
        .unwrap();
        while let Some(Ok(message)) = ws.next().await {
            received.push(message);
        }
        received
    });
    let dir = tempfile::tempdir().unwrap();
    let token = auth::pair(dir.path()).unwrap();
    let output = tokio::task::spawn_blocking({
        let token = token.clone();
        move || {
            std::process::Command::new(env!("CARGO_BIN_EXE_chassis-daemon"))
                .args(["tail", "--url", &url])
                .env("CHASSIS_TOKEN", token)
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("the host could not prove this pairing")
    );
    assert!(output.stdout.is_empty());
    let received = tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap();
    let [Message::Text(hello)] = received.as_slice() else {
        panic!("expected only a client.hello");
    };
    assert!(matches!(
        ClientFrame::parse(hello.as_str()).unwrap().body,
        ClientBody::Hello { .. }
    ));
    assert!(!hello.contains(&token));
    assert!(!hello.contains(&hex::encode(Sha256::digest(token.as_bytes()))));
    assert!(!hello.contains("proof"));
}

#[tokio::test]
async fn tail_reports_connect_failure() {
    let mut out: Vec<u8> = Vec::new();
    let err = tail::run("ws://127.0.0.1:1/v1/ws", "x", &mut out, None)
        .await
        .expect_err("nothing listens on port 1");
    assert!(
        err.to_string()
            .starts_with("cannot connect to ws://127.0.0.1:1/v1/ws: "),
        "{err}"
    );
}
