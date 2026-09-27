//! Process-specific WASAPI audio loopback capture source.
//!
//! Uses Windows 10/11 `ActivateAudioInterfaceAsync` with
//! `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` to capture audio from a
//! specific target process tree (or exclude that process tree from capture).

use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVEFORMATEX,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows::core::{HRESULT, Interface};

use super::com::{ComMtaGuard, MmcssGuard};
use super::loopback::parse_format_from_pwfx;
use crate::discontinuity::{
    DiscontinuityStats, DiscontinuityTracker, FLAG_DATA_DISCONTINUITY, FLAG_SILENT,
    FLAG_TIMESTAMP_ERROR,
};
use crate::format::WaveFormatInfo;
use crate::{AudioChunk, AudioSource};

/// Process-loopback audio clients do not implement `GetMixFormat` / `GetDevicePeriod`
/// (`E_NOTIMPL`), so, like Microsoft's ApplicationLoopback sample, we choose the capture
/// format (48 kHz stereo f32) and let the engine convert (`AUTOCONVERTPCM`).
fn capture_format() -> WAVEFORMATEX {
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    let channels = 2u16;
    let rate = 48_000u32;
    let bits = 32u16;
    let block_align = channels * bits / 8;
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
        nChannels: channels,
        nSamplesPerSec: rate,
        nAvgBytesPerSec: rate * u32::from(block_align),
        nBlockAlign: block_align,
        wBitsPerSample: bits,
        cbSize: 0,
    }
}

/// Engine buffer for the capture client, in 100 ns units (20 ms, as in the sample).
const CAPTURE_BUFFER_HNS: i64 = 200_000;

/// Filtering mode for process-specific loopback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProcessLoopbackMode {
    /// Capture audio only from the target process and its child processes.
    #[default]
    IncludeProcessTree,
    /// Capture all system audio EXCEPT from the target process and its child processes.
    ExcludeProcessTree,
}

/// Configuration options for `ProcessLoopbackSource`.
#[derive(Debug, Clone)]
pub struct ProcessLoopbackConfig {
    /// Target Windows Process ID (PID) to include or exclude.
    pub target_process_id: u32,
    /// Whether to include or exclude the target process tree.
    pub mode: ProcessLoopbackMode,
    /// Capacity of the lock-free ring buffer in audio frames (default: 48,000 frames).
    pub ring_buffer_capacity_frames: usize,
}

impl ProcessLoopbackConfig {
    /// Create a configuration targeting a specific PID in include mode.
    pub fn new(target_process_id: u32) -> Self {
        Self {
            target_process_id,
            mode: ProcessLoopbackMode::IncludeProcessTree,
            ring_buffer_capacity_frames: 48000,
        }
    }
}

/// In-memory representation of a `PROPVARIANT` containing a `VT_BLOB`.
#[repr(C)]
struct BlobPropVariant {
    vt: u16,
    w_reserved1: u16,
    w_reserved2: u16,
    w_reserved3: u16,
    cb_size: u32,
    _pad: u32,
    p_blob_data: *mut u8,
}

struct ClientHolder(IAudioClient);
unsafe impl Send for ClientHolder {}

/// Async completion handler for `ActivateAudioInterfaceAsync`.
#[windows::core::implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationHandler {
    event: HANDLE,
    result: Arc<Mutex<Option<windows::core::Result<ClientHolder>>>>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
    fn ActivateCompleted(
        &self,
        activateoperation: Option<&IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        if let Some(op) = activateoperation {
            let mut hr = HRESULT(0);
            let mut punk = None;
            unsafe {
                let res = op.GetActivateResult(&mut hr, &mut punk);
                let client_res = if let Err(e) = res {
                    Err(e)
                } else if hr.is_err() {
                    Err(windows::core::Error::from(hr))
                } else if let Some(unk) = punk {
                    unk.cast::<IAudioClient>().map(ClientHolder)
                } else {
                    Err(windows::core::Error::new(
                        HRESULT(0x80004005u32 as i32),
                        "Activated interface is null",
                    ))
                };
                if let Ok(mut lock) = self.result.lock() {
                    *lock = Some(client_res);
                }
            }
        }
        unsafe {
            let _ = SetEvent(self.event);
        }
        Ok(())
    }
}

/// WASAPI process-specific audio loopback capture source implementing `AudioSource`.
pub struct ProcessLoopbackSource {
    config: ProcessLoopbackConfig,
    format_info: WaveFormatInfo,
    consumer: rtrb::Consumer<f32>,
    discontinuity_tracker: Arc<DiscontinuityTracker>,
    last_read_instant: Instant,
    stop_signal: Arc<AtomicBool>,
    thread_handle: Option<JoinHandle<()>>,
    is_started: bool,
}

impl ProcessLoopbackSource {
    /// Create a new `ProcessLoopbackSource` targeting the specified PID in include mode.
    pub fn new(target_process_id: u32) -> Result<Self, Box<dyn Error + Send + Sync>> {
        Self::with_config(ProcessLoopbackConfig::new(target_process_id))
    }

    /// Create a new `ProcessLoopbackSource` with explicit configuration.
    pub fn with_config(
        config: ProcessLoopbackConfig,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let _com = ComMtaGuard::new()?;

        let audio_client = activate_process_audio_client(config.target_process_id, config.mode)?;
        // Activation above validates the target; the format is ours (see `capture_format`).
        drop(audio_client);
        let wfx = capture_format();
        let format_info = unsafe { parse_format_from_pwfx(&wfx)? };

        info!(
            target_pid = config.target_process_id,
            mode = ?config.mode,
            sample_rate = format_info.sample_rate,
            channels = format_info.channels,
            "Initialized WASAPI process loopback capture source"
        );

        let total_samples = config.ring_buffer_capacity_frames * (format_info.channels as usize);
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(total_samples);

        let discontinuity_tracker = Arc::new(DiscontinuityTracker::new());
        let stop_signal = Arc::new(AtomicBool::new(false));

        let thread_stop = stop_signal.clone();
        let thread_discontinuity = discontinuity_tracker.clone();
        let thread_pid = config.target_process_id;
        let thread_mode = config.mode;

        let thread_handle = std::thread::Builder::new()
            .name("wasapi-process-loopback".into())
            .spawn(move || {
                run_process_capture_loop(
                    thread_pid,
                    thread_mode,
                    producer,
                    thread_discontinuity,
                    thread_stop,
                );
            })?;

        Ok(Self {
            config,
            format_info,
            consumer,
            discontinuity_tracker,
            last_read_instant: Instant::now(),
            stop_signal,
            thread_handle: Some(thread_handle),
            is_started: false,
        })
    }

    /// Return the configuration for this process loopback source.
    pub fn config(&self) -> &ProcessLoopbackConfig {
        &self.config
    }

    /// Return format information of the capture stream.
    pub fn format_info(&self) -> &WaveFormatInfo {
        &self.format_info
    }

    /// Get current discontinuity and buffer telemetry statistics.
    pub fn discontinuity_stats(&self) -> DiscontinuityStats {
        self.discontinuity_tracker.stats()
    }
}

impl AudioSource for ProcessLoopbackSource {
    fn is_live(&self) -> bool {
        true
    }

    fn sample_rate(&self) -> u32 {
        self.format_info.sample_rate
    }

    fn channels(&self) -> u16 {
        self.format_info.channels
    }

    fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.is_started = true;
        self.last_read_instant = Instant::now();
        Ok(())
    }

    fn stop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.is_started = false;
        self.stop_signal.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
        Ok(())
    }

    fn read_chunk(
        &mut self,
        max_frames: usize,
    ) -> Result<Option<AudioChunk>, Box<dyn Error + Send + Sync>> {
        let channels = self.format_info.channels as usize;
        let sample_rate = self.format_info.sample_rate;

        let max_samples = max_frames * channels;
        let mut samples = Vec::with_capacity(max_samples);

        while samples.len() < max_samples {
            match self.consumer.pop() {
                Ok(sample) => samples.push(sample),
                Err(rtrb::PopError::Empty) => break,
            }
        }

        if samples.is_empty() {
            // No captured data right now (WASAPI delivers every ~10 ms). Report "nothing yet"
            // instead of fabricating a silence chunk: fabricated chunks interleaved with real
            // data inflated the stream ~3x (buffer always full / >1 s latency) and, once the
            // sender trimmed backlog, produced crackles. Real gaps are filled by the pipeline's
            // underrun handling.
            return Ok(None);
        }

        let remainder = samples.len() % channels;
        if remainder != 0 {
            samples.truncate(samples.len() - remainder);
        }

        // Real arrival time of this chunk's first frame (see loopback.rs).
        let frames_ahead = (samples.len() + self.consumer.slots()) / channels.max(1);
        let data_timestamp = Instant::now()
            .checked_sub(Duration::from_secs_f64(
                frames_ahead as f64 / sample_rate.max(1) as f64,
            ))
            .unwrap_or_else(Instant::now);
        let chunk = AudioChunk::new(
            samples,
            sample_rate,
            self.format_info.channels,
            data_timestamp,
        );
        let duration = chunk.duration();
        self.last_read_instant += duration;

        Ok(Some(chunk))
    }
}

impl Drop for ProcessLoopbackSource {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn activate_process_audio_client(
    target_pid: u32,
    mode: ProcessLoopbackMode,
) -> windows::core::Result<IAudioClient> {
    let mode_flag = match mode {
        ProcessLoopbackMode::IncludeProcessTree => {
            PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
        }
        ProcessLoopbackMode::ExcludeProcessTree => {
            PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
        }
    };

    let mut process_params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: target_pid,
                ProcessLoopbackMode: mode_flag,
            },
        },
    };

    let completion_event = unsafe { CreateEventW(None, false, false, None)? };
    let result_holder = Arc::new(Mutex::new(None));

    let handler: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
        event: completion_event,
        result: result_holder.clone(),
    }
    .into();

    let propvar = BlobPropVariant {
        vt: 65, // VT_BLOB
        w_reserved1: 0,
        w_reserved2: 0,
        w_reserved3: 0,
        cb_size: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
        _pad: 0,
        p_blob_data: &mut process_params as *mut _ as *mut u8,
    };

    unsafe {
        let _op = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&propvar as *const _ as *const windows::core::PROPVARIANT),
            &handler,
        )?;

        let wait = WaitForSingleObject(completion_event, 5000);
        let _ = CloseHandle(completion_event);

        if wait.0 != 0 {
            return Err(windows::core::Error::new(
                HRESULT(0x80004005u32 as i32),
                "ActivateAudioInterfaceAsync timed out",
            ));
        }
    }

    let mut lock = result_holder.lock().unwrap();
    match lock.take() {
        Some(Ok(holder)) => Ok(holder.0),
        Some(Err(e)) => Err(e),
        None => Err(windows::core::Error::new(
            HRESULT(0x80004005u32 as i32),
            "No activation result recorded",
        )),
    }
}

fn run_process_capture_loop(
    target_pid: u32,
    mode: ProcessLoopbackMode,
    mut producer: rtrb::Producer<f32>,
    discontinuity_tracker: Arc<DiscontinuityTracker>,
    stop_signal: Arc<AtomicBool>,
) {
    let _com = match ComMtaGuard::new() {
        Ok(g) => g,
        Err(e) => {
            warn!("Failed to initialize COM for process loopback thread: {e}");
            return;
        }
    };

    let _mmcss = MmcssGuard::enter_pro_audio();

    let audio_client = match activate_process_audio_client(target_pid, mode) {
        Ok(c) => c,
        Err(e) => {
            warn!("Process loopback activation failed on worker thread: {e}");
            return;
        }
    };

    unsafe {
        let wfx = capture_format();
        let pwfx: *const WAVEFORMATEX = &wfx;

        let format_info = match parse_format_from_pwfx(pwfx) {
            Ok(f) => f,
            Err(e) => {
                warn!("parse_format failed for process loopback: {e}");
                return;
            }
        };

        let init_res = audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK
                | AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            CAPTURE_BUFFER_HNS,
            0,
            pwfx,
            None,
        );

        if let Err(e) = init_res {
            warn!("Initialize failed for process loopback: {e}");
            return;
        }

        let event_handle = match CreateEventW(None, false, false, None) {
            Ok(h) => h,
            Err(e) => {
                warn!("CreateEventW failed: {e}");
                return;
            }
        };

        let _ = audio_client.SetEventHandle(event_handle);
        let capture_client: IAudioCaptureClient = match audio_client.GetService() {
            Ok(c) => c,
            Err(e) => {
                warn!("GetService::<IAudioCaptureClient> failed: {e}");
                let _ = CloseHandle(event_handle);
                return;
            }
        };

        let _ = audio_client.Start();
        debug!("Process loopback capture streaming started");

        let mut sample_buf = Vec::<f32>::with_capacity(4096);

        while !stop_signal.load(Ordering::Relaxed) {
            let wait_res = WaitForSingleObject(event_handle, 50);
            if wait_res.0 != 0 {
                continue;
            }

            while let Ok(packet_size) = capture_client.GetNextPacketSize() {
                if packet_size == 0 {
                    break;
                }
                let mut p_data: *mut u8 = std::ptr::null_mut();
                let mut num_frames = 0u32;
                let mut flags = 0u32;
                let mut device_pos = 0u64;
                let mut qpc_pos = 0u64;

                let hr = capture_client.GetBuffer(
                    &mut p_data,
                    &mut num_frames,
                    &mut flags,
                    Some(&mut device_pos),
                    Some(&mut qpc_pos),
                );

                if hr.is_err() {
                    break;
                }

                let mut mapped_flags = 0u32;
                if (flags & (AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32)) != 0 {
                    mapped_flags |= FLAG_DATA_DISCONTINUITY;
                }
                if (flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)) != 0 {
                    mapped_flags |= FLAG_SILENT;
                }
                if (flags & (AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32)) != 0 {
                    mapped_flags |= FLAG_TIMESTAMP_ERROR;
                }
                discontinuity_tracker.record_packet(mapped_flags, num_frames as usize);

                if !p_data.is_null() && num_frames > 0 {
                    let total_bytes = num_frames as usize * format_info.bytes_per_frame;
                    let raw_bytes = std::slice::from_raw_parts(p_data, total_bytes);
                    let is_silent = (mapped_flags & FLAG_SILENT) != 0;

                    if format_info
                        .convert_to_interleaved_f32(
                            raw_bytes,
                            num_frames as usize,
                            is_silent,
                            &mut sample_buf,
                        )
                        .is_ok()
                    {
                        for &sample in &sample_buf {
                            if producer.push(sample).is_err() {
                                discontinuity_tracker.record_packet(FLAG_DATA_DISCONTINUITY, 1);
                            }
                        }
                    }
                }

                let _ = capture_client.ReleaseBuffer(num_frames);
            }
        }

        let _ = audio_client.Stop();
        let _ = CloseHandle(event_handle);
        debug!("Process loopback capture streaming stopped cleanly");
    }
}
