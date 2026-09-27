//! WASAPI audio capture backend for Windows.

pub mod com;
pub mod endpoint;
pub mod loopback;
pub mod notification;
pub mod process_loopback;
pub mod silence_stream;

pub use com::{ComMtaGuard, MmcssGuard};
pub use endpoint::{
    DeviceInfo, DeviceSpec, create_device_enumerator, get_default_render_device,
    get_device_friendly_name, get_device_id, list_render_endpoints, open_render_device,
};
pub use loopback::{WasapiLoopbackConfig, WasapiLoopbackSource};
pub use notification::{DeviceNotificationClient, NotificationRegistration};
pub use process_loopback::{ProcessLoopbackConfig, ProcessLoopbackMode, ProcessLoopbackSource};
pub use silence_stream::SilenceKeepAliveStream;

use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// Test and verify WASAPI / COM initialization and linking.
pub fn init_wasapi() -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| format!("CoInitializeEx failed: {e}"))?;
        CoUninitialize();
    }
    Ok(())
}
