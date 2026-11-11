//! WASAPI audio render endpoint discovery and selection.

use std::fmt;
use tracing::{debug, info, warn};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, ERole, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    eCommunications, eConsole, eMultimedia, eRender,
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

/// Keeps the default render endpoint (this PC's speakers) muted until dropped, then unmutes
/// it. Holds only the endpoint ID, so it can be dropped on any thread.
pub struct EndpointMuteGuard {
    device_id: String,
}

impl EndpointMuteGuard {
    /// Mutes the default render endpoint. If it is already muted, returns `None` (the user
    /// muted it, leave it alone) unless `adopt` is set: then the guard takes it over and
    /// unmutes it on drop (it was muted by an earlier guard that never got dropped).
    pub fn mute_default(adopt: bool) -> windows::core::Result<Option<Self>> {
        let _com = super::ComMtaGuard::new()?;
        let device = get_default_render_device()?;
        let device_id = get_device_id(&device)?;
        let volume: IAudioEndpointVolume = unsafe { device.Activate(CLSCTX_ALL, None)? };
        if unsafe { volume.GetMute()? }.as_bool() {
            // Not `then_some`: a guard built and dropped here would unmute.
            return Ok(if adopt {
                Some(Self { device_id })
            } else {
                None
            });
        }
        unsafe { volume.SetMute(true, std::ptr::null())? };
        info!(device = %device_id, "Muted the local render endpoint");
        Ok(Some(Self { device_id }))
    }
}

/// `IPolicyConfig`, the undocumented interface Windows' own sound settings use to set the
/// default endpoint (also used by EarTrumpet and SoundSwitch). Only `SetDefaultEndpoint` is
/// called; the methods before it just fill their vtable slots.
#[allow(non_snake_case)]
mod policy_config {
    use windows::Win32::Media::Audio::ERole;
    use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
    use windows::core::{HSTRING, PCWSTR};

    #[windows::core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    unsafe trait IPolicyConfig: windows::core::IUnknown {
        fn GetMixFormat(&self) -> windows::core::HRESULT;
        fn GetDeviceFormat(&self) -> windows::core::HRESULT;
        fn ResetDeviceFormat(&self) -> windows::core::HRESULT;
        fn SetDeviceFormat(&self) -> windows::core::HRESULT;
        fn GetProcessingPeriod(&self) -> windows::core::HRESULT;
        fn SetProcessingPeriod(&self) -> windows::core::HRESULT;
        fn GetShareMode(&self) -> windows::core::HRESULT;
        fn SetShareMode(&self) -> windows::core::HRESULT;
        fn GetPropertyValue(&self) -> windows::core::HRESULT;
        fn SetPropertyValue(&self) -> windows::core::HRESULT;
        fn SetDefaultEndpoint(&self, device_id: PCWSTR, role: ERole) -> windows::core::HRESULT;
    }

    const CLSID_POLICY_CONFIG_CLIENT: windows::core::GUID =
        windows::core::GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

    /// Makes the endpoint `id` the default output for `roles`.
    pub(super) fn set_default_render_device(
        id: &str,
        roles: &[ERole],
    ) -> windows::core::Result<()> {
        let policy: IPolicyConfig =
            unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)? };
        let id = HSTRING::from(id);
        for &role in roles {
            unsafe { policy.SetDefaultEndpoint(PCWSTR(id.as_ptr()), role).ok()? };
        }
        Ok(())
    }
}
use policy_config::set_default_render_device;

/// Whether the render endpoint `id` exists and is plugged in.
fn is_active(id: &str) -> windows::core::Result<bool> {
    let device = open_render_device(&DeviceSpec::ById(id.to_string()))?;
    Ok(unsafe { device.GetState()? } == DEVICE_STATE_ACTIVE)
}

/// Whether the render endpoint `id` is the default output.
fn is_default(id: &str) -> windows::core::Result<bool> {
    Ok(get_device_id(&get_default_render_device()?)? == id)
}

/// The roles of the default output: what plays unless an app picks an output itself.
const OUTPUT_ROLES: [ERole; 2] = [eConsole, eMultimedia];
/// The role of the default communications output (calls, voice chat). Windows keeps it apart
/// from the default output, and it is often another endpoint (a headset).
const COMMUNICATIONS_ROLES: [ERole; 1] = [eCommunications];

/// The default communications output's endpoint ID, if there is one and Windows can say.
fn default_communications_id() -> Option<String> {
    let enumerator = create_device_enumerator().ok()?;
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eCommunications) }.ok()?;
    get_device_id(&device).ok()
}

/// Keeps another render endpoint the default output and the default communications output
/// until dropped, then switches each back to the one it replaced. The two are remembered and
/// switched back separately: either may already have been ours, or be changed by the user
/// meanwhile. Holds only endpoint IDs, so it can be dropped on any thread.
pub struct DefaultOutputGuard {
    target_id: String,
    /// The default output we replaced; `target_id` itself if it already was the default
    /// (only the communications output was switched).
    previous_id: String,
    /// The default communications output we replaced; `None` if we left it alone (it already
    /// was `target_id`, or Windows couldn't say what it was).
    previous_communications_id: Option<String>,
    /// Cleared by [`Self::release`].
    restore: bool,
}

impl DefaultOutputGuard {
    /// Makes `target_id` the default output and the default communications output. Returns
    /// `None` if it already is both.
    pub fn switch_to(target_id: &str) -> windows::core::Result<Option<Self>> {
        let _com = super::ComMtaGuard::new()?;
        let previous_id = get_device_id(&get_default_render_device()?)?;
        let previous_communications_id = default_communications_id().filter(|id| id != target_id);
        if previous_id == target_id && previous_communications_id.is_none() {
            return Ok(None);
        }
        if !is_active(target_id)? {
            return Err(windows::core::Error::new(
                windows::core::HRESULT(0x80070490u32 as i32), // ERROR_NOT_FOUND
                "the output is not connected",
            ));
        }
        if previous_id != target_id {
            set_default_render_device(target_id, &OUTPUT_ROLES)?;
            info!(from = %previous_id, to = %target_id, "Switched the default output");
        }
        if let Some(previous) = &previous_communications_id {
            // The default output is what matters; a failure here is not worth undoing it for.
            match set_default_render_device(target_id, &COMMUNICATIONS_ROLES) {
                Ok(()) => info!(
                    from = %previous, to = %target_id,
                    "Switched the default communications output"
                ),
                Err(e) => {
                    warn!(to = %target_id, "Failed to switch the default communications output: {e}")
                }
            }
        }
        Ok(Some(Self {
            target_id: target_id.to_string(),
            previous_id,
            previous_communications_id,
            restore: true,
        }))
    }

    /// Takes over a switch made by an earlier guard that was never dropped (the app exited
    /// without switching back), so that dropping this one switches back.
    pub fn adopt(
        target_id: String,
        previous_id: String,
        previous_communications_id: Option<String>,
    ) -> Self {
        Self {
            target_id,
            previous_id,
            previous_communications_id,
            restore: true,
        }
    }

    /// Whether our output is still the default: the user may have picked another, or it was
    /// unplugged. Assumed so if Windows can't say.
    pub fn is_current(&self) -> bool {
        let _com = super::ComMtaGuard::new();
        is_default(&self.target_id).unwrap_or(true)
    }

    /// Whether our output is still plugged in. Assumed so if Windows can't say.
    pub fn target_connected(&self) -> bool {
        let _com = super::ComMtaGuard::new();
        is_active(&self.target_id).unwrap_or(true)
    }

    /// The default output's endpoint ID, if Windows can say.
    pub fn current_default() -> Option<String> {
        let _com = super::ComMtaGuard::new().ok()?;
        get_device_id(&get_default_render_device().ok()?).ok()
    }

    /// After a guard was dropped while `stopped_on_id` (a headset) was the default, instead of
    /// `target_id`. Once that output is unplugged and Windows (or its app) falls back to
    /// `target_id`, switches back to `previous_id`, the output `target_id` replaced. Returns
    /// whether to stop watching: switched back, or the user moved on (picked another output,
    /// or picked `target_id` by hand while the headset is still connected). The communications
    /// output goes back to `previous_communications_id` with it, if it fell back to `target_id`
    /// too.
    pub fn hand_back(
        target_id: &str,
        previous_id: &str,
        previous_communications_id: Option<&str>,
        stopped_on_id: &str,
    ) -> windows::core::Result<bool> {
        let _com = super::ComMtaGuard::new()?;
        let default = get_device_id(&get_default_render_device()?)?;
        if default == stopped_on_id {
            return Ok(false);
        }
        if default != target_id || is_active(stopped_on_id)? || !is_active(previous_id)? {
            return Ok(true);
        }
        if previous_id != target_id {
            set_default_render_device(previous_id, &OUTPUT_ROLES)?;
            info!(from = %target_id, to = %previous_id, "Switched the default output back");
        }
        if let Some(previous) = previous_communications_id
            && is_active(previous).unwrap_or(false)
        {
            restore_communications(target_id, previous);
        }
        Ok(true)
    }

    /// Drops the guard without switching back.
    pub fn release(mut self) {
        self.restore = false;
    }

    pub fn target_id(&self) -> &str {
        &self.target_id
    }

    pub fn previous_id(&self) -> &str {
        &self.previous_id
    }

    pub fn previous_communications_id(&self) -> Option<&str> {
        self.previous_communications_id.as_deref()
    }
}

/// Switches the default communications output from `target_id` back to `previous_id`, unless
/// it was changed meanwhile: then the user's choice stays.
fn restore_communications(target_id: &str, previous_id: &str) {
    if default_communications_id().as_deref() != Some(target_id) {
        info!("The default communications output was changed meanwhile; not switching back");
        return;
    }
    match set_default_render_device(previous_id, &COMMUNICATIONS_ROLES) {
        Ok(()) => info!(to = %previous_id, "Switched the default communications output back"),
        Err(e) => warn!(
            to = %previous_id,
            "Failed to switch the default communications output back: {e}"
        ),
    }
}

impl Drop for DefaultOutputGuard {
    fn drop(&mut self) {
        if !self.restore {
            return;
        }
        let Ok(_com) = super::ComMtaGuard::new() else {
            warn!(to = %self.previous_id, "Failed to switch the default output back");
            return;
        };
        if let Some(previous) = &self.previous_communications_id {
            restore_communications(&self.target_id, previous);
        }
        // It already was the default output when we came: nothing to switch back.
        if self.previous_id == self.target_id {
            return;
        }
        let result = (|| -> windows::core::Result<bool> {
            // The user picked another output meanwhile: keep their choice.
            if !is_default(&self.target_id)? {
                return Ok(false);
            }
            set_default_render_device(&self.previous_id, &OUTPUT_ROLES)?;
            Ok(true)
        })();
        match result {
            Ok(true) => info!(to = %self.previous_id, "Switched the default output back"),
            Ok(false) => info!("The default output was changed meanwhile; not switching back"),
            Err(e) => {
                warn!(to = %self.previous_id, "Failed to switch the default output back: {e}")
            }
        }
    }
}

impl Drop for EndpointMuteGuard {
    fn drop(&mut self) {
        let result = (|| -> windows::core::Result<()> {
            let _com = super::ComMtaGuard::new()?;
            let device = open_render_device(&DeviceSpec::ById(self.device_id.clone()))?;
            let volume: IAudioEndpointVolume = unsafe { device.Activate(CLSCTX_ALL, None)? };
            unsafe { volume.SetMute(false, std::ptr::null()) }
        })();
        match result {
            Ok(()) => info!(device = %self.device_id, "Unmuted the local render endpoint"),
            Err(e) => {
                warn!(device = %self.device_id, "Failed to unmute the local render endpoint: {e}")
            }
        }
    }
}
