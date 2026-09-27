//! WASAPI audio render endpoint discovery and selection.

use std::fmt;
use tracing::{debug, info};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::core::{HSTRING, PCWSTR};

/// Specification for selecting an audio render endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DeviceSpec {
    /// The default system audio render endpoint (`eRender`, `eConsole`).
    #[default]
    Default,
    /// Exact endpoint ID string (from `IMMDevice::GetId`).
    ById(String),
    /// Case-insensitive substring match against the device's friendly name.
    ByName(String),
}

impl fmt::Display for DeviceSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => write!(f, "Default"),
            Self::ById(id) => write!(f, "ID({id})"),
            Self::ByName(name) => write!(f, "Name({name})"),
        }
    }
}

/// Metadata describing an audio render endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Unique endpoint identifier string.
    pub id: String,
    /// Friendly human-readable device name (e.g. "Speakers (Realtek Audio)").
    pub friendly_name: String,
    /// Whether this endpoint is currently the system default render device.
    pub is_default: bool,
}

/// Create a new `IMMDeviceEnumerator` COM instance.
pub fn create_device_enumerator() -> windows::core::Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
}

/// Extract the friendly display name of an audio device.
pub fn get_device_friendly_name(device: &IMMDevice) -> windows::core::Result<String> {
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ)?;
        let propvar = store.GetValue(&PKEY_Device_FriendlyName)?;
        let name = propvar.to_string();
        if name.is_empty() {
            Ok("Unknown Audio Device".to_string())
        } else {
            Ok(name)
        }
    }
}

/// Extract the unique endpoint ID string from an `IMMDevice`.
pub fn get_device_id(device: &IMMDevice) -> windows::core::Result<String> {
    unsafe {
        let pwstr = device.GetId()?;
        let id_str = pwstr.to_string().unwrap_or_default();
        CoTaskMemFree(Some(pwstr.as_ptr() as *const _));
        Ok(id_str)
    }
}

/// Retrieve the default system audio render endpoint.
pub fn get_default_render_device() -> windows::core::Result<IMMDevice> {
    let enumerator = create_device_enumerator()?;
    unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
}

/// Enumerate all currently active audio render endpoints.
pub fn list_render_endpoints() -> windows::core::Result<Vec<DeviceInfo>> {
    let enumerator = create_device_enumerator()?;

    let default_id = unsafe {
        enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .and_then(|dev| get_device_id(&dev))
            .unwrap_or_default()
    };

    let collection = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)? };
    let count = unsafe { collection.GetCount()? };

    let mut devices = Vec::with_capacity(count as usize);
    for i in 0..count {
        let device = unsafe { collection.Item(i)? };
        let id = get_device_id(&device).unwrap_or_default();
        let friendly_name = get_device_friendly_name(&device).unwrap_or_else(|_| "Unknown".into());
        let is_default = !default_id.is_empty() && id == default_id;

        devices.push(DeviceInfo {
            id,
            friendly_name,
            is_default,
        });
    }

    Ok(devices)
}

/// Locate and open the requested audio render endpoint matching `spec`.
pub fn open_render_device(spec: &DeviceSpec) -> windows::core::Result<IMMDevice> {
    let enumerator = create_device_enumerator()?;

    match spec {
        DeviceSpec::Default => {
            debug!("Opening default audio render endpoint");
            unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
        }
        DeviceSpec::ById(id) => {
            debug!(id = %id, "Opening audio render endpoint by ID");
            let id_hstring = HSTRING::from(id.as_str());
            unsafe { enumerator.GetDevice(PCWSTR(id_hstring.as_ptr())) }
        }
        DeviceSpec::ByName(name_query) => {
            debug!(query = %name_query, "Locating audio render endpoint by friendly name");
            let collection =
                unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)? };
            let count = unsafe { collection.GetCount()? };

            let query_lower = name_query.to_lowercase();
            let mut available_names = Vec::new();

            for i in 0..count {
                let device = unsafe { collection.Item(i)? };
                let friendly_name =
                    get_device_friendly_name(&device).unwrap_or_else(|_| "Unknown".into());
                if friendly_name.to_lowercase().contains(&query_lower) {
                    info!(
                        query = %name_query,
                        matched = %friendly_name,
                        "Matched audio render endpoint"
                    );
                    return Ok(device);
                }
                available_names.push(friendly_name);
            }

            let msg = format!(
                "No audio render device matched '{}'. Available devices: {:?}",
                name_query, available_names
            );
            Err(windows::core::Error::new(
                windows::core::HRESULT(0x80070490u32 as i32), // ERROR_NOT_FOUND
                msg.as_str(),
            ))
        }
    }
}
