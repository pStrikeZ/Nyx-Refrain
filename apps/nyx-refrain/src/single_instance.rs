//! One GUI instance per user session. A second launch signals the running instance to show
//! its window (it may be hidden in the tray) and exits.

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, ReleaseMutex, SetEvent,
    WaitForSingleObject,
};
#[cfg(windows)]
use windows::core::w;

#[cfg(windows)]
/// Holds the instance mutex; dropping it releases the slot.
pub struct Guard {
    mutex: HANDLE,
}

#[cfg(windows)]
impl Drop for Guard {
    fn drop(&mut self) {
        // Safety: `mutex` was created by `acquire` and is owned by this guard.
        unsafe {
            let _ = ReleaseMutex(self.mutex);
            let _ = CloseHandle(self.mutex);
        }
    }
}

#[cfg(windows)]
/// Returns `None` (after asking the running instance to show itself, if `show_existing` is true)
/// if another instance already runs.
pub fn acquire(show_existing: bool) -> Option<Guard> {
    // Safety: plain Win32 calls with static wide-string names.
    unsafe {
        let mutex = CreateMutexW(None, true, w!("Local\\NyxRefrain.Instance")).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if show_existing
                && let Ok(ev) = OpenEventW(EVENT_MODIFY_STATE, false, w!("Local\\NyxRefrain.Show"))
            {
                let _ = SetEvent(ev);
            }
            return None;
        }
        Some(Guard { mutex })
    }
}

#[cfg(windows)]
/// Waits for "show" requests from later launches and invokes the callback.
pub fn listen_for_show<F>(on_show: F)
where
    F: Fn() + Send + Sync + 'static,
{
    // Safety: auto-reset event owned by this process for its whole lifetime.
    let Ok(ev) = (unsafe { CreateEventW(None, false, false, w!("Local\\NyxRefrain.Show")) }) else {
        return;
    };
    let raw = ev.0 as isize;
    let _ = std::thread::Builder::new()
        .name("nyx-single-instance".into())
        .spawn(move || {
            loop {
                let ev = HANDLE(raw as *mut core::ffi::c_void);
                // Safety: `ev` stays valid; the thread lives until process exit.
                unsafe { WaitForSingleObject(ev, INFINITE) };
                on_show();
            }
        });
}

#[cfg(target_os = "linux")]
/// Holds open lock file with advisory lock (flock).
pub struct Guard {
    _file: std::fs::File,
}

#[cfg(target_os = "linux")]
/// Acquire exclusive non-blocking advisory lock on `$XDG_RUNTIME_DIR/nyx-refrain.lock`
/// (fallback `/tmp/nyx-refrain-$UID.lock`). Returns `None` if already locked.
pub fn acquire() -> Option<Guard> {
    let path =
        if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|s| !s.is_empty()) {
            std::path::PathBuf::from(runtime_dir).join("nyx-refrain.lock")
        } else {
            // Safety: getuid is always valid on POSIX systems.
            let uid = unsafe { libc::getuid() };
            std::path::PathBuf::from(format!("/tmp/nyx-refrain-{uid}.lock"))
        };

    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .ok()?;

    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // Safety: plain flock syscall on a valid open file descriptor.
    let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        return None;
    }

    Some(Guard { _file: file })
}
