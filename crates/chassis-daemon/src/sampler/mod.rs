//! Host metrics sampler: one dedicated thread owns every source and publishes the latest
//! [`HostSample`] on a `tokio::sync::watch` channel.

mod gpu;
mod memory;
mod processes;
mod rates;

use std::collections::HashSet;
use std::fmt::Display;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chassis_telemetry_protocol::{CpuSample, HostIdentity, HostSample, MetricCapabilities};
use sysinfo::{DiskRefreshKind, Disks, Networks, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::sync::watch;

use crate::cli::Error;
use gpu::GpuSource;
use processes::ProcRecord;
use rates::{DiskRecord, NetRecord};

/// What never changes while the daemon runs: `hello.host` and `hello.capabilities.metrics`.
#[derive(Debug, Clone)]
pub struct StaticInfo {
    pub host: HostIdentity,
    pub metrics: MetricCapabilities,
}

/// Receiver of the latest sample; `None` until the first one is published.
pub type SampleRx = watch::Receiver<Option<Arc<HostSample>>>;

/// Sampler status is independent of the latest-value telemetry channel.
#[derive(Clone)]
pub struct Liveness {
    pub ended: watch::Receiver<Option<String>>,
    started: Instant,
    last_ms: Arc<std::sync::atomic::AtomicU64>,
}

impl Liveness {
    pub fn new() -> (watch::Sender<Option<String>>, Self) {
        let (tx, ended) = watch::channel(None);
        (
            tx,
            Self {
                ended,
                started: Instant::now(),
                last_ms: Arc::default(),
            },
        )
    }

    fn elapsed_ms(&self) -> u64 {
        self.started
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    pub fn published(&self) {
        self.last_ms
            .store(self.elapsed_ms(), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn healthy(&self, interval_ms: u32) -> bool {
        self.elapsed_ms()
            .saturating_sub(self.last_ms.load(std::sync::atomic::Ordering::Relaxed))
            < u64::from(interval_ms) * 3
    }
}

struct PublishEnd {
    tx: watch::Sender<Option<String>>,
    reason: String,
}

impl PublishEnd {
    fn run(mut self, sample: impl FnOnce() + std::panic::UnwindSafe) {
        if let Err(payload) = std::panic::catch_unwind(sample) {
            let message = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("non-string panic payload");
            self.reason = format!("sampler thread panicked: {message}");
        }
    }
}

impl Drop for PublishEnd {
    fn drop(&mut self) {
        self.tx.send_replace(Some(std::mem::take(&mut self.reason)));
    }
}

/// A running sampler thread and its published samples.
pub struct SamplerHandle {
    pub info: StaticInfo,
    pub samples: SampleRx,
    pub liveness: Liveness,
    stop_tx: mpsc::Sender<()>,
    join: JoinHandle<()>,
}

impl SamplerHandle {
    /// Signals the thread to stop and waits for it; returns within about one source refresh.
    pub fn stop(self) {
        let SamplerHandle { stop_tx, join, .. } = self;
        let _ = stop_tx.send(());
        drop(stop_tx);
        if join.join().is_err() {
            tracing::warn!("sampler thread panicked");
        }
    }
}

/// Starts the sampler thread. Every source is built inside it; a required source that fails
/// to initialise makes this return `cannot initialise <source>: <error>`.
pub fn spawn(interval: Duration, host_id: String) -> Result<SamplerHandle, Error> {
    let interval = interval.max(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    let (tx, samples) = watch::channel(None);
    let (init_tx, init_rx) = mpsc::channel::<Result<StaticInfo, String>>();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (ended, liveness) = Liveness::new();
    let marks = liveness.clone();

    let join = std::thread::Builder::new()
        .name("chassis-sampler".to_string())
        .spawn(move || {
            let guard = PublishEnd {
                tx: ended,
                reason: "sampler thread stopped".into(),
            };
            guard.run(|| match Sources::init(host_id) {
                Ok((sources, info)) => {
                    marks.published();
                    if init_tx.send(Ok(info)).is_ok() {
                        run(sources, interval, &tx, &stop_rx, &marks);
                    }
                }
                Err(message) => {
                    let _ = init_tx.send(Err(message));
                }
            });
        })?;

    match init_rx.recv() {
        Ok(Ok(info)) => Ok(SamplerHandle {
            info,
            samples,
            liveness,
            stop_tx,
            join,
        }),
        Ok(Err(message)) => {
            let _ = join.join();
            Err(Error::new(message))
        }
        Err(_) => {
            let _ = join.join();
            Err(Error::new("sampler thread exited during start-up"))
        }
    }
}

/// Names the failing source when a required constructor returns `Err`.
fn required<T, E: Display>(name: &str, result: Result<T, E>) -> Result<T, String> {
    result.map_err(|e| format!("cannot initialise {name}: {e}"))
}

/// Logs a failing source once per distinct error kind: `warn` first, `debug` afterwards.
#[derive(Debug, Default)]
pub struct ErrorLog {
    seen: HashSet<(String, String)>,
}

impl ErrorLog {
    /// True the first time this `(source, kind)` pair is seen.
    pub fn first(&mut self, source: &str, kind: &str) -> bool {
        self.seen.insert((source.to_string(), kind.to_string()))
    }

    /// Logs `kind` for `source`, at `warn` only on the first occurrence.
    pub fn report(&mut self, source: &str, kind: &str) {
        if self.first(source, kind) {
            tracing::warn!(
                source,
                error = kind,
                "source failed; leaving its fields out"
            );
        } else {
            tracing::debug!(source, error = kind, "source failed again");
        }
    }
}

fn run(
    mut sources: Sources,
    interval: Duration,
    tx: &watch::Sender<Option<Arc<HostSample>>>,
    stop_rx: &mpsc::Receiver<()>,
    marks: &Liveness,
) {
    let mut next = Instant::now() + interval;
    loop {
        let wait = next.saturating_duration_since(Instant::now());
        match stop_rx.recv_timeout(wait) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            // A message or a dropped handle both mean stop.
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        let sample = Arc::new(sources.tick());
        tx.send_replace(Some(sample));
        marks.published();
        next += interval;
        let now = Instant::now();
        if next <= now {
            // Fell behind (suspend or a slow refresh): resume the schedule, don't burst.
            next = now + interval;
        }
    }
}

struct Sources {
    sys: System,
    nets: Networks,
    disks: Disks,
    gpu: GpuSource,
    log: ErrorLog,
    last: Instant,
    last_uptime: u64,
}

fn or_unknown<E: Display>(what: &str, result: Result<String, E>) -> String {
    result.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "cannot read {what}; reporting \"unknown\"");
        "unknown".to_string()
    })
}

impl Sources {
    fn init(host_id: String) -> Result<(Self, StaticInfo), String> {
        let mut sys = required("System", System::new())?;
        let nets = required("Networks", Networks::new_with_refreshed_list())?;
        let disks = required("Disks", Disks::new_with_refreshed_list())?;
        // Brand and CPU count need one CPU refresh.
        sys.refresh_cpu_all();
        sys.refresh_memory();
        let (gpu, mut metrics) = GpuSource::init();
        metrics.memory_pressure = Some(memory::capability(memory::pressure()));

        let host = HostIdentity {
            id: host_id,
            hostname: or_unknown("hostname", System::host_name()),
            os_name: or_unknown("OS name", System::name()),
            os_version: or_unknown("OS version", System::os_version()),
            arch: System::cpu_arch(),
            cpu_brand: sys
                .cpus()
                .first()
                .map(|c| c.brand().trim().to_string())
                .unwrap_or_default(),
            cpu_cores: u32::try_from(sys.cpus().len()).unwrap_or(u32::MAX),
            memory_total_bytes: sys.total_memory(),
        };

        // Seed the delta counters just before starting the first sampling interval.
        sys.refresh_cpu_usage();
        refresh_processes(&mut sys);
        let sources = Sources {
            sys,
            nets,
            disks,
            gpu,
            log: ErrorLog::default(),
            last: Instant::now(),
            last_uptime: 0,
        };
        Ok((sources, StaticInfo { host, metrics }))
    }

    /// Refreshes every source and builds one sample; rates use the measured elapsed time.
    fn tick(&mut self) -> HostSample {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.nets.refresh(true);
        self.disks
            .refresh_specifics(true, DiskRefreshKind::nothing().with_io_usage());
        refresh_processes(&mut self.sys);
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last);
        self.last = now;

        let net_records: Vec<NetRecord> = self
            .nets
            .iter()
            .map(|(name, data)| {
                let name = name.to_string_lossy().into_owned();
                let is_loopback = rates::is_loopback_name(&name)
                    || (!data.ip_networks().is_empty()
                        && data.ip_networks().iter().all(|n| n.addr.is_loopback()));
                NetRecord {
                    name,
                    delta_rx: data.received(),
                    delta_tx: data.transmitted(),
                    lifetime_total: data
                        .total_received()
                        .saturating_add(data.total_transmitted()),
                    is_loopback,
                }
            })
            .collect();

        let disk_records: Vec<DiskRecord> = self
            .disks
            .iter()
            .map(|disk| {
                let usage = disk.usage();
                DiskRecord {
                    id: disk.id(),
                    name: disk.name().to_string_lossy().into_owned(),
                    read: usage.read_bytes,
                    written: usage.written_bytes,
                }
            })
            .collect();

        let logical_cpus = self.sys.cpus().len();
        let proc_records: Vec<ProcRecord<'_>> = self
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| ProcRecord {
                pid: pid.as_u32(),
                name: p.name().to_string_lossy(),
                cpu_usage: p.cpu_usage(),
                memory: p.memory(),
            })
            .collect();
        let top_processes = processes::top_processes(&proc_records, logical_cpus);
        drop(proc_records);

        match System::uptime() {
            Ok(secs) => self.last_uptime = secs,
            Err(e) => self.log.report("uptime", &e.to_string()),
        }

        HostSample {
            cpu: CpuSample {
                usage_pct: rates::pct(self.sys.global_cpu_usage()),
                per_core_pct: self
                    .sys
                    .cpus()
                    .iter()
                    .map(|c| rates::pct(c.usage()))
                    .collect(),
            },
            memory: memory::sample(&self.sys, memory::pressure().ok().flatten()),
            network: rates::aggregate_network(&net_records, elapsed),
            disk: rates::aggregate_disk(&disk_records, elapsed),
            gpus: self.gpu.sample(&mut self.log),
            top_processes,
            uptime_s: self.last_uptime,
        }
    }
}

fn refresh_processes(sys: &mut System) {
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_cpu().with_memory(),
    );
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    use chassis_telemetry_protocol::{PROTOCOL_MAJOR, ServerBody, ServerFrame};

    use super::*;

    #[test]
    fn required_error_names_the_source() {
        let err = required::<(), _>("Networks", Err("no permission")).unwrap_err();
        assert_eq!(err, "cannot initialise Networks: no permission");
        assert_eq!(required("Disks", Ok::<_, String>(3)), Ok(3));
    }

    #[test]
    fn error_log_warns_once_per_kind() {
        let mut log = ErrorLog::default();
        assert!(log.first("macmon", "boom"));
        assert!(!log.first("macmon", "boom"));
        assert!(log.first("macmon", "other"));
        assert!(log.first("uptime", "boom"));
    }

    fn frame(seq: u64, sample: &HostSample) -> String {
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        serde_json::to_string(&ServerFrame {
            v: PROTOCOL_MAJOR,
            ts_ms,
            seq,
            body: ServerBody::HostSample(sample.clone()),
        })
        .expect("serialise frame")
    }

    #[tokio::test]
    async fn publishes_samples_that_pass_the_protocol_parser() {
        let handle = spawn(Duration::from_millis(250), "test-host".to_string())
            .expect("sampler starts on this host");
        assert!(handle.info.host.cpu_cores > 0);
        assert_eq!(handle.info.host.id, "test-host");
        let mut samples = handle.samples.clone();
        for _ in 0..2 {
            tokio::time::timeout(Duration::from_secs(10), samples.changed())
                .await
                .expect("a sample within 10 s")
                .expect("sampler still running");
            let sample = samples
                .borrow_and_update()
                .clone()
                .expect("a published sample");
            let json = frame(1, &sample);
            ServerFrame::parse_lenient(&json).expect("frame passes parse_lenient");
            assert!(sample.top_processes.len() <= 5);
            assert_eq!(
                sample.cpu.per_core_pct.len() as u32,
                handle.info.host.cpu_cores
            );
        }
        handle.stop();
    }

    #[tokio::test]
    async fn first_sample_reports_cpu_for_a_busy_process() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let stop = Arc::new(AtomicBool::new(false));
        let busy_stop = stop.clone();
        let (ready, started) = mpsc::channel();
        let busy = std::thread::spawn(move || {
            // Ensure the process has accumulated CPU time before the baseline.
            let warmup = Instant::now();
            while warmup.elapsed() < Duration::from_millis(100) {
                std::hint::spin_loop();
            }
            let _ = ready.send(());
            while !busy_stop.load(Ordering::Relaxed) {
                std::hint::spin_loop();
            }
        });
        let result = async {
            started.recv().ok()?;
            let handle = spawn(Duration::from_secs(2), "test-host".into()).ok()?;
            let mut samples = handle.samples.clone();
            let first =
                tokio::time::timeout(Duration::from_secs(10), samples.wait_for(Option::is_some))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .and_then(|sample| sample.clone());
            let cores = handle.info.host.cpu_cores;
            handle.stop();
            first.map(|sample| (sample, cores))
        }
        .await;
        stop.store(true, Ordering::Relaxed);
        busy.join().unwrap();

        let (sample, cores) = result.expect("first sample within 10 s");
        let process = sample
            .top_processes
            .iter()
            .find(|p| p.pid == std::process::id())
            .expect("busy process should be in the first sample");
        assert!(
            process.cpu_pct >= 80.0 / f64::from(cores),
            "{process:?}, {cores} cores"
        );
        assert!(sample.top_processes.iter().all(|p| p.cpu_pct <= 100.0));
    }

    #[test]
    fn stop_returns_promptly_with_a_long_interval() {
        let handle = spawn(Duration::from_secs(60), "test-host".to_string())
            .expect("sampler starts on this host");
        let started = Instant::now();
        handle.stop();
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    /// Writes 64 MiB and checks the disk write rate is within 25% of the true rate.
    /// Run: `cargo test -p chassis-daemon -- --ignored disk_write_rate`.
    #[test]
    #[ignore = "writes 64 MiB and needs a quiet disk"]
    fn disk_write_rate_matches_a_known_write() {
        const BYTES: usize = 64 * 1024 * 1024;
        let (mut sources, _) = Sources::init("test-host".to_string()).expect("sources");
        let _ = sources.tick(); // baseline
        let started = Instant::now();

        let mut file = tempfile::NamedTempFile::new().expect("temp file");
        let chunk = vec![0xA5u8; 1024 * 1024];
        for _ in 0..BYTES / chunk.len() {
            file.write_all(&chunk).expect("write");
        }
        file.as_file().sync_all().expect("sync");
        if let Some(rest) = Duration::from_secs(1).checked_sub(started.elapsed()) {
            std::thread::sleep(rest);
        }

        let sample = sources.tick();
        let elapsed = started.elapsed().as_secs_f64();
        let expected = BYTES as f64 / elapsed;
        let measured = sample.disk.write_bytes_per_s;
        eprintln!("disk write rate: measured {measured:.0} B/s, expected {expected:.0} B/s");
        assert!(
            (measured - expected).abs() <= expected * 0.25,
            "measured {measured}, expected {expected} ±25%"
        );
    }
}
#[test]
fn sampler_end_guard_reports_panic_reason() {
    let (tx, status) = Liveness::new();
    std::thread::spawn(move || {
        PublishEnd {
            tx,
            reason: "sampler thread stopped".into(),
        }
        .run(|| panic!("synthetic sampler panic"));
    })
    .join()
    .unwrap();
    assert_eq!(
        status.ended.borrow().as_deref(),
        Some("sampler thread panicked: synthetic sampler panic")
    );
}
