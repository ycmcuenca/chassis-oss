//! Debug tail client: connect, authenticate and print one compact line per frame.

use std::fmt::Write as _;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use chassis_telemetry_protocol::{
    Availability, ClientBody, ClientFrame, ClientIdentity, GpuSample, HostSample, PROTOCOL_MAJOR,
    ParseError, ServerBody, ServerFrame,
};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::cli::Error;

const CLIENT_NAME: &str = "chassis-daemon-tail";
const TOP_SHOWN: usize = 5;

/// The `snake_case` wire name of an enum value.
fn wire_name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => "?".to_string(),
    }
}

/// Bytes as `B`, `K`, `M` or `G` (base 1024), one decimal below 10.
fn bytes(value: f64) -> String {
    const UNITS: [(f64, &str); 3] = [(1073741824.0, "G"), (1048576.0, "M"), (1024.0, "K")];
    let (scaled, unit) = UNITS
        .iter()
        .find(|(size, _)| value >= *size)
        .map_or((value, "B"), |(size, unit)| (value / size, *unit));
    if unit != "B" && scaled < 9.95 {
        format!("{scaled:.1}{unit}")
    } else {
        format!("{scaled:.0}{unit}")
    }
}

/// Bytes in gibibytes with one decimal, without a unit.
fn gib(value: u64) -> String {
    format!("{:.1}", value as f64 / 1073741824.0)
}

fn availability(a: &Availability) -> String {
    wire_name(&a.status)
}

fn gpu(g: &GpuSample) -> String {
    let mut out = g.name.clone();
    if let Some(v) = g.usage_pct {
        let _ = write!(out, " {v:.0}%");
    }
    if let Some(used) = g.vram_used_bytes {
        match g.vram_total_bytes {
            Some(total) => {
                let _ = write!(out, " vram {}/{}G", gib(used), gib(total));
            }
            None => {
                let _ = write!(out, " vram {}", bytes(used as f64));
            }
        }
    }
    if let Some(v) = g.freq_mhz {
        let _ = write!(out, " {v:.0}MHz");
    }
    if let Some(v) = g.power_w {
        let _ = write!(out, " {v:.2}W");
    }
    if let Some(v) = g.temp_c {
        let _ = write!(out, " {v:.0}C");
    }
    out
}

fn host_sample(s: &HostSample) -> String {
    let mut out = format!(
        "host.sample cpu {:.1}% mem {}/{}G net v{} ^{} disk r{} w{}",
        s.cpu.usage_pct,
        gib(s.memory.used_bytes),
        gib(s.memory.total_bytes),
        bytes(s.network.rx_bytes_per_s),
        bytes(s.network.tx_bytes_per_s),
        bytes(s.disk.read_bytes_per_s),
        bytes(s.disk.write_bytes_per_s),
    );
    if !s.gpus.is_empty() {
        let gpus: Vec<String> = s.gpus.iter().map(gpu).collect();
        let _ = write!(out, " gpu {}", gpus.join(" | "));
    }
    if !s.top_processes.is_empty() {
        out.push_str(" top");
        for p in s.top_processes.iter().take(TOP_SHOWN) {
            let _ = write!(out, " {} {:.1}%", p.name, p.cpu_pct);
        }
    }
    out
}

/// One compact line (without a trailing newline) describing a frame.
pub fn format_frame(frame: &ServerFrame) -> String {
    let body = match &frame.body {
        ServerBody::Hello(h) => {
            let m = &h.capabilities.metrics;
            format!(
                "hello chassis-daemon {} protocol {}.{} host {} {} {} {} \"{}\" {} cpus {}G \
                 interval {}ms gpu_usage={} gpu_memory={} gpu_power={} gpu_freq={} \
                 temperatures={}",
                h.daemon_version,
                h.protocol.major,
                h.protocol.minor,
                h.host.hostname,
                h.host.os_name,
                h.host.os_version,
                h.host.arch,
                h.host.cpu_brand,
                h.host.cpu_cores,
                gib(h.host.memory_total_bytes),
                h.sample_interval_ms,
                availability(&m.gpu_usage),
                availability(&m.gpu_memory),
                availability(&m.gpu_power),
                availability(&m.gpu_freq),
                availability(&m.temperatures),
            )
        }
        ServerBody::HostSample(s) => host_sample(s),
        ServerBody::AgentsSnapshot { sessions } => {
            format!("agents.snapshot {} sessions", sessions.len())
        }
        ServerBody::AgentUpsert { session } => {
            format!("agent.upsert {} {}", session.id, wire_name(&session.state))
        }
        ServerBody::AgentRemove { id } => format!("agent.remove {id}"),
        ServerBody::Error { code, message } => format!("error {}: {message}", wire_name(code)),
    };
    format!("#{} {body}", frame.seq)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(0))
}

fn emit(out: &mut impl Write, line: &str) -> Result<(), Error> {
    writeln!(out, "{line}")?;
    out.flush()?;
    Ok(())
}

/// Connect, authenticate and print every frame until the daemon closes the connection.
///
/// `max_frames` stops after that many printed lines; it exists for tests.
pub async fn run(
    url: &str,
    token: &str,
    out: &mut (impl Write + Send),
    max_frames: Option<usize>,
) -> Result<(), Error> {
    let (mut ws, _) = connect_async(url)
        .await
        .map_err(|e| Error::new(format!("cannot connect to {url}: {e}")))?;

    let hello = ClientFrame {
        v: PROTOCOL_MAJOR,
        ts_ms: now_ms(),
        body: ClientBody::Hello {
            protocol_major: PROTOCOL_MAJOR,
            token: token.to_string(),
            client: ClientIdentity {
                name: CLIENT_NAME.to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        },
    };
    let text = serde_json::to_string(&hello)
        .map_err(|e| Error::new(format!("cannot encode client.hello: {e}")))?;
    ws.send(Message::Text(text.into()))
        .await
        .map_err(|e| Error::new(format!("cannot send client.hello: {e}")))?;

    let mut printed = 0usize;
    while let Some(message) = ws.next().await {
        let message = message.map_err(|e| Error::new(format!("connection failed: {e}")))?;
        match message {
            Message::Text(text) => {
                match ServerFrame::parse_lenient(text.as_str()) {
                    Ok(frame) => {
                        emit(out, &format_frame(&frame))?;
                        if let ServerBody::Error { code, message } = &frame.body {
                            return Err(Error::new(format!(
                                "daemon refused the connection: {}: {message}",
                                wire_name(code)
                            )));
                        }
                    }
                    Err(ParseError::UnknownType { type_name }) => {
                        emit(out, &format!("? {type_name}"))?;
                    }
                    Err(ParseError::Invalid { reason }) => {
                        emit(out, &format!("! invalid frame: {reason}"))?;
                    }
                }
                printed += 1;
                if max_frames.is_some_and(|max| printed >= max) {
                    return Ok(());
                }
            }
            Message::Close(frame) => {
                let (code, reason) = frame.map_or((CloseCode::Status, String::new()), |f| {
                    (f.code, f.reason.to_string())
                });
                return match code {
                    CloseCode::Normal | CloseCode::Away => {
                        emit(out, &format!("closed: {reason}"))?;
                        Ok(())
                    }
                    other => Err(Error::new(format!(
                        "connection closed: {} {reason}",
                        u16::from(other)
                    ))),
                };
            }
            // Ping, pong and binary frames carry nothing for the operator.
            _ => {}
        }
    }
    Err(Error::new("connection closed without a close frame"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(fixture: &str) -> String {
        let frame = ServerFrame::parse_lenient(fixture).expect("fixture parses");
        format_frame(&frame)
    }

    #[test]
    fn hello_line() {
        let text = include_str!("../../../protocol/fixtures/valid/hello.json");
        assert_eq!(
            line(text),
            "#1 hello chassis-daemon 0.1.0 protocol 1.0 host workstation Linux 6.12 x86_64 \
             \"AMD Ryzen 5 5600\" 6 cpus 32.0G interval 1000ms gpu_usage=available \
             gpu_memory=available gpu_power=unavailable gpu_freq=available \
             temperatures=unavailable"
        );
    }

    #[test]
    fn host_sample_full_line() {
        let text = include_str!("../../../protocol/fixtures/valid/host-sample-full.json");
        assert_eq!(
            line(text),
            "#2 host.sample cpu 25.0% mem 8.0/32.0G net v24K ^8.0K disk r256K w64K \
             gpu Radeon RX 6600 42% vram 2.0/8.0G 1750MHz 64.50W 58C top codex 12.5%"
        );
    }

    #[test]
    fn host_sample_without_gpu_omits_gpu_part() {
        let text = include_str!("../../../protocol/fixtures/valid/host-sample-no-gpu.json");
        let line = line(text);
        assert!(!line.contains(" gpu "), "{line}");
        assert!(line.ends_with(" top codex 12.5%"), "{line}");
    }

    #[test]
    fn host_sample_partial_gpu_omits_absent_fields() {
        let text = include_str!("../../../protocol/fixtures/valid/host-sample-partial-gpu.json");
        assert!(line(text).contains(" gpu Radeon RX 6600 42% top codex"));
    }

    #[test]
    fn top_lists_at_most_five_processes() {
        let mut frame = ServerFrame::parse_lenient(include_str!(
            "../../../protocol/fixtures/valid/host-sample-full.json"
        ))
        .unwrap();
        if let ServerBody::HostSample(s) = &mut frame.body {
            let first = s.top_processes[0].clone();
            s.top_processes = (0..7)
                .map(|i| chassis_telemetry_protocol::ProcessSample {
                    name: format!("p{i}"),
                    ..first.clone()
                })
                .collect();
        }
        let line = format_frame(&frame);
        assert!(line.ends_with(" top p0 12.5% p1 12.5% p2 12.5% p3 12.5% p4 12.5%"));
    }

    #[test]
    fn agent_lines() {
        let snapshot = include_str!("../../../protocol/fixtures/valid/agents-snapshot.json");
        assert_eq!(line(snapshot), "#3 agents.snapshot 2 sessions");
        let upsert = include_str!("../../../protocol/fixtures/valid/agent-upsert.json");
        assert_eq!(
            line(upsert),
            "#4 agent.upsert claude_code:claude-01 working"
        );
        let remove = include_str!("../../../protocol/fixtures/valid/agent-remove.json");
        assert_eq!(line(remove), "#5 agent.remove codex:codex-01");
    }

    #[test]
    fn error_line() {
        let text = include_str!("../../../protocol/fixtures/valid/error-protocol-mismatch.json");
        assert_eq!(
            line(text),
            "#1 error protocol_mismatch: Client protocol major 0 is not supported; expected 1"
        );
    }

    #[test]
    fn byte_formatting() {
        assert_eq!(bytes(0.0), "0B");
        assert_eq!(bytes(1023.0), "1023B");
        assert_eq!(bytes(1024.0), "1.0K");
        assert_eq!(bytes(12.0 * 1024.0), "12K");
        assert_eq!(bytes(1.2 * 1048576.0), "1.2M");
        assert_eq!(bytes(9.99 * 1024.0), "10K");
        assert_eq!(bytes(3.5 * 1073741824.0), "3.5G");
        assert_eq!(gib(16106127360), "15.0");
    }
}
