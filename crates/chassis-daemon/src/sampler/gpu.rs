//! GPU data: sysinfo `Gpus`, plus Apple Silicon counters from macmon when available.

use chassis_telemetry_protocol::{Availability, AvailabilityStatus, GpuSample, MetricCapabilities};
use sysinfo::Gpus;

use super::ErrorLog;
use super::rates::pct;

const ONE_GPU_REASON: &str = "needs exactly one GPU to attribute Apple Silicon counters";

/// One GPU as sysinfo reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct GpuReading {
    pub name: String,
    pub vendor: Option<String>,
    pub usage_pct: Option<f64>,
    pub vram_used_bytes: Option<u64>,
    pub vram_total_bytes: Option<u64>,
}

/// Apple Silicon counters for the integrated GPU; non-finite values are already dropped.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AppleReading {
    pub freq_mhz: Option<f64>,
    pub power_w: Option<f64>,
    pub temp_c: Option<f64>,
}

/// A source of Apple Silicon counters; an `Err` leaves those fields out of one interval.
pub trait AppleSource {
    /// Takes one non-blocking reading.
    fn read(&mut self) -> Result<AppleReading, String>;
}

/// Outcome of initialising the Apple Silicon counters.
#[derive(Debug, Clone, PartialEq)]
pub enum AppleStatus {
    /// The source works; holds its first reading (only built on Apple Silicon macOS).
    #[cfg_attr(
        not(all(target_os = "macos", target_arch = "aarch64")),
        allow(dead_code)
    )]
    Ready(AppleReading),
    /// The source is missing or failed; holds the reason.
    Unavailable(String),
}

#[cfg_attr(
    not(all(target_os = "macos", target_arch = "aarch64")),
    allow(dead_code)
)]
fn finite(x: f64) -> Option<f64> {
    x.is_finite().then_some(x)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod apple {
    use super::{AppleReading, AppleSource, AppleStatus, finite};

    struct Macmon(macmon::Sampler);

    impl AppleSource for Macmon {
        fn read(&mut self) -> Result<AppleReading, String> {
            // get_metrics_now is non-blocking; get_metrics(ms) would sleep.
            let m = self.0.get_metrics_now().map_err(|e| e.to_string())?;
            Ok(AppleReading {
                freq_mhz: finite(f64::from(m.gpu_freq_mhz)),
                power_w: finite(f64::from(m.gpu_power)),
                temp_c: m.temp.gpu_temp_avg.and_then(|t| finite(f64::from(t))),
            })
        }
    }

    /// Creates macmon's sampler (on the calling thread, per its README) and takes one reading.
    pub fn init() -> (Option<Box<dyn AppleSource>>, AppleStatus) {
        let sampler = match macmon::Sampler::new() {
            Ok(sampler) => sampler,
            Err(e) => return (None, AppleStatus::Unavailable(e.to_string())),
        };
        let mut source = Macmon(sampler);
        match source.read() {
            Ok(reading) => (Some(Box::new(source)), AppleStatus::Ready(reading)),
            Err(e) => (None, AppleStatus::Unavailable(e)),
        }
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
mod apple {
    use super::{AppleSource, AppleStatus};

    /// Why Apple Silicon counters are missing on targets other than Apple Silicon macOS.
    const NOT_APPLE_REASON: &str = "only measured on Apple Silicon macOS";

    /// Other targets have no Apple Silicon counters.
    pub fn init() -> (Option<Box<dyn AppleSource>>, AppleStatus) {
        (None, AppleStatus::Unavailable(NOT_APPLE_REASON.to_string()))
    }
}

fn available() -> Availability {
    Availability {
        status: AvailabilityStatus::Available,
        reason: None,
    }
}

fn unavailable(reason: impl Into<String>) -> Availability {
    Availability {
        status: AvailabilityStatus::Unavailable,
        reason: Some(reason.into()),
    }
}

/// Available when every GPU reports the field, partial when some do, else unavailable.
fn coverage(total: usize, have: usize, none_reason: &str) -> Availability {
    if total > 0 && have == total {
        available()
    } else if have > 0 {
        Availability {
            status: AvailabilityStatus::Partial,
            reason: Some(format!("{have} of {total} GPUs report this")),
        }
    } else {
        unavailable(none_reason)
    }
}

/// Derives `hello.capabilities.metrics` from what the first readings showed.
pub fn capabilities(
    readings: &[GpuReading],
    gpus_error: Option<&str>,
    apple: &AppleStatus,
) -> MetricCapabilities {
    let total = readings.len();
    let (gpu_usage, gpu_memory) = if let Some(error) = gpus_error {
        (unavailable(error), unavailable(error))
    } else if total == 0 {
        (unavailable("no GPU found"), unavailable("no GPU found"))
    } else {
        let usage = readings.iter().filter(|r| r.usage_pct.is_some()).count();
        let memory = readings
            .iter()
            .filter(|r| r.vram_used_bytes.is_some() && r.vram_total_bytes.is_some())
            .count();
        (
            coverage(total, usage, "the GPU does not report usage"),
            coverage(total, memory, "no GPU reports dedicated memory"),
        )
    };

    let (gpu_freq, gpu_power, temperatures) = match apple {
        AppleStatus::Unavailable(reason) => (
            unavailable(reason.clone()),
            unavailable(reason.clone()),
            unavailable(reason.clone()),
        ),
        AppleStatus::Ready(_) if total != 1 => (
            unavailable(ONE_GPU_REASON),
            unavailable(ONE_GPU_REASON),
            unavailable(ONE_GPU_REASON),
        ),
        AppleStatus::Ready(first) => (
            if first.freq_mhz.is_some() {
                available()
            } else {
                unavailable("GPU frequency not readable")
            },
            if first.power_w.is_some() {
                available()
            } else {
                unavailable("GPU power not readable")
            },
            if first.temp_c.is_some() {
                available()
            } else {
                unavailable("GPU temperature sensor not readable")
            },
        ),
    };

    MetricCapabilities {
        gpu_usage,
        gpu_memory,
        gpu_power,
        gpu_freq,
        temperatures,
    }
}

/// Builds the GPU list, merging Apple Silicon counters only into a single GPU.
pub fn merge(readings: &[GpuReading], apple: Option<&AppleReading>) -> Vec<GpuSample> {
    let attach = match (readings.len(), apple) {
        (1, Some(reading)) => Some(reading),
        _ => None,
    };
    readings
        .iter()
        .map(|r| GpuSample {
            name: r.name.clone(),
            vendor: r.vendor.clone(),
            usage_pct: r.usage_pct,
            vram_used_bytes: r.vram_used_bytes,
            vram_total_bytes: r.vram_total_bytes,
            freq_mhz: attach.and_then(|a| a.freq_mhz),
            power_w: attach.and_then(|a| a.power_w),
            temp_c: attach.and_then(|a| a.temp_c),
        })
        .collect()
}

/// Reads the optional Apple source (only when it can attach) and merges. An `Err` leaves its
/// fields out of this interval and is logged once per distinct error.
pub fn assemble(
    readings: &[GpuReading],
    apple: Option<&mut (dyn AppleSource + '_)>,
    log: &mut ErrorLog,
) -> Vec<GpuSample> {
    let reading = match apple {
        Some(source) if readings.len() == 1 => match source.read() {
            Ok(reading) => Some(reading),
            Err(error) => {
                log.report("macmon", &error);
                None
            }
        },
        _ => None,
    };
    merge(readings, reading.as_ref())
}

fn read_sysinfo(gpus: &Gpus) -> Vec<GpuReading> {
    gpus.iter()
        .enumerate()
        .map(|(index, gpu)| GpuReading {
            name: gpu
                .model()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("GPU {index}")),
            vendor: gpu.vendor().map(str::to_owned),
            usage_pct: gpu.usage().map(pct),
            vram_used_bytes: gpu.used_memory(),
            vram_total_bytes: gpu.total_memory(),
        })
        .collect()
}

/// The GPU side of the sampler: sysinfo `Gpus` plus the optional Apple Silicon source.
pub struct GpuSource {
    gpus: Option<Gpus>,
    apple: Option<Box<dyn AppleSource>>,
}

impl GpuSource {
    /// Builds both sources (not fatal when either fails) and derives the capabilities.
    pub fn init() -> (Self, MetricCapabilities) {
        let (gpus, gpus_error) = match Gpus::new_with_refreshed_list() {
            Ok(gpus) => (Some(gpus), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let readings = gpus.as_ref().map(read_sysinfo).unwrap_or_default();
        let (apple, status) = apple::init();
        let caps = capabilities(&readings, gpus_error.as_deref(), &status);
        (Self { gpus, apple }, caps)
    }

    /// Refreshes and reads every GPU for one interval.
    pub fn sample(&mut self, log: &mut ErrorLog) -> Vec<GpuSample> {
        let Some(gpus) = self.gpus.as_mut() else {
            return Vec::new();
        };
        gpus.refresh(false);
        let readings = read_sysinfo(gpus);
        assemble(&readings, self.apple.as_deref_mut(), log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reading(usage: Option<f64>, vram: Option<u64>) -> GpuReading {
        GpuReading {
            name: "GPU 0".to_string(),
            vendor: Some("Apple".to_string()),
            usage_pct: usage,
            vram_used_bytes: vram,
            vram_total_bytes: vram,
        }
    }

    fn apple_ok() -> AppleReading {
        AppleReading {
            freq_mhz: Some(388.0),
            power_w: Some(0.04),
            temp_c: Some(46.0),
        }
    }

    struct Fake(Vec<Result<AppleReading, String>>);

    impl AppleSource for Fake {
        fn read(&mut self) -> Result<AppleReading, String> {
            self.0.remove(0)
        }
    }

    #[test]
    fn one_gpu_merges_apple_counters() {
        let out = merge(&[reading(Some(3.0), None)], Some(&apple_ok()));
        assert_eq!(out[0].usage_pct, Some(3.0));
        assert_eq!(out[0].freq_mhz, Some(388.0));
        assert_eq!(out[0].power_w, Some(0.04));
        assert_eq!(out[0].temp_c, Some(46.0));
    }

    #[test]
    fn two_gpus_do_not_merge() {
        let out = merge(
            &[reading(Some(1.0), None), reading(Some(2.0), None)],
            Some(&apple_ok()),
        );
        assert_eq!(out.len(), 2);
        assert!(
            out.iter()
                .all(|g| g.freq_mhz.is_none() && g.temp_c.is_none())
        );
    }

    #[test]
    fn missing_apple_reading_leaves_usage_only() {
        let out = merge(&[reading(Some(3.0), None)], None);
        assert_eq!(out[0].usage_pct, Some(3.0));
        assert!(out[0].freq_mhz.is_none() && out[0].power_w.is_none() && out[0].temp_c.is_none());
    }

    #[test]
    fn non_finite_values_are_dropped() {
        assert_eq!(finite(f64::NAN), None);
        assert_eq!(finite(f64::INFINITY), None);
        assert_eq!(finite(1.5), Some(1.5));
    }

    #[test]
    fn capabilities_all_available_on_a_complete_single_gpu() {
        let caps = capabilities(
            &[reading(Some(3.0), Some(10))],
            None,
            &AppleStatus::Ready(apple_ok()),
        );
        for a in [
            &caps.gpu_usage,
            &caps.gpu_memory,
            &caps.gpu_freq,
            &caps.gpu_power,
            &caps.temperatures,
        ] {
            assert_eq!(a.status, AvailabilityStatus::Available);
            assert!(a.reason.is_none());
        }
    }

    #[test]
    fn capabilities_report_partial_and_reasons() {
        let caps = capabilities(
            &[reading(Some(1.0), None), reading(None, None)],
            None,
            &AppleStatus::Ready(apple_ok()),
        );
        assert_eq!(caps.gpu_usage.status, AvailabilityStatus::Partial);
        assert_eq!(caps.gpu_memory.status, AvailabilityStatus::Unavailable);
        assert_eq!(
            caps.gpu_memory.reason.as_deref(),
            Some("no GPU reports dedicated memory")
        );
        assert_eq!(caps.gpu_freq.reason.as_deref(), Some(ONE_GPU_REASON));
    }

    #[test]
    fn capabilities_without_gpus_or_usage() {
        let none = capabilities(&[], None, &AppleStatus::Unavailable("x".into()));
        assert_eq!(none.gpu_usage.reason.as_deref(), Some("no GPU found"));
        assert_eq!(none.gpu_power.reason.as_deref(), Some("x"));

        let silent = capabilities(
            &[reading(None, None)],
            None,
            &AppleStatus::Unavailable("x".into()),
        );
        assert_eq!(
            silent.gpu_usage.reason.as_deref(),
            Some("the GPU does not report usage")
        );
    }

    #[test]
    fn capabilities_carry_the_constructor_error_and_stub_reason() {
        const NOT_APPLE_REASON: &str = "only measured on Apple Silicon macOS";
        let caps = capabilities(
            &[],
            Some("no gpu backend"),
            &AppleStatus::Unavailable(NOT_APPLE_REASON.to_string()),
        );
        assert_eq!(caps.gpu_usage.reason.as_deref(), Some("no gpu backend"));
        assert_eq!(caps.gpu_memory.reason.as_deref(), Some("no gpu backend"));
        assert_eq!(caps.gpu_freq.reason.as_deref(), Some(NOT_APPLE_REASON));
        assert_eq!(caps.temperatures.reason.as_deref(), Some(NOT_APPLE_REASON));
    }

    #[test]
    fn unreadable_temperature_sensor_is_declared() {
        let first = AppleReading {
            temp_c: None,
            ..apple_ok()
        };
        let caps = capabilities(
            &[reading(Some(1.0), None)],
            None,
            &AppleStatus::Ready(first),
        );
        assert_eq!(caps.gpu_freq.status, AvailabilityStatus::Available);
        assert_eq!(
            caps.temperatures.reason.as_deref(),
            Some("GPU temperature sensor not readable")
        );
    }

    #[test]
    fn apple_error_for_one_interval_still_publishes_and_logs_once() {
        let mut fake = Fake(vec![
            Err("channel gone".to_string()),
            Err("channel gone".to_string()),
            Ok(apple_ok()),
        ]);
        let mut log = ErrorLog::default();
        let gpus = [reading(Some(3.0), None)];

        let first = assemble(&gpus, Some(&mut fake), &mut log);
        let second = assemble(&gpus, Some(&mut fake), &mut log);
        let third = assemble(&gpus, Some(&mut fake), &mut log);

        assert_eq!(first[0].usage_pct, Some(3.0));
        assert!(
            first[0].freq_mhz.is_none() && first[0].power_w.is_none() && first[0].temp_c.is_none()
        );
        assert!(second[0].freq_mhz.is_none());
        assert_eq!(third[0].freq_mhz, Some(388.0));
        assert_eq!(log.seen.len(), 1);
        assert!(!log.first("macmon", "channel gone"));
    }

    #[test]
    fn apple_source_is_not_read_when_it_cannot_attach() {
        let mut fake = Fake(Vec::new()); // a read would panic
        let mut log = ErrorLog::default();
        let out = assemble(
            &[reading(Some(1.0), None), reading(Some(2.0), None)],
            Some(&mut fake),
            &mut log,
        );
        assert_eq!(out.len(), 2);
    }
}
