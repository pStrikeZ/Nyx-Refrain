//! WASAPI audio loopback capture source implementing `AudioSource`.
//!
//! Captures system audio or specified endpoint audio in real time using Windows
//! WASAPI loopback mode (`AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK`),
//! delivers interleaved f32 PCM via a lock-free `rtrb` SPSC ring buffer, and handles
//! silence keep-alive and device change notifications with zero stream interruption.

use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient,
    IAudioClient, WAVEFORMATEX,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoTaskMemFree};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

use super::com::{ComMtaGuard, MmcssGuard};
use super::endpoint::{
    DeviceInfo, DeviceSpec, create_device_enumerator, get_device_friendly_name, get_device_id,
    open_render_device,
};
use super::notification::NotificationRegistration;
use super::silence_stream::SilenceKeepAliveStream;
use crate::discontinuity::{
    DiscontinuityStats, DiscontinuityTracker, FLAG_DATA_DISCONTINUITY, FLAG_SILENT,
    FLAG_TIMESTAMP_ERROR,
};
use crate::format::FormatError;
use crate::format::WaveFormatInfo;
use crate::rebuild::RebuildStateMachine;
use crate::time_map::QpcTimestampMapper;
use crate::{AudioChunk, AudioSource};

/// Configuration options for `WasapiLoopbackSource`.
#[derive(Debug, Clone)]
pub struct WasapiLoopbackConfig {
    /// Audio render endpoint specification.
    pub device_spec: DeviceSpec,
    /// Whether to run an active silence keep-alive render stream.
    pub enable_silence_keepalive: bool,
    /// Capacity of the lock-free SPSC ring buffer in audio frames (default: 48,000 frames ≈ 1s).
    pub ring_buffer_capacity_frames: usize,
}

impl Default for WasapiLoopbackConfig {
    fn default() -> Self {
        Self {
            device_spec: DeviceSpec::Default,
            enable_silence_keepalive: true,
            ring_buffer_capacity_frames: 48000,
        }
    }
}

/// Diagnostic counters for `read_chunk` outcomes (process-wide): rebuild-silence chunks,
/// real-data chunks, empty reads, and frames pushed by the capture thread.
pub static DIAG_REBUILD_SILENCE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static DIAG_REAL_CHUNKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static DIAG_EMPTY_READS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static DIAG_CAPTURED_FRAMES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
pub static DIAG_SESSIONS_OPENED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// WASAPI audio loopback capture source implementing `AudioSource`.
pub struct WasapiLoopbackSource {
    config: WasapiLoopbackConfig,
    device_info: DeviceInfo,
    format_info: WaveFormatInfo,
    consumer: rtrb::Consumer<f32>,
    discontinuity_tracker: Arc<DiscontinuityTracker>,
    rebuild_sm: Arc<Mutex<RebuildStateMachine>>,
    drift_mapper: Arc<Mutex<QpcTimestampMapper>>,
    last_read_instant: Instant,
    stop_signal: Arc<AtomicBool>,
    thread_handle: Option<JoinHandle<()>>,
    is_started: bool,
}

impl WasapiLoopbackSource {
    /// Create a new `WasapiLoopbackSource` with default configuration.
    pub fn new() -> Result<Self, Box<dyn Error + Send + Sync>> {
        Self::with_config(WasapiLoopbackConfig::default())
    }

    /// Create a new `WasapiLoopbackSource` with custom configuration.
    pub fn with_config(config: WasapiLoopbackConfig) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let _com = ComMtaGuard::new()?;

        let device = open_render_device(&config.device_spec)?;
        let id = get_device_id(&device).unwrap_or_default();
        let friendly_name = get_device_friendly_name(&device).unwrap_or_else(|_| "Unknown".into());
        let device_info = DeviceInfo {
            id,
            friendly_name,
            is_default: matches!(config.device_spec, DeviceSpec::Default),
        };

        // Query mix format
        let format_info = unsafe {
            let audio_client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let pwfx = audio_client.GetMixFormat()?;
            let fmt = parse_format_from_pwfx(pwfx)?;
            CoTaskMemFree(Some(pwfx as *const _));
            fmt
        };

        info!(
            endpoint = %device_info.friendly_name,
            sample_rate = format_info.sample_rate,
            channels = format_info.channels,
            format = ?format_info.sample_format,
            "Initialized WASAPI loopback capture source"
        );

        let total_samples_capacity =
            config.ring_buffer_capacity_frames * (format_info.channels as usize);
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(total_samples_capacity);

        let discontinuity_tracker = Arc::new(DiscontinuityTracker::new());
        let rebuild_sm = Arc::new(Mutex::new(RebuildStateMachine::new()));
        let drift_mapper = Arc::new(Mutex::new(QpcTimestampMapper::new(
            0,
            Instant::now(),
            format_info.sample_rate,
        )));
        let stop_signal = Arc::new(AtomicBool::new(false));

        // Spawn capture thread
        let thread_stop = stop_signal.clone();
        let thread_device_spec = config.device_spec.clone();
        let thread_enable_keepalive = config.enable_silence_keepalive;
        let thread_discontinuity = discontinuity_tracker.clone();
        let thread_rebuild = rebuild_sm.clone();
        let thread_drift = drift_mapper.clone();

        let thread_handle = std::thread::Builder::new()
            .name("wasapi-loopback-capture".into())
            .spawn(move || {
                run_capture_loop(
                    thread_device_spec,
                    thread_enable_keepalive,
                    producer,
                    thread_discontinuity,
                    thread_rebuild,
                    thread_drift,
                    thread_stop,
                );
            })?;

        Ok(Self {
            config,
            device_info,
            format_info,
            consumer,
            discontinuity_tracker,
            rebuild_sm,
            drift_mapper,
            last_read_instant: Instant::now(),
            stop_signal,
            thread_handle: Some(thread_handle),
            is_started: false,
        })
    }

    /// Return the configuration for this loopback source.
    pub fn config(&self) -> &WasapiLoopbackConfig {
        &self.config
    }

    /// Return metadata describing the current audio capture endpoint.
    pub fn device_info(&self) -> &DeviceInfo {
        &self.device_info
    }

    /// Return format information of the capture stream.
    pub fn format_info(&self) -> &WaveFormatInfo {
        &self.format_info
    }

    /// Get current discontinuity and buffer telemetry statistics.
    pub fn discontinuity_stats(&self) -> DiscontinuityStats {
        self.discontinuity_tracker.stats()
    }

    /// Get current estimated clock drift in ppm.
    pub fn drift_ppm(&self) -> Option<f64> {
        let drift = self.drift_mapper.lock().ok()?;
        let now_instant = Instant::now();
        let anchor_instant = drift.anchor_instant();
        let elapsed_secs = now_instant
            .checked_duration_since(anchor_instant)
            .unwrap_or_default()
            .as_secs_f64();

        if elapsed_secs >= 1.0 {
            // Mapping estimation
            let qpc_units = (elapsed_secs * 10_000_000.0) as u64;
            let current_qpc = drift.anchor_qpc() + qpc_units;
            let frames = (elapsed_secs * drift.sample_rate() as f64) as u64;
            drift.estimate_drift_ppm(frames, current_qpc)
        } else {
            None
        }
    }

    /// Whether the capture stream is currently rebuilding following a device change.
    pub fn is_rebuilding(&self) -> bool {
        self.rebuild_sm
            .lock()
            .map(|sm| sm.is_rebuilding())
            .unwrap_or(false)
    }
}

impl AudioSource for WasapiLoopbackSource {
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
        let chunk_timestamp = self.last_read_instant;

        // If currently rebuilding from a device change, emit digital silence to keep stream continuous
        if let Ok(mut sm) = self.rebuild_sm.lock()
            && sm.is_rebuilding()
        {
            let silence_chunk = sm.generate_silence_chunk(
                max_frames,
                sample_rate,
                self.format_info.channels,
                chunk_timestamp,
            );
            let duration = silence_chunk.duration();
            self.last_read_instant += duration;
            DIAG_REBUILD_SILENCE.fetch_add(1, Ordering::Relaxed);
            return Ok(Some(silence_chunk));
        }

        let max_samples = max_frames * channels;
        let mut samples = Vec::with_capacity(max_samples);

        // Drain available samples from lock-free SPSC consumer
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
            DIAG_EMPTY_READS.fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        }

        // Align to whole audio frames
        let remainder = samples.len() % channels;
        if remainder != 0 {
            samples.truncate(samples.len() - remainder);
        }

        DIAG_REAL_CHUNKS.fetch_add(1, Ordering::Relaxed);
        // Real arrival time of this chunk's first frame on the monotonic clock: now minus
        // everything captured after it (this chunk + samples still queued). A frame-count
        // derived timestamp made the drift estimator always report exactly 0 ppm.
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

impl Drop for WasapiLoopbackSource {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Helper converting a raw Windows `WAVEFORMATEX` pointer to `WaveFormatInfo`.
pub(crate) unsafe fn parse_format_from_pwfx(
    pwfx: *const WAVEFORMATEX,
) -> Result<WaveFormatInfo, FormatError> {
    unsafe {
        if pwfx.is_null() {
            return Err(FormatError::BufferTooShort {
                expected: 18,
                actual: 0,
            });
        }

        let w_format_tag = (*pwfx).wFormatTag;
        let n_channels = (*pwfx).nChannels;
        let n_samples_per_sec = (*pwfx).nSamplesPerSec;
        let w_bits_per_sample = (*pwfx).wBitsPerSample;
        let cb_size = (*pwfx).cbSize;

        let extra_bytes = if cb_size > 0 {
            let extra_ptr = (pwfx as *const u8).add(std::mem::size_of::<WAVEFORMATEX>());
            std::slice::from_raw_parts(extra_ptr, cb_size as usize)
        } else {
            &[]
        };

        WaveFormatInfo::parse(
            w_format_tag,
            n_channels,
            n_samples_per_sec,
            w_bits_per_sample,
            cb_size,
            extra_bytes,
        )
    }
}

/// Real-time capture loop running on dedicated OS thread with MMCSS "Pro Audio" priority.
fn run_capture_loop(
    device_spec: DeviceSpec,
    enable_silence_keepalive: bool,
    mut producer: rtrb::Producer<f32>,
    discontinuity_tracker: Arc<DiscontinuityTracker>,
    rebuild_sm: Arc<Mutex<RebuildStateMachine>>,
    drift_mapper: Arc<Mutex<QpcTimestampMapper>>,
    stop_signal: Arc<AtomicBool>,
) {
    let _com = match ComMtaGuard::new() {
        Ok(g) => g,
        Err(e) => {
            warn!("Failed to initialize COM MTA on capture thread: {e}");
            return;
        }
    };

    let _mmcss = MmcssGuard::enter_pro_audio();

    // Register device change notification callback
    let device_change_flag = Arc::new(AtomicBool::new(false));
    let _notif_guard = create_device_enumerator().ok().and_then(|enumerator| {
        NotificationRegistration::register(enumerator, device_change_flag.clone()).ok()
    });

    let mut sample_buf = Vec::<f32>::with_capacity(4096);

    while !stop_signal.load(Ordering::Relaxed) {
        let mut session = match open_capture_session(&device_spec, enable_silence_keepalive) {
            Ok(s) => s,
            Err(e) => {
                warn!("Failed to open WASAPI capture session: {e}; retrying in 500ms");
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };

        if let Ok(mut sm) = rebuild_sm.lock() {
            sm.mark_active();
        }
        DIAG_SESSIONS_OPENED.fetch_add(1, Ordering::Relaxed);

        // Initialize drift anchor
        if let Ok(mut mapper) = drift_mapper.lock() {
            mapper.update_anchor(0, Instant::now());
        }

        info!("WASAPI loopback capture streaming active");

        // Inner real-time capture packet loop
        while !stop_signal.load(Ordering::Relaxed) {
            // Check for device changes/removal
            if device_change_flag.swap(false, Ordering::SeqCst) {
                info!("Device change detected, rebuilding WASAPI loopback session...");
                if let Ok(mut sm) = rebuild_sm.lock() {
                    sm.trigger_rebuild("endpoint changed");
                }
                break; // Break inner loop to rebuild session
            }

            // Wait on event callback from WASAPI audio engine
            let wait_res = unsafe { WaitForSingleObject(session.event_handle, 50) };
            if wait_res.0 != 0 {
                // Timeout or failure; continue
                continue;
            }

            // Process all pending packets
            while let Ok(packet_size) = unsafe { session.capture_client.GetNextPacketSize() } {
                if packet_size == 0 {
                    break;
                }
                let mut p_data: *mut u8 = std::ptr::null_mut();
                let mut num_frames = 0u32;
                let mut flags = 0u32;
                let mut device_pos = 0u64;
                let mut qpc_pos = 0u64;

                let hr = unsafe {
                    session.capture_client.GetBuffer(
                        &mut p_data,
                        &mut num_frames,
                        &mut flags,
                        Some(&mut device_pos),
                        Some(&mut qpc_pos),
                    )
                };

                if hr.is_err() {
                    break;
                }

                // Discontinuity accounting
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
                DIAG_CAPTURED_FRAMES.fetch_add(num_frames as u64, Ordering::Relaxed);

                // Convert audio samples into interleaved f32
                if !p_data.is_null() && num_frames > 0 {
                    let total_bytes = num_frames as usize * session.format_info.bytes_per_frame;
                    let raw_bytes = unsafe { std::slice::from_raw_parts(p_data, total_bytes) };
                    let is_silent = (mapped_flags & FLAG_SILENT) != 0;

                    if session
                        .format_info
                        .convert_to_interleaved_f32(
                            raw_bytes,
                            num_frames as usize,
                            is_silent,
                            &mut sample_buf,
                        )
                        .is_ok()
                    {
                        // Push into lock-free rtrb buffer (Zero heap alloc, zero locks, zero logs in RT loop!)
                        for &sample in &sample_buf {
                            if producer.push(sample).is_err() {
                                // Ring buffer full; count as dropped frame without blocking
                                discontinuity_tracker.record_packet(FLAG_DATA_DISCONTINUITY, 1);
                            }
                        }
                    }
                }

                // Update drift estimation mapping
                if let Ok(mapper) = drift_mapper.lock() {
                    let _ = mapper.estimate_drift_ppm(device_pos, qpc_pos);
                }

                unsafe {
                    let _ = session.capture_client.ReleaseBuffer(num_frames);
                }
            }
        }

        // Clean up current session before loop iteration / rebuild
        session.teardown();
    }
}

/// Active capture session bundle.
struct ActiveCaptureSession {
    audio_client: IAudioClient,
    capture_client: IAudioCaptureClient,
    event_handle: HANDLE,
    format_info: WaveFormatInfo,
    _silence_keepalive: Option<SilenceKeepAliveStream>,
    pwfx: *mut WAVEFORMATEX,
}

impl ActiveCaptureSession {
    fn teardown(&mut self) {
        unsafe {
            let _ = self.audio_client.Stop();
            let _ = CloseHandle(self.event_handle);
            CoTaskMemFree(Some(self.pwfx as *const _));
        }
    }
}

fn open_capture_session(
    device_spec: &DeviceSpec,
    enable_silence_keepalive: bool,
) -> windows::core::Result<ActiveCaptureSession> {
    unsafe {
        let device = open_render_device(device_spec)?;
        let audio_client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let pwfx = audio_client.GetMixFormat()?;
        let format_info = parse_format_from_pwfx(pwfx).map_err(|e| {
            windows::core::Error::new(
                windows::core::HRESULT(0x80004005u32 as i32),
                format!("Failed to parse mix format: {e}"),
            )
        })?;

        let mut default_period: i64 = 0;
        let mut min_period: i64 = 0;
        audio_client.GetDevicePeriod(Some(&mut default_period), Some(&mut min_period))?;

        // NOTE on IAudioClient3:
        // While Windows 10 introduced IAudioClient3 for sub-10ms shared mode audio streams,
        // Microsoft explicitly does NOT support AUDCLNT_STREAMFLAGS_LOOPBACK on IAudioClient3
        // (IAudioClient3::InitializeSharedAudioStream returns E_INVALIDARG 0x80070057 if LOOPBACK is set).
        // Therefore, loopback capture must use standard IAudioClient::Initialize with shared mode
        // and the minimum device period reported by GetDevicePeriod.
        let buffer_duration = if min_period > 0 {
            min_period
        } else {
            default_period
        };

        audio_client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            buffer_duration,
            0,
            pwfx,
            None,
        )?;

        let event_handle = CreateEventW(None, false, false, None)?;
        audio_client.SetEventHandle(event_handle)?;

        let capture_client: IAudioCaptureClient = audio_client.GetService()?;

        let silence_keepalive = if enable_silence_keepalive {
            let id = get_device_id(&device).unwrap_or_default();
            if !id.is_empty() {
                SilenceKeepAliveStream::start(id).ok()
            } else {
                None
            }
        } else {
            None
        };

        audio_client.Start()?;

        Ok(ActiveCaptureSession {
            audio_client,
            capture_client,
            event_handle,
            format_info,
            _silence_keepalive: silence_keepalive,
            pwfx,
        })
    }
}
