//! Discontinuity and buffer flag tracking for audio capture streams.
//!
//! Tracks WASAPI buffer flags such as `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`,
//! `AUDCLNT_BUFFERFLAGS_SILENT`, and `AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR`,
//! logging warnings on discontinuity and maintaining atomic telemetry counters.

use std::sync::atomic::{AtomicU64, Ordering};
use tracing::warn;

pub const FLAG_DATA_DISCONTINUITY: u32 = 0x1;
pub const FLAG_SILENT: u32 = 0x2;
pub const FLAG_TIMESTAMP_ERROR: u32 = 0x4;

/// Snapshot of discontinuity and buffer statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiscontinuityStats {
    /// Total number of packets with `AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY`.
    pub discontinuity_count: u64,
    /// Total number of packets with `AUDCLNT_BUFFERFLAGS_SILENT`.
    pub silent_packet_count: u64,
    /// Total number of packets with `AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR`.
    pub timestamp_error_count: u64,
    /// Total number of frames processed.
    pub total_frames: u64,
    /// Total number of packets processed.
    pub total_packets: u64,
}

/// Atomic tracker for audio stream discontinuities and flags.
#[derive(Debug, Default)]
pub struct DiscontinuityTracker {
    discontinuity_count: AtomicU64,
    silent_packet_count: AtomicU64,
    timestamp_error_count: AtomicU64,
    total_frames: AtomicU64,
    total_packets: AtomicU64,
}

impl DiscontinuityTracker {
    /// Create a new discontinuity tracker with zeroed counters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a packet's buffer flags and frame count.
    ///
    /// If `FLAG_DATA_DISCONTINUITY` is present, logs a warning and increments
    /// the discontinuity counter.
    pub fn record_packet(&self, flags: u32, frame_count: usize) {
        self.total_packets.fetch_add(1, Ordering::Relaxed);
        self.total_frames
            .fetch_add(frame_count as u64, Ordering::Relaxed);

        if (flags & FLAG_DATA_DISCONTINUITY) != 0 {
            let count = self.discontinuity_count.fetch_add(1, Ordering::Relaxed) + 1;
            warn!(
                discontinuity_count = count,
                frame_count = frame_count,
                "WASAPI audio capture data discontinuity detected"
            );
        }

        if (flags & FLAG_SILENT) != 0 {
            self.silent_packet_count.fetch_add(1, Ordering::Relaxed);
        }

        if (flags & FLAG_TIMESTAMP_ERROR) != 0 {
            self.timestamp_error_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Return the current discontinuity count.
    pub fn discontinuity_count(&self) -> u64 {
        self.discontinuity_count.load(Ordering::Relaxed)
    }

    /// Return the current silent packet count.
    pub fn silent_packet_count(&self) -> u64 {
        self.silent_packet_count.load(Ordering::Relaxed)
    }

    /// Return the total number of frames captured.
    pub fn total_frames(&self) -> u64 {
        self.total_frames.load(Ordering::Relaxed)
    }

    /// Return the total number of packets captured.
    pub fn total_packets(&self) -> u64 {
        self.total_packets.load(Ordering::Relaxed)
    }

    /// Capture a snapshot of all statistics.
    pub fn stats(&self) -> DiscontinuityStats {
        DiscontinuityStats {
            discontinuity_count: self.discontinuity_count.load(Ordering::Relaxed),
            silent_packet_count: self.silent_packet_count.load(Ordering::Relaxed),
            timestamp_error_count: self.timestamp_error_count.load(Ordering::Relaxed),
            total_frames: self.total_frames.load(Ordering::Relaxed),
            total_packets: self.total_packets.load(Ordering::Relaxed),
        }
    }

    /// Reset all counters to zero.
    pub fn reset(&self) {
        self.discontinuity_count.store(0, Ordering::Relaxed);
        self.silent_packet_count.store(0, Ordering::Relaxed);
        self.timestamp_error_count.store(0, Ordering::Relaxed);
        self.total_frames.store(0, Ordering::Relaxed);
        self.total_packets.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discontinuity_tracker_normal() {
        let tracker = DiscontinuityTracker::new();
        tracker.record_packet(0, 480);
        tracker.record_packet(0, 480);

        let s = tracker.stats();
        assert_eq!(s.discontinuity_count, 0);
        assert_eq!(s.silent_packet_count, 0);
        assert_eq!(s.total_packets, 2);
        assert_eq!(s.total_frames, 960);
    }

    #[test]
    fn test_discontinuity_tracker_flags() {
        let tracker = DiscontinuityTracker::new();
        // Packet 1: Normal
        tracker.record_packet(0, 240);
        // Packet 2: Discontinuity
        tracker.record_packet(FLAG_DATA_DISCONTINUITY, 240);
        // Packet 3: Silent + Discontinuity
        tracker.record_packet(FLAG_DATA_DISCONTINUITY | FLAG_SILENT, 240);
        // Packet 4: Timestamp error
        tracker.record_packet(FLAG_TIMESTAMP_ERROR, 240);

        let s = tracker.stats();
        assert_eq!(s.discontinuity_count, 2);
        assert_eq!(s.silent_packet_count, 1);
        assert_eq!(s.timestamp_error_count, 1);
        assert_eq!(s.total_packets, 4);
        assert_eq!(s.total_frames, 960);

        tracker.reset();
        assert_eq!(tracker.stats(), DiscontinuityStats::default());
    }
}
