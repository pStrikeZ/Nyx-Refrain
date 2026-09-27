//! COM and MMCSS thread initialization helpers for WASAPI capture.

use tracing::{debug, warn};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
};
use windows::core::{HRESULT, w};

// RPC_E_CHANGED_MODE: HRESULT 0x80010106
const RPC_E_CHANGED_MODE: HRESULT = HRESULT(0x80010106u32 as i32);

/// RAII guard ensuring COM MTA is initialized and uninitialized on the current thread.
pub struct ComMtaGuard {
    needs_uninit: bool,
}

impl ComMtaGuard {
    /// Initialize COM in multithreaded apartment (MTA) mode.
    pub fn new() -> windows::core::Result<Self> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Ok(Self { needs_uninit: true })
        } else if hr == RPC_E_CHANGED_MODE {
            // Already initialized on this thread in STA mode or similar; allow progress
            debug!("COM already initialized on thread with different apartment mode");
            Ok(Self {
                needs_uninit: false,
            })
        } else {
            Err(windows::core::Error::from(hr))
        }
    }
}

impl Drop for ComMtaGuard {
    fn drop(&mut self) {
        if self.needs_uninit {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

/// RAII guard for Multimedia Class Scheduler Service (MMCSS) Pro Audio priority.
pub struct MmcssGuard {
    handle: HANDLE,
}

impl MmcssGuard {
    /// Boost current thread priority to MMCSS "Pro Audio" class.
    pub fn enter_pro_audio() -> Option<Self> {
        let mut task_index = 0u32;
        let handle = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) };
        match handle {
            Ok(h) if !h.is_invalid() => {
                debug!(task_index = task_index, "Entered MMCSS Pro Audio class");
                Some(Self { handle: h })
            }
            Ok(_) => {
                warn!("AvSetMmThreadCharacteristicsW returned invalid handle");
                None
            }
            Err(e) => {
                warn!("Failed to set MMCSS Pro Audio thread characteristics: {e}");
                None
            }
        }
    }
}

impl Drop for MmcssGuard {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            unsafe {
                let _ = AvRevertMmThreadCharacteristics(self.handle);
            }
            debug!("Reverted MMCSS thread characteristics");
        }
    }
}
