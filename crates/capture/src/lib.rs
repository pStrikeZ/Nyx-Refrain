//! Audio capture abstraction and platform implementations.
//!
//! Provides the `AudioSource` trait for audio input streams (PipeWire virtual sink
//! on Linux, WASAPI loopback on Windows, sine/silence/wav/stdin test sources
//! for cross-platform testing).

use std::time::{Duration, Instant};

pub mod discontinuity;
pub mod format;
#[cfg(target_os = "linux")]
pub mod pipewire;
pub mod rebuild;
pub mod sources;
pub mod time_map;
#[cfg(windows)]
pub mod wasapi;

pub use discontinuity::{DiscontinuityStats, DiscontinuityTracker};
pub use format::{SampleFormat, WaveFormatInfo};
pub use rebuild::{RebuildState, RebuildStateMachine};
pub use sources::{FileWavSource, SilenceSource, SineSource, StdinFormat, StdinSource, WavSource};
pub use time_map::QpcTimestampMapper;

#[cfg(windows)]
pub use wasapi::{
    DeviceInfo, DeviceSpec, ProcessLoopbackConfig, ProcessLoopbackMode, ProcessLoopbackSource,
    WasapiLoopbackConfig, WasapiLoopbackSource,
};

#[cfg(target_os = "linux")]
pub use pipewire::{PipeWireSinkConfig, PipeWireSinkSource, SinkVolume, SinkVolumeControl};

/// Puts the calling thread in the MMCSS "Pro Audio" class on Windows until the returned guard
/// is dropped, so a fully loaded CPU does not starve the audio path. Only the few threads that
/// produce or pace audio should call this; elsewhere it does nothing.
#[must_use]
pub fn enter_audio_thread_priority() -> impl Sized {
    #[cfg(windows)]
    {
        wasapi::MmcssGuard::enter_pro_audio()
    }
}

/// A chunk of audio PCM samples delivered by an `AudioSource`.
///
/// Holds interleaved f32 samples, format metadata, and the monotonic
/// timestamp of the first frame in the chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioChunk {
    /// Interleaved 32-bit floating-point samples (e.g. [L0, R0, L1, R1, ...] for stereo).
    pub data: Vec<f32>,
    /// Sampling rate in Hz (e.g., 44100).
    pub sample_rate: u32,
    /// Number of interleaved audio channels (e.g., 2).
    pub channels: u16,
    /// Monotonic clock instant of the first audio frame in this chunk.
    pub timestamp: Instant,
}

impl AudioChunk {
    /// Create a new audio chunk with the specified data and metadata.
    pub fn new(data: Vec<f32>, sample_rate: u32, channels: u16, timestamp: Instant) -> Self {
        Self {
            data,
            sample_rate,
            channels,
            timestamp,
        }
    }

    /// Return the number of multi-channel audio frames in this chunk.
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.data.len() / self.channels as usize
        }
    }

    /// Calculate the duration of the audio in this chunk.
    pub fn duration(&self) -> Duration {
        if self.sample_rate == 0 || self.channels == 0 {
            Duration::ZERO
        } else {
            let frames = self.frame_count() as u64;
            let secs = frames / self.sample_rate as u64;
            let rem_frames = frames % self.sample_rate as u64;
            let nanos = (rem_frames * 1_000_000_000) / self.sample_rate as u64;
            Duration::new(secs, nanos as u32)
        }
    }
}

/// Common trait for all audio capture sources (loopback, sine, wav, stdin).
pub trait AudioSource: Send {
    /// Sample rate in Hz (e.g., 44100).
    fn sample_rate(&self) -> u32;

    /// Number of audio channels (e.g., 2).
    fn channels(&self) -> u16;

    /// Read or generate the next chunk of audio up to `max_frames` frames.
    ///
    /// Returns:
    /// - `Ok(Some(chunk))` if audio frames are available.
    /// - `Ok(None)` if the source reached the end of stream / EOF.
    /// - `Err(e)` on I/O or capture error.
    fn read_chunk(
        &mut self,
        max_frames: usize,
    ) -> Result<Option<AudioChunk>, Box<dyn std::error::Error + Send + Sync>>;

    /// Start capturing audio (optional lifecycle hook).
    fn start(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    /// Stop capturing audio (optional lifecycle hook).
    /// True for real-time capture sources (WASAPI loopback, PulseAudio monitor…) whose data
    /// arrives at wall-clock pace regardless of consumption. Senders then keep only a few
    /// packets buffered and drop any backlog (e.g. audio captured during the connection
    /// handshake) instead of carrying it as permanent extra latency. File/stdin/test sources
    /// return false and are paced by back-pressure.
    fn is_live(&self) -> bool {
        false
    }

    fn stop(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }
}
