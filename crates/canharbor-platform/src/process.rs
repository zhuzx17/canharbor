// SPDX-License-Identifier: AGPL-3.0-or-later
//! CPU seconds, current resident bytes, and peak resident bytes are distinct observations.
#[cfg(unix)]
pub fn process_usage() -> Option<(f64, u64)> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the supplied structure on success.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    let usage = unsafe { usage.assume_init() };
    let seconds = usage.ru_utime.tv_sec as f64
        + usage.ru_utime.tv_usec as f64 / 1e6
        + usage.ru_stime.tv_sec as f64
        + usage.ru_stime.tv_usec as f64 / 1e6;
    #[cfg(target_os = "macos")]
    let bytes = usage.ru_maxrss.max(0) as u64;
    #[cfg(not(target_os = "macos"))]
    let bytes = (usage.ru_maxrss.max(0) as u64).saturating_mul(1024);
    Some((seconds, bytes))
}
#[cfg(target_os = "macos")]
#[allow(deprecated)] // Pinned libc owns the complete Mach ABI used for this read-only probe.
pub fn current_rss() -> Option<u64> {
    let mut value = std::mem::MaybeUninit::<libc::mach_task_basic_info>::uninit();
    let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
    // SAFETY: own task port, correctly sized output and count; read only on success.
    if unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            value.as_mut_ptr().cast(),
            &mut count,
        )
    } != libc::KERN_SUCCESS
    {
        return None;
    }
    // SAFETY: task_info initialized the complete basic-info structure.
    Some(unsafe { value.assume_init() }.resident_size)
}
#[cfg(target_os = "linux")]
pub fn current_rss() -> Option<u64> {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page <= 0 {
        return None;
    }
    std::fs::read_to_string("/proc/self/statm")
        .ok()?
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?
        .checked_mul(page as u64)
}
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn current_rss() -> Option<u64> {
    None
}

#[cfg(windows)]
fn memory() -> Option<windows_sys::Win32::System::ProcessStatus::PROCESS_MEMORY_COUNTERS> {
    use windows_sys::Win32::System::{
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::GetCurrentProcess,
    };
    let mut value: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    value.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: own-process pseudo handle; value is correctly sized writable memory.
    if unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut value,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
    } == 0
    {
        return None;
    }
    Some(value)
}
#[cfg(windows)]
pub fn current_rss() -> Option<u64> {
    Some(memory()?.WorkingSetSize as u64)
}
#[cfg(windows)]
pub fn process_usage() -> Option<(f64, u64)> {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentProcess, GetProcessTimes},
    };
    let zero = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: owned FILETIME outputs and the current-process pseudo handle.
    if unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    } == 0
    {
        return None;
    }
    let ticks = |v: FILETIME| (u64::from(v.dwHighDateTime) << 32) | u64::from(v.dwLowDateTime);
    Some((
        (ticks(kernel) as f64 + ticks(user) as f64) / 10_000_000.0,
        memory()?.PeakWorkingSetSize as u64,
    ))
}
#[cfg(test)]
mod tests {
    #[test]
    fn own_process_observations_have_explicit_units() {
        let (seconds, peak_bytes) = super::process_usage().expect("process usage");
        assert!(seconds.is_finite() && seconds >= 0.0);
        assert!(peak_bytes > 0);
        assert!(super::current_rss().expect("resident bytes") > 0);
    }
}
