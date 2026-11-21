//! Top processes by CPU.

use std::borrow::Cow;

use chassis_telemetry_protocol::ProcessSample;

use super::rates::pct;

/// How many processes each sample lists.
pub const TOP_N: usize = 5;

/// One process as read from the OS: `cpu_usage` is relative to a single core.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcRecord<'a> {
    pub pid: u32,
    pub name: Cow<'a, str>,
    pub cpu_usage: f32,
    pub memory: u64,
}

/// Picks the top [`TOP_N`] by CPU; `cpu_pct` is a share of all `logical_cpus` (0–100).
pub fn top_processes(records: &[ProcRecord<'_>], logical_cpus: usize) -> Vec<ProcessSample> {
    let cpus = logical_cpus.max(1) as f32;
    let mut ranked: Vec<&ProcRecord<'_>> = records.iter().collect();
    let key = |r: &ProcRecord<'_>| {
        if r.cpu_usage.is_finite() {
            r.cpu_usage
        } else {
            0.0
        }
    };
    ranked.sort_by(|a, b| key(b).total_cmp(&key(a)).then_with(|| a.pid.cmp(&b.pid)));
    ranked
        .into_iter()
        .take(TOP_N)
        .map(|r| ProcessSample {
            pid: r.pid,
            name: r.name.to_string(),
            cpu_pct: pct(r.cpu_usage / cpus),
            memory_bytes: r.memory,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(pid: u32, cpu: f32) -> ProcRecord<'static> {
        ProcRecord {
            pid,
            name: Cow::Owned(format!("p{pid}")),
            cpu_usage: cpu,
            memory: u64::from(pid) * 1024,
        }
    }

    #[test]
    fn orders_by_cpu_descending_and_truncates_to_five() {
        let records: Vec<_> = (1..=9).map(|i| rec(i, i as f32 * 10.0)).collect();
        let top = top_processes(&records, 100);
        let pids: Vec<_> = top.iter().map(|p| p.pid).collect();
        assert_eq!(pids, [9, 8, 7, 6, 5]);
        assert_eq!(top[0].memory_bytes, 9 * 1024);
        assert_eq!(top[0].name, "p9");
    }

    #[test]
    fn fewer_than_five_returns_all() {
        assert_eq!(top_processes(&[rec(1, 1.0), rec(2, 2.0)], 4).len(), 2);
        assert!(top_processes(&[], 4).is_empty());
    }

    #[test]
    fn four_hundred_percent_on_eight_cpus_reads_fifty() {
        let top = top_processes(&[rec(1, 400.0)], 8);
        assert_eq!(top[0].cpu_pct, 50.0);
    }

    #[test]
    fn share_is_clamped_to_one_hundred() {
        assert_eq!(top_processes(&[rec(1, 1600.0)], 8)[0].cpu_pct, 100.0);
    }

    #[test]
    fn zero_cpus_counts_as_one_and_nan_reads_zero() {
        assert_eq!(top_processes(&[rec(1, 40.0)], 0)[0].cpu_pct, 40.0);
        assert_eq!(top_processes(&[rec(1, f32::NAN)], 4)[0].cpu_pct, 0.0);
    }

    #[test]
    fn ties_break_by_pid_for_stable_output() {
        let top = top_processes(&[rec(5, 10.0), rec(3, 10.0)], 1);
        assert_eq!(top[0].pid, 3);
    }
}
