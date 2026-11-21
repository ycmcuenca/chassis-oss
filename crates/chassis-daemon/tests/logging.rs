//! Log events, captured in an isolated test binary so the subscriber can also
//! install the `log` bridge used by the daemon for libraries like tungstenite.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose};
use chassis_daemon::auth::{TOKEN_HASH_FILE, TokenHash};
use chassis_daemon::cli::log_filter;
use chassis_telemetry_protocol::{ErrorCode, ServerBody};
use common::*;
use tracing::level_filters::LevelFilter;

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
async fn trace_logs_exclude_pairing_material_and_third_party_trace() {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let writer = Capture(buffer.clone());
    tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_env_filter(log_filter(LevelFilter::TRACE))
        .try_init()
        .unwrap();

    tracing::trace!(target: "chassis_daemon::test", "daemon trace probe");
    tracing::trace!(target: "third_party::test", "library trace probe");
    tracing::debug!(target: "third_party::test", "library debug probe");

    let mut h = Harness::start(Duration::from_secs(5)).await;
    h.publish(1 << 20);
    let mut good = h.connect().await;
    let (server_proof, client_proof) = authenticate(&mut good, &h.token).await;
    next_frame(&mut good).await;
    next_frame(&mut good).await;
    assert!(matches!(
        next_frame(&mut good).await.body,
        ServerBody::HostSample(_)
    ));
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
    let raw_key = hex::decode(hash.trim()).unwrap();
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
        &general_purpose::URL_SAFE_NO_PAD.encode(&raw_key),
        &general_purpose::STANDARD.encode(&raw_key),
        &server_proof,
        &client_proof,
        &bad_server_proof,
        &wrong_proof,
        marker,
    ] {
        assert!(!text.contains(secret), "pairing material appeared in logs");
    }
    let records: Vec<_> = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            fields.next()?; // timestamp
            Some((fields.next()?, fields.next()?.trim_end_matches(':')))
        })
        .collect();
    assert!(records.contains(&("TRACE", "chassis_daemon::test")));
    assert!(records.contains(&("DEBUG", "third_party::test")));
    assert!(
        records
            .iter()
            .all(|(level, target)| { *level != "TRACE" || target.starts_with("chassis_daemon::") }),
        "third-party trace record appeared in logs"
    );
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
