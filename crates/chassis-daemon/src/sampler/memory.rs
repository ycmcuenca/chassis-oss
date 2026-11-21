//! Platform memory readings and conversion to the wire sample.

use chassis_telemetry_protocol::{Availability, AvailabilityStatus, MemorySample, PressureLevel};
use sysinfo::System;

fn totals(total: u64, raw_available: u64) -> (u64, u64) {
    let available = raw_available.min(total);
    (total - available, available)
}

pub fn sample(sys: &System, pressure: Option<PressureLevel>) -> MemorySample {
    #[cfg(target_os = "macos")]
    let raw_available = mac_available().unwrap_or_else(|| sys.available_memory());
    #[cfg(not(target_os = "macos"))]
    let raw_available = sys.available_memory();

    let total_bytes = sys.total_memory();
    let (used_bytes, available_bytes) = totals(total_bytes, raw_available);
    MemorySample {
        used_bytes,
        available_bytes,
        total_bytes,
        swap_used_bytes: sys.used_swap(),
        swap_total_bytes: sys.total_swap(),
        pressure,
    }
}

pub fn capability(reading: Result<Option<PressureLevel>, &'static str>) -> Availability {
    match reading {
        Ok(_) => Availability {
            status: AvailabilityStatus::Available,
            reason: None,
        },
        Err(reason) => Availability {
            status: AvailabilityStatus::Unavailable,
            reason: Some(reason.into()),
        },
    }
}

#[cfg(target_os = "macos")]
#[allow(deprecated)] // libc still provides these Mach bindings; no extra dependency is needed.
fn mac_available() -> Option<u64> {
    // Reuse the send right for the lifetime of the process.
    static HOST: std::sync::OnceLock<libc::mach_port_t> = std::sync::OnceLock::new();
    // SAFETY: mach_host_self has no preconditions.
    let host = *HOST.get_or_init(|| unsafe { libc::mach_host_self() });
    let mut stats = std::mem::MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: HOST_VM_INFO64 takes this aligned output buffer with count in integer units.
    let status = unsafe {
        libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            stats.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if status != libc::KERN_SUCCESS || count != libc::HOST_VM_INFO64_COUNT {
        return None;
    }
    // SAFETY: Mach returned success and the full result size.
    let stats = unsafe { stats.assume_init() };
    // Match vm_stat's “Pages free”, which excludes speculative pages.
    let free = stats.free_count.saturating_sub(stats.speculative_count);
    let pages =
        u64::from(free) + u64::from(stats.inactive_count) + u64::from(stats.purgeable_count);
    // SAFETY: sysconf has no pointer arguments.
    let page_size = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
    Some(pages.saturating_mul(page_size))
}

#[cfg(any(target_os = "macos", test))]
fn mac_pressure(level: i32) -> Option<PressureLevel> {
    match level {
        1 => Some(PressureLevel::Normal),
        2 => Some(PressureLevel::Warning),
        4 => Some(PressureLevel::Critical),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
pub fn pressure() -> Result<Option<PressureLevel>, &'static str> {
    let mut level = 0i32;
    let mut size = size_of::<i32>();
    // SAFETY: the name is NUL terminated, the output buffer is size bytes, and no value is set.
    let status = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_vm_pressure_level".as_ptr(),
            (&mut level as *mut i32).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || size != size_of::<i32>() {
        return Err("cannot read kern.memorystatus_vm_pressure_level");
    }
    Ok(mac_pressure(level))
}

#[cfg(any(target_os = "linux", test))]
fn psi_pressure(input: &str) -> Option<PressureLevel> {
    let avg10 = |kind| {
        input.lines().find_map(|line| {
            let mut words = line.split_ascii_whitespace();
            if words.next()? != kind {
                return None;
            }
            let value: f64 = words
                .find_map(|word| word.strip_prefix("avg10="))?
                .parse()
                .ok()?;
            (0.0..=100.0).contains(&value).then_some(value)
        })
    };
    let (some, full) = (avg10("some")?, avg10("full")?);
    Some(if full >= 5.0 {
        PressureLevel::Critical
    } else if some >= 10.0 {
        PressureLevel::Warning
    } else {
        PressureLevel::Normal
    })
}

#[cfg(target_os = "linux")]
pub fn pressure() -> Result<Option<PressureLevel>, &'static str> {
    let input = std::fs::read_to_string("/proc/pressure/memory")
        .map_err(|_| "cannot read /proc/pressure/memory")?;
    psi_pressure(&input)
        .map(Some)
        .ok_or("invalid /proc/pressure/memory readings")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub fn pressure() -> Result<Option<PressureLevel>, &'static str> {
    Err("memory pressure is not supported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_memory_totals_stay_consistent() {
        for raw_available in [261 * 16_384, 20_000_000] {
            let total = 10_000_000;
            let (used, available) = totals(total, raw_available);
            assert_eq!(available, raw_available.min(total));
            assert_eq!(used, total - available);
            assert!(used + available <= total);
        }
    }

    #[test]
    fn mac_pressure_maps_known_levels_and_omits_unknown() {
        assert_eq!(mac_pressure(1), Some(PressureLevel::Normal));
        assert_eq!(mac_pressure(2), Some(PressureLevel::Warning));
        assert_eq!(mac_pressure(4), Some(PressureLevel::Critical));
        assert_eq!(mac_pressure(3), None);
        assert_eq!(capability(Ok(None)).status, AvailabilityStatus::Available);
        assert_eq!(
            capability(Err("missing")).reason.as_deref(),
            Some("missing")
        );
    }

    #[test]
    fn psi_checks_avg10_and_prioritizes_critical() {
        let read = |some, full| {
            psi_pressure(&format!(
                "some avg10={some} avg60=99.0 total=0\nfull avg10={full} total=0"
            ))
        };
        assert_eq!(read("9.99", "4.99"), Some(PressureLevel::Normal));
        assert_eq!(read("10", "4.99"), Some(PressureLevel::Warning));
        assert_eq!(read("10", "5"), Some(PressureLevel::Critical));
        assert_eq!(read("9", "5"), Some(PressureLevel::Critical));
        assert_eq!(read("NaN", "5"), None);
        assert_eq!(psi_pressure("some avg10=10"), None);
    }
}
