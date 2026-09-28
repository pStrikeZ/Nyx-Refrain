//! Linux PipeWire virtual sink audio capture source.
//!
//! Registers a virtual `Audio/Sink` node in PipeWire, configures it as the default output
//! via WirePlumber metadata, and captures audio played by applications directly into the sink.
//!
//! Follows the same contract as WASAPI loopback:
//! - Non-blocking `read_chunk` returns `Ok(None)` when no audio is queued (never fabricates silence
//!   during normal operation).
//! - Accurate `CLOCK_MONOTONIC` to `Instant` timestamp conversion with queued frame compensation.
//! - Recovers from stream disconnects or rate changes using `RebuildStateMachine` and
//!   `DiscontinuityTracker`.

use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use crate::discontinuity::{DiscontinuityStats, DiscontinuityTracker, FLAG_DATA_DISCONTINUITY};
use crate::rebuild::RebuildStateMachine;
use crate::{AudioChunk, AudioSource};
pub use pwnative::{SinkVolume, SinkVolumeControl};

/// Configuration options for `PipeWireSinkSource`.
#[derive(Debug, Clone)]
pub struct PipeWireSinkConfig {
    /// Node name in PipeWire graph. Default: `"nyx_refrain_airplay"`.
    pub node_name: String,
    /// Human-readable node description. Default: `"Nyx Refrain (AirPlay)"`.
    pub node_description: String,
    /// Whether to configure this virtual sink as the default audio output in metadata.
    pub set_default: bool,
    /// Optional initial volume level.
    pub initial_volume: Option<SinkVolume>,
}

impl Default for PipeWireSinkConfig {
    fn default() -> Self {
        Self {
            node_name: "nyx_refrain_airplay".to_string(),
            node_description: "Nyx Refrain (AirPlay)".to_string(),
            set_default: true,
            initial_volume: None,
        }
    }
}

/// Linux PipeWire virtual sink capture source implementing `AudioSource`.
pub struct PipeWireSinkSource {
    config: PipeWireSinkConfig,
    sink: Option<pwnative::VirtualSink>,
    consumer: Option<pwnative::SinkConsumer>,
    volume_control: SinkVolumeControl,
    rebuild_sm: RebuildStateMachine,
    discontinuity_tracker: Arc<DiscontinuityTracker>,
    current_rate: u32,
    last_read_instant: Instant,
    /// `last_read_instant` is the expected start of the next real chunk (set after the
    /// first real chunk; cleared on start()).
    timeline_valid: bool,
    /// Real chunk held back while a timeline gap is bridged with silence first.
    pending: Option<AudioChunk>,
    /// Silent frames still to deliver (at `last_read_instant`) before `pending`.
    gap_frames_left: usize,
    is_started: bool,
}

/// Timeline gaps longer than this are bridged with silence (e.g. cycles skipped while
/// WirePlumber regroups the graph when a player connects). PipeWire's cycle clock is
/// accurate to microseconds, so anything above this is a real gap, not jitter. Even a
/// ~30 ms unbridged gap biases the pipeline's cumulative drift estimate for minutes.
const GAP_FILL_THRESHOLD: Duration = Duration::from_micros(500);
/// Longer gaps are not bridged (the timeline is simply re-based).
const GAP_FILL_MAX: Duration = Duration::from_secs(5);

impl PipeWireSinkSource {
    /// Create a new PipeWire virtual sink capture source.
    pub fn new(config: PipeWireSinkConfig) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let opts = pwnative::SinkOptions {
            node_name: config.node_name.clone(),
            node_description: config.node_description.clone(),
            set_default: config.set_default,
            initial_volume: config.initial_volume,
            volume_control: None,
        };

        let (sink, consumer) = pwnative::VirtualSink::create(opts)?;
        let rate = consumer.sample_rate();
        let volume_control = sink.volume_control();

        Ok(Self {
            config,
            sink: Some(sink),
            consumer: Some(consumer),
            volume_control,
            rebuild_sm: RebuildStateMachine::new(),
            discontinuity_tracker: Arc::new(DiscontinuityTracker::new()),
            current_rate: rate,
            last_read_instant: Instant::now(),
            timeline_valid: false,
            pending: None,
            gap_frames_left: 0,
            is_started: false,
        })
    }

    /// Return the volume control handle for the virtual sink.
    pub fn volume_control(&self) -> SinkVolumeControl {
        self.volume_control.clone()
    }

    /// Return current discontinuity and buffer telemetry statistics.
    pub fn discontinuity_stats(&self) -> DiscontinuityStats {
        self.discontinuity_tracker.stats()
    }

    /// Whether the capture stream is currently rebuilding following a disconnect.
    pub fn is_rebuilding(&self) -> bool {
        self.rebuild_sm.is_rebuilding()
    }

    fn rebuild_or_reconnect(&mut self) {
        self.sink.take();
        self.consumer.take();

        let opts = pwnative::SinkOptions {
            node_name: self.config.node_name.clone(),
            node_description: self.config.node_description.clone(),
            set_default: self.config.set_default,
            initial_volume: Some(self.volume_control.current()),
            volume_control: Some(self.volume_control.clone()),
        };

        match pwnative::VirtualSink::create(opts) {
            Ok((new_sink, new_consumer)) => {
                info!("PipeWire virtual sink reconnected successfully");
                self.current_rate = new_consumer.sample_rate();
                self.sink = Some(new_sink);
                self.consumer = Some(new_consumer);
                self.rebuild_sm.mark_active();
            }
            Err(e) => {
                warn!("PipeWire virtual sink reconnect attempt failed: {}", e);
            }
        }
    }
}

impl AudioSource for PipeWireSinkSource {
    fn sample_rate(&self) -> u32 {
        self.current_rate
    }

    fn channels(&self) -> u16 {
        2
    }

    fn is_live(&self) -> bool {
        true
    }

    fn start(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.is_started = true;
        self.last_read_instant = Instant::now();
        self.timeline_valid = false;
        self.pending = None;
        self.gap_frames_left = 0;
        // The sink keeps running between sessions (the GUI keeps it across reconnects);
        // audio queued while nobody read it is stale.
        if let Some(c) = self.consumer.as_mut() {
            while c.pop_chunk(1 << 16).is_some() {}
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.is_started = false;
        self.sink.take();
        self.consumer.take();
        Ok(())
    }

    fn read_chunk(
        &mut self,
        max_frames: usize,
    ) -> Result<Option<AudioChunk>, Box<dyn Error + Send + Sync>> {
        if self.gap_frames_left > 0 {
            let n = self.gap_frames_left.min(max_frames.max(1));
            self.gap_frames_left -= n;
            let silence = AudioChunk::new(
                vec![0.0; n * 2],
                self.current_rate,
                2,
                self.last_read_instant,
            );
            self.last_read_instant += silence.duration();
            return Ok(Some(silence));
        }
        if let Some(chunk) = self.pending.take() {
            self.last_read_instant = chunk.timestamp + chunk.duration();
            return Ok(Some(chunk));
        }

        // If rebuilding, emit synthetic silence to maintain continuous timeline
        if self.rebuild_sm.is_rebuilding() {
            self.rebuild_or_reconnect();
            if self.rebuild_sm.is_rebuilding() {
                let silence_chunk = self.rebuild_sm.generate_silence_chunk(
                    max_frames,
                    self.current_rate,
                    2,
                    self.last_read_instant,
                );
                self.last_read_instant += silence_chunk.duration();
                return Ok(Some(silence_chunk));
            }
        }

        // Check if consumer is disconnected
        let is_disconnected = self.consumer.as_ref().is_none_or(|c| c.is_disconnected());

        if is_disconnected {
            warn!("PipeWire sink disconnected; triggering rebuild");
            self.rebuild_sm
                .trigger_rebuild("pipewire sink disconnected");
            self.discontinuity_tracker
                .record_packet(FLAG_DATA_DISCONTINUITY, max_frames);
            let silence_chunk = self.rebuild_sm.generate_silence_chunk(
                max_frames,
                self.current_rate,
                2,
                self.last_read_instant,
            );
            self.last_read_instant += silence_chunk.duration();
            return Ok(Some(silence_chunk));
        }

        let consumer = match self.consumer.as_mut() {
            Some(c) => c,
            None => return Ok(None),
        };

        let chunk = match consumer.pop_chunk(max_frames) {
            Some(c) => c,
            None => return Ok(None), // Non-blocking: no samples available
        };

        // Handle sample rate change
        if chunk.rate != self.current_rate {
            info!(
                "PipeWire sink sample rate changed: {} -> {}",
                self.current_rate, chunk.rate
            );
            self.discontinuity_tracker
                .record_packet(FLAG_DATA_DISCONTINUITY, chunk.samples.len() / 2);
            self.current_rate = chunk.rate;
        }

        // `chunk.clock_nsec` is already the CLOCK_MONOTONIC time of the chunk's first frame
        // (the graph cycle time, advanced within the cycle by pwnative), so only map that
        // clock onto `Instant` (also CLOCK_MONOTONIC on Linux); queued frames are already
        // accounted for by that per-frame time.
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
        let now_mono_ns = (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64);
        // Frames later in a cycle are timed after the cycle start and can lie slightly in
        // the future relative to `now`, so the offset is signed.
        let now = Instant::now();
        let chunk_timestamp = if chunk.clock_nsec <= now_mono_ns {
            now.checked_sub(Duration::from_nanos(now_mono_ns - chunk.clock_nsec))
        } else {
            now.checked_add(Duration::from_nanos(chunk.clock_nsec - now_mono_ns))
        }
        .unwrap_or(now);

        let mut samples = chunk.samples;
        let mut chunk_timestamp = chunk_timestamp;
        let expected = self.last_read_instant;
        let had_timeline = self.timeline_valid;

        // Timeline went backwards (overlap): drop the frames that would repeat already
        // delivered time so frames and timestamps stay consistent.
        if had_timeline
            && let Some(overlap) = expected.checked_duration_since(chunk_timestamp)
            && overlap > GAP_FILL_THRESHOLD
            && overlap < GAP_FILL_MAX
        {
            let overlap_frames = ((overlap.as_secs_f64() * chunk.rate as f64).round() as usize)
                .min(samples.len() / 2);
            debug!("PipeWire capture overlap of {overlap:?}; trimming {overlap_frames} frames");
            self.discontinuity_tracker
                .record_packet(FLAG_DATA_DISCONTINUITY, overlap_frames);
            samples.drain(..overlap_frames * 2);
            chunk_timestamp = expected;
            if samples.is_empty() {
                return Ok(None);
            }
        }

        let audio_chunk = AudioChunk::new(samples, chunk.rate, 2, chunk_timestamp);
        self.timeline_valid = true;
        self.last_read_instant = chunk_timestamp + audio_chunk.duration();

        // Keep the timeline continuous: the pipeline's drift estimator measures frames per
        // elapsed time, so a gap with no frames would read as a huge negative drift.
        if had_timeline
            && let Some(gap) = chunk_timestamp.checked_duration_since(expected)
            && gap > GAP_FILL_THRESHOLD
            && gap < GAP_FILL_MAX
        {
            let gap_frames = (gap.as_secs_f64() * chunk.rate as f64).round() as usize;
            debug!("PipeWire capture gap of {gap:?}; bridging with {gap_frames} silent frames");
            self.discontinuity_tracker
                .record_packet(FLAG_DATA_DISCONTINUITY, gap_frames);
            self.pending = Some(audio_chunk);
            self.gap_frames_left = gap_frames;
            self.last_read_instant = expected;
            return self.read_chunk(max_frames);
        }

        Ok(Some(audio_chunk))
    }
}

impl Drop for PipeWireSinkSource {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
