//! High-resolution timers and real-time thread scheduling.
//!
//! Windows:
//! - MMCSS ("Pro Audio") registration via `AvSetMmThreadCharacteristicsW`.
//! - `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION` waitable timer via `CreateWaitableTimerExW`.
//!
//! Linux:
//! - `clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME)` high-resolution sleep + spin loop.
//! - Real-time scheduling via `SCHED_FIFO` (priority 10) with fallback to `setpriority(-10)`.
//!
//! Non-Windows / Non-Linux:
//! - Monotonic sleep with high-precision spin-loop fallback.

use std::time::{Duration, Instant};

/// Wait until target monotonic `Instant` with sub-millisecond precision.
pub fn wait_until(target: Instant) {
    let now = Instant::now();
    if target <= now {
        return;
    }

    #[cfg(windows)]
    {
        if wait_until_windows(target) {
            return;
        }
    }

    #[cfg(target_os = "linux")]
    {
        wait_until_linux(target);
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Default / fallback: hybrid sleep + spin-loop
        let remaining = target.duration_since(now);
        if remaining > Duration::from_millis(2) {
            std::thread::sleep(remaining - Duration::from_millis(1));
        }

        while Instant::now() < target {
            std::hint::spin_loop();
        }
    }
}

#[cfg(target_os = "linux")]
#[allow(clippy::unnecessary_cast)]
fn wait_until_linux(target: Instant) {
    let now = Instant::now();
    if target <= now {
        return;
    }

    const SPIN_MARGIN: Duration = Duration::from_micros(200);
    let remaining = target.duration_since(now);
    if remaining > SPIN_MARGIN {
        let sleep_dur = remaining - SPIN_MARGIN;
        let mut now_ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now_ts) } == 0 {
            let add_sec = sleep_dur.as_secs() as i64;
            let add_nsec = sleep_dur.subsec_nanos() as i64;
            let mut sec = now_ts.tv_sec as i64 + add_sec;
            let mut nsec = now_ts.tv_nsec as i64 + add_nsec;
            if nsec >= 1_000_000_000 {
                sec += nsec / 1_000_000_000;
                nsec %= 1_000_000_000;
            }
            let req = libc::timespec {
                tv_sec: sec as _,
                tv_nsec: nsec as _,
            };
            loop {
                let ret = unsafe {
                    libc::clock_nanosleep(
                        libc::CLOCK_MONOTONIC,
                        libc::TIMER_ABSTIME,
                        &req,
                        std::ptr::null_mut(),
                    )
                };
                if ret != libc::EINTR {
                    break;
                }
            }
        }
    }

    while Instant::now() < target {
        std::hint::spin_loop();
    }
}

#[cfg(windows)]
fn wait_until_windows(target: Instant) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, SetWaitableTimer,
        TIMER_ALL_ACCESS, WaitForSingleObject,
    };

    let now = Instant::now();
    if target <= now {
        return true;
    }
    let dur = target.duration_since(now);

    // Negative 100-nanosecond units for relative time in SetWaitableTimer
    let nanos_100 = (dur.as_nanos() / 100) as i64;
    let due_time = -nanos_100;

    unsafe {
        // Try high-resolution waitable timer (Windows 10 1803+)
        let timer = match CreateWaitableTimerExW(
            None,
            None,
            CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
            TIMER_ALL_ACCESS.0,
        ) {
            Ok(h) => h,
            Err(_) => {
                // Fall back to standard timer if high resolution flag is rejected
                match CreateWaitableTimerExW(None, None, 0, TIMER_ALL_ACCESS.0) {
                    Ok(h) => h,
                    Err(_) => return false,
                }
            }
        };

        if timer.is_invalid() {
            return false;
        }

        let res = SetWaitableTimer(timer, &due_time, 0, None, None, false);
        if res.is_ok() {
            let timeout_ms = (dur.as_millis() as u32).saturating_add(50);
            let _ = WaitForSingleObject(timer, timeout_ms);
        }

        let _ = CloseHandle(timer);
        true
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
enum LinuxRealtimeState {
    SchedFifo {
        thread: libc::pthread_t,
        prev_policy: libc::c_int,
        prev_param: libc::sched_param,
    },
    Nice {
        tid: libc::pid_t,
        prev_nice: libc::c_int,
    },
    None,
}

/// Real-time scheduling guard that requests MMCSS "Pro Audio" on Windows
/// or real-time SCHED_FIFO / high priority on Linux.
pub struct RealtimeThreadGuard {
    #[cfg(windows)]
    mmcss_handle: Option<windows::Win32::Foundation::HANDLE>,
    #[cfg(target_os = "linux")]
    linux_state: LinuxRealtimeState,
}

impl Default for RealtimeThreadGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl RealtimeThreadGuard {
    /// Enter real-time scheduling mode for the current thread.
    pub fn new() -> Self {
        #[cfg(windows)]
        {
            let handle = enable_mmcss_pro_audio();
            Self {
                mmcss_handle: handle,
            }
        }
        #[cfg(target_os = "linux")]
        {
            Self {
                linux_state: init_linux_realtime(),
            }
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            Self {}
        }
    }
}

impl Drop for RealtimeThreadGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            if let Some(h) = self.mmcss_handle.take() {
                revert_mmcss(h);
            }
        }
        #[cfg(target_os = "linux")]
        {
            match self.linux_state {
                LinuxRealtimeState::SchedFifo {
                    thread,
                    prev_policy,
                    prev_param,
                } => {
                    let ret =
                        unsafe { libc::pthread_setschedparam(thread, prev_policy, &prev_param) };
                    if ret != 0 {
                        tracing::debug!(
                            errno = ret,
                            "RealtimeThreadGuard: failed to restore previous schedparam"
                        );
                    }
                }
                LinuxRealtimeState::Nice { tid, prev_nice } => {
                    let ret = unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as _, prev_nice) };
                    if ret != 0 {
                        tracing::debug!(
                            err = %std::io::Error::last_os_error(),
                            "RealtimeThreadGuard: failed to restore previous nice priority"
                        );
                    }
                }
                LinuxRealtimeState::None => {}
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn init_linux_realtime() -> LinuxRealtimeState {
    let thread = unsafe { libc::pthread_self() };
    let mut prev_policy: libc::c_int = 0;
    let mut prev_param: libc::sched_param = unsafe { std::mem::zeroed() };
    let got_sched =
        unsafe { libc::pthread_getschedparam(thread, &mut prev_policy, &mut prev_param) } == 0;

    let mut fifo_param: libc::sched_param = unsafe { std::mem::zeroed() };
    fifo_param.sched_priority = 10;
    let ret = unsafe { libc::pthread_setschedparam(thread, libc::SCHED_FIFO, &fifo_param) };
    if ret == 0 {
        tracing::debug!("RealtimeThreadGuard: acquired SCHED_FIFO priority 10");
        if got_sched {
            return LinuxRealtimeState::SchedFifo {
                thread,
                prev_policy,
                prev_param,
            };
        } else {
            return LinuxRealtimeState::None;
        }
    }

    tracing::debug!(
        errno = ret,
        "RealtimeThreadGuard: pthread_setschedparam(SCHED_FIFO, 10) failed; falling back to setpriority"
    );

    let tid = unsafe { libc::gettid() };
    unsafe {
        *libc::__errno_location() = 0;
    }
    let prev_nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, tid as _) };
    let got_nice = prev_nice != -1 || unsafe { *libc::__errno_location() } == 0;

    let set_ret = unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as _, -10) };
    if set_ret == 0 {
        tracing::debug!("RealtimeThreadGuard: acquired nice priority -10");
        if got_nice {
            return LinuxRealtimeState::Nice { tid, prev_nice };
        } else {
            return LinuxRealtimeState::None;
        }
    }

    let err = std::io::Error::last_os_error();
    tracing::debug!(
        %err,
        "RealtimeThreadGuard: setpriority(PRIO_PROCESS, gettid, -10) failed; continuing without RT priority"
    );
    LinuxRealtimeState::None
}

#[cfg(windows)]
fn enable_mmcss_pro_audio() -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
    use windows::core::PCWSTR;

    unsafe {
        // Dynamically load avrt.dll to preserve clean system DLL imports
        let avrt = LoadLibraryA(windows::core::s!("avrt.dll")).ok()?;
        let func_name = windows::core::s!("AvSetMmThreadCharacteristicsW");
        let func = GetProcAddress(avrt, func_name)?;
        type AvSetMmFn = unsafe extern "system" fn(PCWSTR, *mut u32) -> HANDLE;
        let av_set: AvSetMmFn = std::mem::transmute(func);

        let mut task_index = 0u32;
        let task_name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let handle = av_set(PCWSTR::from_raw(task_name.as_ptr()), &mut task_index);
        if handle.is_invalid() {
            None
        } else {
            Some(handle)
        }
    }
}

#[cfg(windows)]
fn revert_mmcss(handle: windows::Win32::Foundation::HANDLE) {
    use windows::Win32::Foundation::{BOOL, HANDLE};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};

    unsafe {
        if let Ok(avrt) = LoadLibraryA(windows::core::s!("avrt.dll")) {
            let func_name = windows::core::s!("AvRevertMmThreadCharacteristics");
            if let Some(func) = GetProcAddress(avrt, func_name) {
                type AvRevFn = unsafe extern "system" fn(HANDLE) -> BOOL;
                let av_rev: AvRevFn = std::mem::transmute(func);
                let _ = av_rev(handle);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wait_until_short_interval() {
        let start = Instant::now();
        let target = start + Duration::from_millis(5);
        wait_until(target);
        let elapsed = Instant::now().duration_since(start);
        assert!(
            elapsed >= Duration::from_millis(4),
            "Elapsed too short: {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_millis(30),
            "Elapsed too long: {:?}",
            elapsed
        );
    }

    #[test]
    fn test_wait_until_accuracy() {
        let start = Instant::now();
        let wait_dur = Duration::from_millis(5);
        let target = start + wait_dur;
        wait_until(target);
        let elapsed = Instant::now().duration_since(start);
        assert!(
            elapsed >= wait_dur,
            "Elapsed should reach target: elapsed={:?}, target={:?}",
            elapsed,
            wait_dur
        );
        assert!(
            elapsed < wait_dur + Duration::from_millis(25),
            "Elapsed too long: {:?}",
            elapsed
        );
    }

    #[test]
    fn test_wait_until_sub_millisecond() {
        let start = Instant::now();
        let wait_dur = Duration::from_micros(600);
        let target = start + wait_dur;
        wait_until(target);
        let elapsed = Instant::now().duration_since(start);
        assert!(
            elapsed >= wait_dur,
            "Elapsed should reach target: elapsed={:?}, target={:?}",
            elapsed,
            wait_dur
        );
        assert!(
            elapsed < wait_dur + Duration::from_millis(10),
            "Elapsed too long: {:?}",
            elapsed
        );
    }

    #[test]
    fn test_wait_until_already_passed() {
        let past = Instant::now() - Duration::from_millis(5);
        wait_until(past);
    }

    #[test]
    fn test_realtime_thread_guard_lifecycle() {
        let guard = RealtimeThreadGuard::new();
        std::thread::yield_now();
        drop(guard);
    }
}
