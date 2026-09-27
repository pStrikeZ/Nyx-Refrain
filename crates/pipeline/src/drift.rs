//! Clock drift estimation and compensation.
//!
//! Provides:
//! - Primary estimator: calculates source sample rate ppm offset from (device position,
//!   QPC/monotonic timestamp) pairs supplied by `AudioChunk`, low-pass filtered.
//! - Fallback estimator: Proportional-Integral (PI) controller operating on ring-buffer
//!   fill level to prevent buffer underrun/overrun.
//! - Hard clamping: drift correction strictly clamped to `[-500.0, 500.0]` ppm.

use std::time::{Duration, Instant};

/// Maximum allowable drift correction in parts per million (±500 ppm).
pub const MAX_DRIFT_CORRECTION_PPM: f64 = 500.0;

/// Configuration parameters for drift compensation.
#[derive(Debug, Clone)]
pub struct DriftConfig {
    /// Nominal input sample rate in Hz (e.g. 48000 or 44100).
    pub nominal_sample_rate: u32,
    /// Target buffer fill level (in frames or items).
    pub target_fill_level: usize,
    /// Low-pass filter smoothing factor (0.0 < alpha <= 1.0) for primary estimator.
    /// Alpha = 0.05 corresponds to ~20 chunk time constant.
    pub filter_alpha: f64,
    /// Proportional gain Kp for fallback PI controller.
    pub kp: f64,
    /// Integral gain Ki for fallback PI controller.
    pub ki: f64,
}

impl Default for DriftConfig {
    fn default() -> Self {
        Self {
            nominal_sample_rate: 48000,
            target_fill_level: 16, // 16 packets of 352 frames ≈ 5632 frames ≈ 128 ms
            filter_alpha: 0.05,
            kp: 200.0,
            ki: 50.0,
        }
    }
}

/// Estimates and compensates for clock drift between capture device and network timeline.
#[derive(Debug)]
pub struct DriftCompensator {
    config: DriftConfig,
    /// Total frames delivered by source since tracking began.
    total_source_frames: u64,
    /// First timestamp recorded.
    start_time: Option<Instant>,
    /// Last timestamp recorded.
    last_time: Option<Instant>,
    /// Low-pass filtered estimated drift in ppm.
    filtered_drift_ppm: f64,
    /// Fallback PI controller integrator state.
    integral_error: f64,
    /// Last applied correction in ppm.
    applied_correction_ppm: f64,
}

impl DriftCompensator {
    /// Create a new DriftCompensator with given configuration.
    pub fn new(config: DriftConfig) -> Self {
        Self {
            config,
            total_source_frames: 0,
            start_time: None,
            last_time: None,
            filtered_drift_ppm: 0.0,
            integral_error: 0.0,
            applied_correction_ppm: 0.0,
        }
    }

    /// Reset internal state.
    pub fn reset(&mut self) {
        self.total_source_frames = 0;
        self.start_time = None;
        self.last_time = None;
        self.filtered_drift_ppm = 0.0;
        self.integral_error = 0.0;
        self.applied_correction_ppm = 0.0;
    }

    /// Return the current filtered drift estimate in ppm.
    pub fn estimated_drift_ppm(&self) -> f64 {
        self.filtered_drift_ppm
    }

    /// Return the last applied correction in ppm.
    pub fn applied_correction_ppm(&self) -> f64 {
        self.applied_correction_ppm
    }

    /// Update drift estimate using incoming `AudioChunk` timestamp and frame count.
    ///
    /// # Primary Method: Device Position & QPC Timestamp
    /// Compares cumulative audio frames against elapsed monotonic time:
    /// `elapsed_nominal_secs = total_frames / nominal_rate`
    /// `actual_rate = total_frames / elapsed_real_secs`
    /// `drift_ppm = ((actual_rate - nominal_rate) / nominal_rate) * 1e6`
    ///
    /// Low-pass filtered via exponential moving average:
    /// `filtered_ppm = (1 - alpha) * filtered_ppm + alpha * raw_ppm`
    pub fn update_primary(&mut self, chunk_frames: usize, timestamp: Instant) -> f64 {
        let start = match self.start_time {
            Some(t) => t,
            None => {
                self.start_time = Some(timestamp);
                self.last_time = Some(timestamp);
                self.total_source_frames = 0;
                return self.filtered_drift_ppm;
            }
        };

        self.last_time = Some(timestamp);
        self.total_source_frames += chunk_frames as u64;

        if timestamp <= start {
            return self.filtered_drift_ppm;
        }

        let elapsed = timestamp.duration_since(start).as_secs_f64();
        // Wait at least 200 ms for initial settling to avoid small denominator noise
        if elapsed < 0.2 {
            return self.filtered_drift_ppm;
        }

        let nominal_rate = self.config.nominal_sample_rate as f64;
        let measured_rate = (self.total_source_frames as f64) / elapsed;
        let raw_drift_ppm = ((measured_rate - nominal_rate) / nominal_rate) * 1_000_000.0;

        // Apply low-pass filter
        let alpha = self.config.filter_alpha;
        self.filtered_drift_ppm = (1.0 - alpha) * self.filtered_drift_ppm + alpha * raw_drift_ppm;

        self.filtered_drift_ppm
    }

    /// Changes the fill level the fallback PI controller steers towards (e.g. 2 packets for
    /// live sources whose backlog is trimmed at 3 packets by the sender).
    pub fn set_target_fill_level(&mut self, packets: usize) {
        self.config.target_fill_level = packets;
        self.integral_error = 0.0;
    }

    /// Update fallback PI controller based on current ring-buffer fill level.
    ///
    /// Returns the PI controller's suggested drift compensation in ppm.
    pub fn update_fallback_pi(&mut self, current_fill: usize, dt: Duration) -> f64 {
        let dt_secs = dt.as_secs_f64().max(0.001);
        let target = self.config.target_fill_level as f64;
        if target <= 0.0 {
            return 0.0;
        }

        // Positive error means buffer is filling up (source delivering faster than consumer)
        let error = (current_fill as f64 - target) / target;

        // Integral update with anti-windup clamping to [-1.0, 1.0]
        self.integral_error = (self.integral_error + error * dt_secs).clamp(-1.0, 1.0);

        let p_term = self.config.kp * error;
        let i_term = self.config.ki * self.integral_error;

        p_term + i_term
    }

    /// Compute the combined drift correction in ppm, clamped to `[-500.0, 500.0]`.
    ///
    /// If primary estimation is active and has settled, it takes precedence;
    /// otherwise, the fallback PI controller provides the correction.
    pub fn compute_correction(
        &mut self,
        chunk_frames: usize,
        timestamp: Instant,
        current_fill: usize,
        dt: Duration,
    ) -> f64 {
        let primary_ppm = self.update_primary(chunk_frames, timestamp);
        let pi_ppm = self.update_fallback_pi(current_fill, dt);

        // If primary has run for at least 1.0 second, use primary + small PI trimming;
        // otherwise rely primarily on PI.
        let elapsed = self
            .start_time
            .map(|t| timestamp.saturating_duration_since(t).as_secs_f64())
            .unwrap_or(0.0);

        let combined_ppm = if elapsed >= 1.0 {
            // Primary feedforward with small PI feedback to maintain target buffer level
            primary_ppm + 0.2 * pi_ppm
        } else {
            pi_ppm
        };

        // Strictly clamp to [-500.0, 500.0] ppm
        let clamped = combined_ppm.clamp(-MAX_DRIFT_CORRECTION_PPM, MAX_DRIFT_CORRECTION_PPM);
        self.applied_correction_ppm = clamped;
        clamped
    }

    /// Convert drift correction ppm to relative resampler ratio multiplier.
    ///
    /// When source runs faster (+ppm), ratio is decreased (`< 1.0`) so fewer output
    /// samples are generated per input sample.
    /// When source runs slower (-ppm), ratio is increased (`> 1.0`).
    #[inline]
    pub fn ppm_to_relative_ratio(ppm: f64) -> f64 {
        1.0 / (1.0 + ppm * 1e-6)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drift_primary_positive_ppm() {
        let mut comp = DriftCompensator::new(DriftConfig {
            nominal_sample_rate: 48000,
            filter_alpha: 0.2,
            ..Default::default()
        });

        let start = Instant::now();
        // Simulate +200 ppm clock: source produces 48000 * 1.0002 = 48009.6 samples per second
        let nominal_chunk = 480; // 10 ms at 48000
        let real_interval = Duration::from_nanos((10_000_000.0 / 1.0002) as u64);

        let mut t = start;
        for _ in 0..200 {
            t += real_interval;
            comp.update_primary(nominal_chunk, t);
        }

        let est = comp.estimated_drift_ppm();
        // Should converge close to +200 ppm (within 10 ppm)
        assert!(
            (est - 200.0).abs() < 10.0,
            "Expected ~200 ppm, got {:.2} ppm",
            est
        );
    }

    #[test]
    fn test_drift_primary_negative_ppm() {
        let mut comp = DriftCompensator::new(DriftConfig {
            nominal_sample_rate: 48000,
            filter_alpha: 0.2,
            ..Default::default()
        });

        let start = Instant::now();
        // Simulate -200 ppm clock: source produces 48000 * 0.9998 = 47990.4 samples per second
        let nominal_chunk = 480;
        let real_interval = Duration::from_nanos((10_000_000.0 / 0.9998) as u64);

        let mut t = start;
        for _ in 0..200 {
            t += real_interval;
            comp.update_primary(nominal_chunk, t);
        }

        let est = comp.estimated_drift_ppm();
        assert!(
            (est - (-200.0)).abs() < 10.0,
            "Expected ~-200 ppm, got {:.2} ppm",
            est
        );
    }

    #[test]
    fn test_hard_clamp_pm500_ppm() {
        let mut comp = DriftCompensator::new(DriftConfig::default());
        let now = Instant::now();

        // High fill forces large PI correction
        let corr = comp.compute_correction(480, now, 1000, Duration::from_millis(50));
        assert!(
            corr <= MAX_DRIFT_CORRECTION_PPM,
            "Exceeded +500 ppm: {}",
            corr
        );

        // Low fill forces negative PI correction
        let corr_neg = comp.compute_correction(480, now, 0, Duration::from_millis(50));
        assert!(
            corr_neg >= -MAX_DRIFT_CORRECTION_PPM,
            "Exceeded -500 ppm: {}",
            corr_neg
        );
    }

    #[test]
    fn test_ppm_to_relative_ratio() {
        let r_zero = DriftCompensator::ppm_to_relative_ratio(0.0);
        assert!((r_zero - 1.0).abs() < 1e-12);

        let r_pos = DriftCompensator::ppm_to_relative_ratio(500.0);
        assert!(r_pos < 1.0);
        assert!((r_pos - (1.0 / 1.0005)).abs() < 1e-9);

        let r_neg = DriftCompensator::ppm_to_relative_ratio(-500.0);
        assert!(r_neg > 1.0);
        assert!((r_neg - (1.0 / 0.9995)).abs() < 1e-9);
    }
}
