// SPDX-License-Identifier: AGPL-3.0-or-later
//! Inclusive and active boot-time readings in nanoseconds, bracketed against preemption.
#[cfg(target_os = "macos")]
pub fn readings() -> Option<(u128, u128, u128)> {
    use std::sync::OnceLock;
    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }
    unsafe extern "C" {
        fn mach_continuous_time() -> u64;
        fn mach_absolute_time() -> u64;
        fn mach_timebase_info(info: *mut Timebase) -> i32;
    }
    static SCALE: OnceLock<Option<(u32, u32)>> = OnceLock::new();
    let &(numer, denom) = SCALE
        .get_or_init(|| {
            let mut info = Timebase { numer: 0, denom: 0 };
            // SAFETY: system call writes one initialized timebase structure.
            if unsafe { mach_timebase_info(&mut info) } != 0 || info.numer == 0 || info.denom == 0 {
                None
            } else {
                Some((info.numer, info.denom))
            }
        })
        .as_ref()?;
    // SAFETY: pure system clock reads, available on macOS 10.12 and later.
    let ns = |ticks: u64| u128::from(ticks) * u128::from(numer) / u128::from(denom);
    unsafe {
        Some((
            ns(mach_continuous_time()),
            ns(mach_absolute_time()),
            ns(mach_continuous_time()),
        ))
    }
}
#[cfg(target_os = "linux")]
pub fn readings() -> Option<(u128, u128, u128)> {
    fn clock(id: libc::clockid_t) -> Option<u128> {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: system call writes one initialized timespec.
        if unsafe { libc::clock_gettime(id, &mut time) } != 0
            || time.tv_sec < 0
            || !(0..1_000_000_000).contains(&time.tv_nsec)
        {
            return None;
        }
        Some(time.tv_sec as u128 * 1_000_000_000 + time.tv_nsec as u128)
    }
    Some((
        clock(libc::CLOCK_BOOTTIME)?,
        clock(libc::CLOCK_MONOTONIC)?,
        clock(libc::CLOCK_BOOTTIME)?,
    ))
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn readings() -> Option<(u128, u128, u128)> {
    None
}

#[cfg(windows)]
pub fn readings() -> Option<(u128, u128, u128)> {
    use windows_sys::Win32::System::WindowsProgramming::{
        QueryInterruptTimePrecise, QueryUnbiasedInterruptTimePrecise,
    };
    let (mut before, mut active, mut after) = (0, 0, 0);
    // SAFETY: Windows 10+ initializes these owned 64-bit outputs; 100 ns units.
    unsafe {
        QueryInterruptTimePrecise(&mut before);
        QueryUnbiasedInterruptTimePrecise(&mut active);
        QueryInterruptTimePrecise(&mut after);
    }
    Some((
        u128::from(before) * 100,
        u128::from(active) * 100,
        u128::from(after) * 100,
    ))
}
#[cfg(test)]
mod tests {
    #[test]
    fn clock_units_and_order_are_consistent() {
        let (before, active, after) = super::readings().expect("supported OS clocks");
        assert!(after >= before);
        assert!(after >= active);
        assert!(after > 0);
    }
}
