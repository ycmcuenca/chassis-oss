//! Log events, captured with a scoped subscriber. Kept in its own test binary: a scoped
//! subscriber is unreliable while other test threads hit the same callsites.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chassis_daemon::auth::{TOKEN_HASH_FILE, TokenHash};
use chassis_telemetry_protocol::ErrorCode;
use common::*;

#[derive(Clone)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn logs_connect_auth_and_disconnect_without_the_token() {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let writer = Capture(buffer.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let mut h = Harness::start(Duration::from_secs(5)).await;
    let mut good = h.connect().await;
    let (server_proof, client_proof) = authenticate(&mut good, &h.token).await;
    next_frame(&mut good).await;
    drop(good);

    let mut bad = h.connect().await;
    let nc = send_hello(&mut bad).await;
    let (ns, bad_server_proof) = challenge(&mut bad).await;
    let wrong_proof = send_auth(
        &mut bad,
        &TokenHash::from_token("wrong-secret-token"),
        &nc,
        &ns,
    )
    .await;
    drain(&mut bad).await;

    let file = h.dir.path().join(TOKEN_HASH_FILE);
    let hash = std::fs::read_to_string(&file).unwrap();
    let marker = "synthetic-broken-file-content";
    std::fs::write(&file, marker).unwrap();
    let mut broken = h.connect().await;
    send_hello(&mut broken).await;
    let (frames, code) = drain(&mut broken).await;
    assert_only_error(&frames, ErrorCode::Internal);
    assert_eq!(frames[0].seq, 1);
    assert_eq!(code, Some(1011));

    let deadline = Instant::now() + Duration::from_secs(3);
    let text = loop {
        let text = String::from_utf8_lossy(&buffer.lock().unwrap()).into_owned();
        if text.matches("disconnected").count() >= 3 || Instant::now() > deadline {
            break text;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    h.shutdown().await;
    let text = format!("{text}{}", String::from_utf8_lossy(&buffer.lock().unwrap()));

    for secret in [
        h.token.as_str(),
        "wrong-secret-token",
        hash.trim(),
        &server_proof,
        &client_proof,
        &bad_server_proof,
        &wrong_proof,
        marker,
    ] {
        assert!(!text.contains(secret), "pairing material appeared in logs");
    }
    assert!(text.contains("connection opened"), "{text}");
    assert!(text.contains("authenticated"), "{text}");
    assert!(text.contains("authentication failed"), "{text}");
    assert!(text.contains("invalid proof"), "{text}");
    assert!(text.contains("disconnected"), "{text}");
    assert!(text.contains("shutting down"), "{text}");
    assert!(
        text.lines().any(|line| line.contains("ERROR")
            && line.contains(TOKEN_HASH_FILE)
            && line.contains("malformed")),
        "{text}"
    );
}
