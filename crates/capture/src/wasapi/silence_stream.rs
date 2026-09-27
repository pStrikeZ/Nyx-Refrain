//! Silence keep-alive render stream for WASAPI loopback capture.
//!
//! When system audio is silent (no active applications rendering audio),
//! WASAPI's audio engine enters low-power sleep and halts loopback buffer
//! delivery. Opening a concurrent shared-mode render client writing zeroed/silent
//! buffers keeps the audio engine clock pumping and guarantees continuous
//! loopback chunk production.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use tracing::{debug, warn};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    IAudioClient, IAudioRenderClient,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoTaskMemFree};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use super::com::ComMtaGuard;
use super::endpoint::{DeviceSpec, open_render_device};

/// RAII handle managing an active silence keep-alive render stream.
pub struct SilenceKeepAliveStream {
    stop_flag: Arc<AtomicBool>,
    thread_handle: Option<JoinHandle<()>>,
}

impl SilenceKeepAliveStream {
    /// Start a silence keep-alive stream on the audio endpoint with ID `device_id`.
    pub fn start(device_id: String) -> windows::core::Result<Self> {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let thread_stop = stop_flag.clone();

        let thread_handle = std::thread::Builder::new()
            .name("wasapi-silence-keepalive".into())
            .spawn(move || {
                let _com_guard = match ComMtaGuard::new() {
                    Ok(g) => g,
                    Err(e) => {
                        warn!("Failed to initialize COM for silence keep-alive thread: {e}");
                        return;
                    }
                };

                if let Err(e) = run_silence_loop(device_id, thread_stop) {
                    debug!("Silence keep-alive stream terminated: {e}");
                }
            })
            .map_err(|e| {
                windows::core::Error::new(
                    windows::core::HRESULT(0x80004005u32 as i32), // E_FAIL
                    format!("Failed to spawn thread: {e}"),
                )
            })?;

        Ok(Self {
            stop_flag,
            thread_handle: Some(thread_handle),
        })
    }

    /// Stop the silence keep-alive stream and wait for its worker thread to exit.
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for SilenceKeepAliveStream {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_silence_loop(device_id: String, stop_flag: Arc<AtomicBool>) -> windows::core::Result<()> {
    unsafe {
        let device = open_render_device(&DeviceSpec::ById(device_id))?;
        let audio_client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let pwfx = audio_client.GetMixFormat()?;

        // 50ms buffer duration (500,000 in 100ns units)
        let buffer_duration_100ns: i64 = 500_000;
        let init_res = audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            buffer_duration_100ns,
            0,
            pwfx,
            None,
        );

        if let Err(e) = init_res {
            CoTaskMemFree(Some(pwfx as *const _));
            return Err(e);
        }

        let render_event = CreateEventW(None, false, false, None)?;
        if let Err(e) = audio_client.SetEventHandle(render_event) {
            let _ = CloseHandle(render_event);
            CoTaskMemFree(Some(pwfx as *const _));
            return Err(e);
        }

        let render_client: IAudioRenderClient = audio_client.GetService()?;
        let buffer_size = audio_client.GetBufferSize()?;

        // Prime the render buffer with initial silence
        if let Ok(buf) = render_client.GetBuffer(buffer_size) {
            let _ = render_client.ReleaseBuffer(buffer_size, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32);
            let _ = buf;
        }

        audio_client.Start()?;
        debug!("Silence keep-alive stream started");

        while !stop_flag.load(Ordering::Relaxed) {
            let wait_res = WaitForSingleObject(render_event, 50);
            if wait_res.0 == 0 {
                // Event signaled: render buffer needs feeding
                if let Ok(padding) = audio_client.GetCurrentPadding() {
                    let needed = buffer_size.saturating_sub(padding);
                    if needed > 0
                        && let Ok(_p_buf) = render_client.GetBuffer(needed)
                    {
                        let _ = render_client
                            .ReleaseBuffer(needed, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32);
                    }
                }
            }
        }

        let _ = audio_client.Stop();
        let _ = CloseHandle(render_event);
        CoTaskMemFree(Some(pwfx as *const _));
        debug!("Silence keep-alive stream stopped cleanly");
        Ok(())
    }
}
