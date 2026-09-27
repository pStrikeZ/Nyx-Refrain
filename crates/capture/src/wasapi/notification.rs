//! IMMNotificationClient implementation for audio device change detection.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::info;
use windows::Win32::Media::Audio::{
    DEVICE_STATE, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, eRender,
};
use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;
use windows::core::PCWSTR;

/// Implementation of Windows `IMMNotificationClient` COM interface.
#[windows::core::implement(IMMNotificationClient)]
pub struct DeviceNotificationClient {
    change_flag: Arc<AtomicBool>,
}

impl DeviceNotificationClient {
    pub fn new(change_flag: Arc<AtomicBool>) -> Self {
        Self { change_flag }
    }
}

impl IMMNotificationClient_Impl for DeviceNotificationClient_Impl {
    fn OnDeviceStateChanged(
        &self,
        _pwstrdeviceid: &PCWSTR,
        _dwnewstate: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        info!("Audio endpoint state change notification received");
        self.change_flag.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn OnDeviceAdded(&self, _pwstrdeviceid: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _pwstrdeviceid: &PCWSTR) -> windows::core::Result<()> {
        info!("Audio endpoint removed notification received");
        self.change_flag.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        _role: ERole,
        _pwstrdefaultdeviceid: &PCWSTR,
    ) -> windows::core::Result<()> {
        if flow == eRender {
            info!("Default audio render endpoint changed notification received");
            self.change_flag.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _pwstrdeviceid: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

/// RAII registration guard for endpoint notifications.
pub struct NotificationRegistration {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl NotificationRegistration {
    /// Register notification callback with `enumerator`.
    pub fn register(
        enumerator: IMMDeviceEnumerator,
        change_flag: Arc<AtomicBool>,
    ) -> windows::core::Result<Self> {
        let client: IMMNotificationClient = DeviceNotificationClient::new(change_flag).into();
        unsafe {
            enumerator.RegisterEndpointNotificationCallback(&client)?;
        }
        Ok(Self { enumerator, client })
    }
}

impl Drop for NotificationRegistration {
    fn drop(&mut self) {
        unsafe {
            let _ = self
                .enumerator
                .UnregisterEndpointNotificationCallback(&self.client);
        }
    }
}
