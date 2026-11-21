//! Pure rate maths and aggregation over plain data, so tests need no live counters.

use std::collections::HashSet;
use std::time::Duration;

use chassis_telemetry_protocol::{DiskSample, NetworkInterfaceSample, NetworkSample};

/// Bytes per second for `delta_bytes` over `elapsed`; `0.0` when no time has passed.
pub fn rate(delta_bytes: u64, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 {
        return 0.0;
    }
    let value = delta_bytes as f64 / secs;
    if value.is_finite() { value } else { 0.0 }
}

/// Clamps a percentage to 0..=100, mapping non-finite values to 0.
pub fn pct(x: f32) -> f64 {
    let value = f64::from(x);
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

/// One network interface's counters for a single interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetRecord {
    pub name: String,
    pub delta_rx: u64,
    pub delta_tx: u64,
    /// Lifetime received plus transmitted bytes; idle interfaces are not listed.
    pub lifetime_total: u64,
    pub is_loopback: bool,
}

/// Whether an interface name is a loopback device on Linux (`lo`) or macOS (`lo0`).
pub fn is_loopback_name(name: &str) -> bool {
    name == "lo" || name.starts_with("lo0")
}

/// Sums active, non-loopback interfaces and lists them by name with their rates.
pub fn aggregate_network(records: &[NetRecord], elapsed: Duration) -> NetworkSample {
    let mut interfaces: Vec<NetworkInterfaceSample> = records
        .iter()
        .filter(|r| !r.is_loopback && r.lifetime_total > 0)
        .map(|r| NetworkInterfaceSample {
            is_virtual: None,
            name: r.name.clone(),
            rx_bytes_per_s: rate(r.delta_rx, elapsed),
            tx_bytes_per_s: rate(r.delta_tx, elapsed),
        })
        .collect();
    interfaces.sort_by(|a, b| a.name.cmp(&b.name));
    NetworkSample {
        rx_bytes_per_s: interfaces.iter().map(|i| i.rx_bytes_per_s).sum(),
        tx_bytes_per_s: interfaces.iter().map(|i| i.tx_bytes_per_s).sum(),
        interfaces,
    }
}

/// One disk's I/O counters (bytes since the previous refresh).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskRecord {
    pub id: Option<u64>,
    pub name: String,
    pub read: u64,
    pub written: u64,
}

#[derive(Hash, PartialEq, Eq)]
enum DiskKey {
    Id(u64),
    Name(String),
}

/// Sums disk I/O once per device: dedupe by `id`, falling back to the device name.
pub fn aggregate_disk(records: &[DiskRecord], elapsed: Duration) -> DiskSample {
    let mut seen = HashSet::new();
    let (mut read, mut written) = (0u64, 0u64);
    for record in records {
        let key = match record.id {
            Some(id) => DiskKey::Id(id),
            None => DiskKey::Name(record.name.clone()),
        };
        if seen.insert(key) {
            read = read.saturating_add(record.read);
            written = written.saturating_add(record.written);
        }
    }
    DiskSample {
        read_bytes_per_s: rate(read, elapsed),
        write_bytes_per_s: rate(written, elapsed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(name: &str, rx: u64, tx: u64, total: u64, lo: bool) -> NetRecord {
        NetRecord {
            name: name.to_string(),
            delta_rx: rx,
            delta_tx: tx,
            lifetime_total: total,
            is_loopback: lo,
        }
    }

    #[test]
    fn rate_divides_by_measured_elapsed() {
        assert_eq!(rate(1000, Duration::from_millis(500)), 2000.0);
        assert_eq!(rate(1000, Duration::from_secs(2)), 500.0);
    }

    #[test]
    fn zero_elapsed_is_finite_zero() {
        let value = rate(123_456, Duration::ZERO);
        assert!(value.is_finite());
        assert_eq!(value, 0.0);
    }

    #[test]
    fn pct_clamps_and_rejects_non_finite() {
        assert_eq!(pct(150.0), 100.0);
        assert_eq!(pct(-3.0), 0.0);
        assert_eq!(pct(f32::NAN), 0.0);
        assert_eq!(pct(f32::INFINITY), 0.0);
        assert_eq!(pct(42.0), 42.0);
    }

    #[test]
    fn network_skips_loopback_and_idle_and_sorts() {
        let records = vec![
            net("en1", 100, 50, 10, false),
            net("lo0", 9999, 9999, 9999, true),
            net("utun3", 5, 5, 0, false),
            net("en0", 200, 25, 10, false),
        ];
        let sample = aggregate_network(&records, Duration::from_secs(1));
        let names: Vec<_> = sample.interfaces.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["en0", "en1"]);
        assert_eq!(sample.rx_bytes_per_s, 300.0);
        assert_eq!(sample.tx_bytes_per_s, 75.0);
    }

    #[test]
    fn network_with_zero_elapsed_stays_finite() {
        let sample = aggregate_network(&[net("en0", 10, 10, 1, false)], Duration::ZERO);
        assert_eq!(sample.rx_bytes_per_s, 0.0);
        assert_eq!(sample.interfaces[0].tx_bytes_per_s, 0.0);
    }

    #[test]
    fn loopback_names() {
        assert!(is_loopback_name("lo"));
        assert!(is_loopback_name("lo0"));
        assert!(!is_loopback_name("en0"));
        assert!(!is_loopback_name("lo-fake"));
    }

    #[test]
    fn disk_counts_a_shared_id_once_and_dedupes_idless_by_name() {
        let disk = |id, name: &str, read, written| DiskRecord {
            id,
            name: name.to_string(),
            read,
            written,
        };
        let records = vec![
            disk(Some(7), "disk3s1", 100, 1000),
            disk(Some(7), "disk3s5", 100, 1000),
            disk(Some(8), "disk4", 10, 20),
            disk(None, "sda", 1, 2),
            disk(None, "sda", 1, 2),
            disk(None, "sdb", 3, 4),
        ];
        let sample = aggregate_disk(&records, Duration::from_secs(1));
        assert_eq!(sample.read_bytes_per_s, 114.0);
        assert_eq!(sample.write_bytes_per_s, 1026.0);
    }
}
