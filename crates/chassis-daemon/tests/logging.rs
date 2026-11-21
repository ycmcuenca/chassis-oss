//! Log events, captured with a scoped subscriber. Kept in its own test binary: a scoped
//! subscriber is unreliable while other test threads hit the same callsites.

mod common;

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chassis_telemetry_protocol::PROTOCOL_MAJOR;
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
#[ignore = "handshake lands in 5.2"]
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
    let mut good = h.login().await;
    next_frame(&mut good).await;
    drop(good);

    let mut bad = h.connect().await;
    send_hello(&mut bad, "wrong-secret-token", PROTOCOL_MAJOR).await;
    drain(&mut bad).await;

    let deadline = Instant::now() + Duration::from_secs(3);
    let text = loop {
        let text = String::from_utf8_lossy(&buffer.lock().unwrap()).into_owned();
        if text.matches("disconnected").count() >= 2 || Instant::now() > deadline {
            break text;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    h.shutdown().await;
    let text = format!("{text}{}", String::from_utf8_lossy(&buffer.lock().unwrap()));

    assert!(text.contains("connection opened"), "{text}");
    assert!(text.contains("authenticated"), "{text}");
    assert!(text.contains("authentication failed"), "{text}");
    assert!(text.contains("invalid token"), "{text}");
    assert!(text.contains("disconnected"), "{text}");
    assert!(text.contains("shutting down"), "{text}");
    assert!(!text.contains(&h.token), "{text}");
    assert!(!text.contains("wrong-secret-token"), "{text}");
}
