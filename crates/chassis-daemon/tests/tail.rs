//! `tail` against an in-process server.

mod common;

use std::time::Duration;

use chassis_daemon::tail;
use common::Harness;

#[tokio::test]
#[ignore = "handshake lands in 5.2"]
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
    assert!(lines[0].starts_with("#1 hello chassis-daemon "), "{text}");
    assert!(
        lines[1].starts_with("#2 agents.snapshot 0 sessions"),
        "{text}"
    );
    assert!(lines[2].starts_with("#3 host.sample cpu 12.5%"), "{text}");
}

#[tokio::test]
#[ignore = "handshake lands in 5.2"]
async fn tail_with_wrong_token_fails_with_auth_failed() {
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

    assert!(err.to_string().contains("auth_failed"), "{err}");
    assert!(
        String::from_utf8(out)
            .unwrap()
            .starts_with("#1 error auth_failed"),
    );
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
