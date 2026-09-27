//! WASAPI QPC timestamp mapping and drift estimation.
//!
//! Converts WASAPI 100-nanosecond QPC (QueryPerformanceCounter) timestamps
//! obtained from `IAudioCaptureClient::GetBuffer` to Rust's monotonic `Instant`
//! and provides drift estimation (ppm) by comparing device audio frame positions
//! against elapsed performance counter time.

use std::time::{Duration, Instant};

/// 100-nanosecond units per second (10,000,000 ticks/sec).
pub const QPC_100NS_PER_SECOND: u64 = 10_000_000;

/// Maps WASAPI 100-nanosecond QPC timestamps to Rust monotonic `Instant`s
/// and estimates clock drift in parts-per-million (ppm).
#[derive(Debug, Clone)]
pub struct QpcTimestampMapper {
    anchor_qpc_100ns: u64,
    anchor_instant: Instant,
    sample_rate: u32,
    initial_device_pos: u64,
    initial_qpc_100ns: u64,
}

impl QpcTimestampMapper {
    /// Create a new timestamp mapper anchored at `(anchor_qpc_100ns, anchor_instant)`.
    pub fn new(anchor_qpc_100ns: u64, anchor_instant: Instant, sample_rate: u32) -> Self {
        Self {
            anchor_qpc_100ns,
            anchor_instant,
            sample_rate,
            initial_device_pos: 0,
            initial_qpc_100ns: anchor_qpc_100ns,
        }
    }

    /// Return the anchor QPC timestamp in 100ns units.
    pub fn anchor_qpc(&self) -> u64 {
        self.anchor_qpc_100ns
    }

    /// Return the anchor `Instant`.
    pub fn anchor_instant(&self) -> Instant {
        self.anchor_instant
    }

    /// Return the sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Update the reference anchor point.
    pub fn update_anchor(&mut self, anchor_qpc_100ns: u64, anchor_instant: Instant) {
        self.anchor_qpc_100ns = anchor_qpc_100ns;
        self.anchor_instant = anchor_instant;
    }

    /// Set or reset the baseline device position and QPC timestamp for drift estimation.
    pub fn set_baseline(&mut self, initial_device_pos: u64, initial_qpc_100ns: u64) {
        self.initial_device_pos = initial_device_pos;
        self.initial_qpc_100ns = initial_qpc_100ns;
    }

    /// Convert a 100-nanosecond QPC timestamp into a monotonic `Instant`.
    pub fn qpc_to_instant(&self, qpc_100ns: u64) -> Instant {
        if qpc_100ns >= self.anchor_qpc_100ns {
            let delta_100ns = qpc_100ns - self.anchor_qpc_100ns;
            let nanos = delta_100ns.saturating_mul(100);
            self.anchor_instant + Duration::from_nanos(nanos)
        } else {
            let delta_100ns = self.anchor_qpc_100ns - qpc_100ns;
            let nanos = delta_100ns.saturating_mul(100);
            self.anchor_instant
                .checked_sub(Duration::from_nanos(nanos))
                .unwrap_or(self.anchor_instant)
        }
    }

    /// Estimate clock drift in parts-per-million (ppm) between the audio device
    /// hardware clock and the system QPC clock.
    ///
    /// Returns `None` if less than 1.0 second of audio has elapsed to prevent
    /// high jitter at startup.
    ///
    /// Positive ppm means the audio device clock is running faster than nominal;
    /// negative ppm means it is running slower than nominal.
    pub fn estimate_drift_ppm(
        &self,
        current_device_pos: u64,
        current_qpc_100ns: u64,
    ) -> Option<f64> {
        if self.sample_rate == 0 {
            return None;
        }

        if current_qpc_100ns <= self.initial_qpc_100ns
            || current_device_pos <= self.initial_device_pos
        {
            return None;
        }

        let delta_qpc_100ns = current_qpc_100ns - self.initial_qpc_100ns;
        let elapsed_qpc_secs = delta_qpc_100ns as f64 / QPC_100NS_PER_SECOND as f64;

        // Need at least 1.0s elapsed for stable drift measurement
        if elapsed_qpc_secs < 1.0 {
            return None;
        }

        let delta_frames = current_device_pos - self.initial_device_pos;
        let elapsed_device_secs = delta_frames as f64 / self.sample_rate as f64;

        let ppm = ((elapsed_device_secs - elapsed_qpc_secs) / elapsed_qpc_secs) * 1_000_000.0;
        Some(ppm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qpc_to_instant_forward() {
        let t0 = Instant::now();
        let qpc0 = 50_000_000u64;
        let mapper = QpcTimestampMapper::new(qpc0, t0, 48000);

        // Exact match
        assert_eq!(mapper.qpc_to_instant(qpc0), t0);

        // 1 second forward (10,000,000 units = 1s = 1_000_000_000 ns)
        let qpc1 = qpc0 + 10_000_000;
        let t1 = mapper.qpc_to_instant(qpc1);
        assert_eq!(t1.duration_since(t0), Duration::from_secs(1));

        // 250 ms forward (2,500,000 units)
        let qpc2 = qpc0 + 2_500_000;
        let t2 = mapper.qpc_to_instant(qpc2);
        assert_eq!(t2.duration_since(t0), Duration::from_millis(250));
    }

    #[test]
    fn test_qpc_to_instant_backward() {
        let t0 = Instant::now();
        let qpc0 = 50_000_000u64;
        let mapper = QpcTimestampMapper::new(qpc0, t0, 48000);

        // 500 ms backward
        let qpc_past = qpc0 - 5_000_000;
        let t_past = mapper.qpc_to_instant(qpc_past);
        assert_eq!(t0.duration_since(t_past), Duration::from_millis(500));
    }

    #[test]
    fn test_drift_estimation() {
        let t0 = Instant::now();
        let qpc0 = 100_000_000u64;
        let sample_rate = 48000;
        let mut mapper = QpcTimestampMapper::new(qpc0, t0, sample_rate);
        mapper.set_baseline(0, qpc0);

        // Less than 1 second: returns None
        let qpc_half_sec = qpc0 + 5_000_000;
        let frames_half_sec = 24000;
        assert!(
            mapper
                .estimate_drift_ppm(frames_half_sec, qpc_half_sec)
                .is_none()
        );

        // Exactly nominal after 10 seconds: 480,000 frames in 100,000,000 units
        let qpc_10s = qpc0 + 10 * QPC_100NS_PER_SECOND;
        let frames_10s = 480_000;
        let ppm = mapper.estimate_drift_ppm(frames_10s, qpc_10s).unwrap();
        assert!(ppm.abs() < 1e-6, "expected ~0 ppm, got {ppm}");

        // Device clock running 50 ppm fast: 480,024 frames in 10 seconds
        let frames_fast = 480_024;
        let ppm_fast = mapper.estimate_drift_ppm(frames_fast, qpc_10s).unwrap();
        assert!(
            (ppm_fast - 50.0).abs() < 0.1,
            "expected ~50 ppm, got {ppm_fast}"
        );

        // Device clock running 50 ppm slow: 479,976 frames in 10 seconds
        let frames_slow = 479_976;
        let ppm_slow = mapper.estimate_drift_ppm(frames_slow, qpc_10s).unwrap();
        assert!(
            (ppm_slow - (-50.0)).abs() < 0.1,
            "expected ~-50 ppm, got {ppm_slow}"
        );
    }
}
