//! Pipeline performance and telemetry metrics.
//!
//! Tracks and reports:
//! - Ring-buffer fill level and capacity.
//! - Estimated clock drift and applied correction (ppm).
//! - Sent and dropped packets.
//! - Capture discontinuities and underruns.
//! - Periodic 5-second JSONL output for `--stats`.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Point-in-time snapshot of audio pipeline telemetry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PipelineStatsSnapshot {
    /// Timestamp in seconds since epoch or session start.
    pub timestamp_epoch_secs: u64,
    /// Number of 352-frame packets currently stored in the ring buffer.
    pub buffer_fill_packets: usize,
    /// Total capacity of the ring buffer in packets.
    pub buffer_capacity_packets: usize,
    /// Fill percentage (0.0% to 100.0%).
    pub buffer_fill_pct: f64,
    /// Estimated device clock drift in ppm.
    pub estimated_drift_ppm: f64,
    /// Applied resampler ratio correction in ppm.
    pub applied_correction_ppm: f64,
    /// Total audio packets transmitted.
    pub packets_sent: u64,
    /// Packets dropped due to buffer overflow or deadline expiration.
    pub packets_dropped: u64,
    /// Capture discontinuities / source underrun events.
    pub capture_discontinuities: u64,
}

impl PipelineStatsSnapshot {
    /// Format as single-line JSON string suitable for `--stats` stream output.
    pub fn to_json_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Thread-safe atomic tracker for live pipeline telemetry.
#[derive(Debug)]
pub struct PipelineMetricsTracker {
    buffer_fill_packets: AtomicU64,
    buffer_capacity_packets: usize,
    estimated_drift_ppm_scaled: AtomicI64, // ppm * 1000 stored as integer
    applied_correction_ppm_scaled: AtomicI64,
    packets_sent: AtomicU64,
    packets_dropped: AtomicU64,
    capture_discontinuities: AtomicU64,
}

impl PipelineMetricsTracker {
    /// Create a new tracker with specified ring buffer capacity.
    pub fn new(buffer_capacity_packets: usize) -> Self {
        Self {
            buffer_fill_packets: AtomicU64::new(0),
            buffer_capacity_packets,
            estimated_drift_ppm_scaled: AtomicI64::new(0),
            applied_correction_ppm_scaled: AtomicI64::new(0),
            packets_sent: AtomicU64::new(0),
            packets_dropped: AtomicU64::new(0),
            capture_discontinuities: AtomicU64::new(0),
        }
    }

    /// Record current buffer fill level.
    #[inline]
    pub fn set_buffer_fill(&self, fill: usize) {
        self.buffer_fill_packets
            .store(fill as u64, Ordering::Relaxed);
    }

    /// Record drift estimate and applied correction in ppm.
    #[inline]
    pub fn set_drift_ppm(&self, estimated_ppm: f64, applied_ppm: f64) {
        self.estimated_drift_ppm_scaled
            .store((estimated_ppm * 1000.0) as i64, Ordering::Relaxed);
        self.applied_correction_ppm_scaled
            .store((applied_ppm * 1000.0) as i64, Ordering::Relaxed);
    }

    /// Record an audio packet sent.
    #[inline]
    pub fn record_packet_sent(&self) {
        self.packets_sent.fetch_add(1, Ordering::Relaxed);
    }

    /// Record packets dropped.
    #[inline]
    pub fn record_packets_dropped(&self, count: u64) {
        self.packets_dropped.fetch_add(count, Ordering::Relaxed);
    }

    /// Record capture discontinuity / underrun.
    #[inline]
    pub fn record_discontinuity(&self) {
        self.capture_discontinuities.fetch_add(1, Ordering::Relaxed);
    }

    /// Capture point-in-time snapshot.
    pub fn snapshot(&self) -> PipelineStatsSnapshot {
        let fill = self.buffer_fill_packets.load(Ordering::Relaxed) as usize;
        let cap = self.buffer_capacity_packets;
        let fill_pct = if cap > 0 {
            (fill as f64 / cap as f64) * 100.0
        } else {
            0.0
        };

        let est_ppm = (self.estimated_drift_ppm_scaled.load(Ordering::Relaxed) as f64) / 1000.0;
        let app_ppm = (self.applied_correction_ppm_scaled.load(Ordering::Relaxed) as f64) / 1000.0;

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        PipelineStatsSnapshot {
            timestamp_epoch_secs: now_secs,
            buffer_fill_packets: fill,
            buffer_capacity_packets: cap,
            buffer_fill_pct: fill_pct,
            estimated_drift_ppm: est_ppm,
            applied_correction_ppm: app_ppm,
            packets_sent: self.packets_sent.load(Ordering::Relaxed),
            packets_dropped: self.packets_dropped.load(Ordering::Relaxed),
            capture_discontinuities: self.capture_discontinuities.load(Ordering::Relaxed),
        }
    }
}

pub type SharedPipelineMetrics = Arc<PipelineMetricsTracker>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipeline_metrics_tracker_and_json_line() {
        let tracker = PipelineMetricsTracker::new(32);
        tracker.set_buffer_fill(16);
        tracker.set_drift_ppm(123.456, -120.0);
        tracker.record_packet_sent();
        tracker.record_discontinuity();
        tracker.record_packets_dropped(2);

        let snap = tracker.snapshot();
        assert_eq!(snap.buffer_fill_packets, 16);
        assert_eq!(snap.buffer_capacity_packets, 32);
        assert!((snap.buffer_fill_pct - 50.0).abs() < 1e-6);
        assert!((snap.estimated_drift_ppm - 123.456).abs() < 1e-3);
        assert!((snap.applied_correction_ppm - (-120.0)).abs() < 1e-3);
        assert_eq!(snap.packets_sent, 1);
        assert_eq!(snap.capture_discontinuities, 1);
        assert_eq!(snap.packets_dropped, 2);

        let json = snap.to_json_line();
        assert!(json.contains("\"buffer_fill_packets\":16"));
        assert!(json.contains("\"estimated_drift_ppm\":123.456"));
    }
}
