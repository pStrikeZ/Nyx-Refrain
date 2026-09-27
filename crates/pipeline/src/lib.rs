//! Audio pipeline, buffering, and clock drift compensation.
//!
//! Handles lock-free SPSC ring buffers, format conversion, runtime resampling,
//! clock drift compensation, 352-frame packet chunking, and silence padding.

pub mod convert;
pub mod drift;
pub mod metrics;
pub mod pipeline;
pub mod quiet;
pub mod resample;
pub mod timer;

pub use convert::{TpdfDither, Xorshift64, downmix_to_stereo};
pub use drift::{DriftCompensator, DriftConfig, MAX_DRIFT_CORRECTION_PPM};
pub use metrics::{PipelineMetricsTracker, PipelineStatsSnapshot, SharedPipelineMetrics};
pub use pipeline::{
    ALAC_FRAME_SAMPLES, ALAC_PCM_SAMPLES, DEFAULT_RING_BUFFER_PACKETS, DEFAULT_UNDERRUN_TIMEOUT_MS,
    PipelineProcessor, start_pipeline_feeder,
};
pub use quiet::{QUIET_MAX_AMPLITUDE_F32, QUIET_MAX_AMPLITUDE_S16, QuietGuard};
pub use resample::{DEFAULT_RESAMPLER_CHUNK_FRAMES, ResamplePipeline, TARGET_SAMPLE_RATE};
pub use timer::{RealtimeThreadGuard, wait_until};
