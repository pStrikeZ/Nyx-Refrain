//! RT-safe quiet-mode guard and atomic violation counter.
//!
//! Enforces the nighttime safety red-line:
//! - Handshake sends volume attenuation (-144 / -30).
//! - Sent PCM must remain <= -60 dBFS or digital silence.
//! - Code-level hard guard: any sample exceeding -60 dBFS is clamped, and
//!   increments an atomic `quiet_violation` counter which must remain 0.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// -60 dBFS threshold amplitude for normalized f32 audio [-1.0, 1.0].
/// 10^(-60 / 20) = 0.001.
pub const QUIET_MAX_AMPLITUDE_F32: f32 = 0.001;

/// -60 dBFS threshold amplitude for 16-bit signed integer audio [-32768, 32767].
/// 32 / 32768 ≈ 0.00097656 (-60.2 dBFS <= -60.0 dBFS).
pub const QUIET_MAX_AMPLITUDE_S16: i16 = 32;

/// Real-time safe quiet guard that clamps loud audio samples and counts violations.
///
/// Designed for use on high-priority audio threads:
/// - Zero heap allocations
/// - Lock-free atomic state
/// - No logging I/O or panics
#[derive(Debug)]
pub struct QuietGuard {
    enabled: AtomicBool,
    violations: AtomicU64,
    max_f32: f32,
    max_s16: i16,
}

impl Default for QuietGuard {
    fn default() -> Self {
        Self::new(true)
    }
}

impl QuietGuard {
    /// Create a new QuietGuard with default -60 dBFS thresholds.
    pub const fn new(enabled: bool) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            violations: AtomicU64::new(0),
            max_f32: QUIET_MAX_AMPLITUDE_F32,
            max_s16: QUIET_MAX_AMPLITUDE_S16,
        }
    }

    /// Create a new QuietGuard with custom thresholds.
    pub const fn with_thresholds(enabled: bool, max_f32: f32, max_s16: i16) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            violations: AtomicU64::new(0),
            max_f32,
            max_s16,
        }
    }

    /// Whether quiet-mode clamping is currently active.
    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Enable or disable quiet-mode clamping.
    #[inline]
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Read the total number of sample-level quiet-mode violations encountered.
    #[inline]
    pub fn violations(&self) -> u64 {
        self.violations.load(Ordering::Relaxed)
    }

    /// Reset the violation counter to zero.
    #[inline]
    pub fn reset_violations(&self) {
        self.violations.store(0, Ordering::Relaxed);
    }

    /// Process a slice of interleaved f32 PCM samples in-place.
    ///
    /// If quiet mode is active, any sample with absolute magnitude > max_f32
    /// is clamped in-place, and the atomic violation counter is incremented.
    ///
    /// Returns the number of violations detected in this slice.
    #[inline]
    pub fn process_f32(&self, samples: &mut [f32]) -> u64 {
        if !self.is_enabled() {
            return 0;
        }

        let max = self.max_f32;
        let min = -max;
        let mut count = 0u64;

        for s in samples.iter_mut() {
            if *s > max {
                *s = max;
                count += 1;
            } else if *s < min {
                *s = min;
                count += 1;
            }
        }

        if count > 0 {
            self.violations.fetch_add(count, Ordering::Relaxed);
        }

        count
    }

    /// Process a slice of interleaved 16-bit signed PCM samples in-place.
    ///
    /// If quiet mode is active, any sample with absolute magnitude > max_s16
    /// is clamped in-place, and the atomic violation counter is incremented.
    ///
    /// Returns the number of violations detected in this slice.
    #[inline]
    pub fn process_s16(&self, samples: &mut [i16]) -> u64 {
        if !self.is_enabled() {
            return 0;
        }

        let max = self.max_s16;
        let min = -max;
        let mut count = 0u64;

        for s in samples.iter_mut() {
            if *s > max {
                *s = max;
                count += 1;
            } else if *s < min {
                *s = min;
                count += 1;
            }
        }

        if count > 0 {
            self.violations.fetch_add(count, Ordering::Relaxed);
        }

        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quiet_guard_f32_clamping_and_counter() {
        let guard = QuietGuard::new(true);
        assert_eq!(guard.violations(), 0);

        // Within limits (-60 dBFS = 0.001)
        let mut quiet_data = [0.0f32, 0.0005, -0.0008, 0.001, -0.001];
        let original_copy = quiet_data;
        let v = guard.process_f32(&mut quiet_data);
        assert_eq!(v, 0);
        assert_eq!(guard.violations(), 0);
        assert_eq!(quiet_data, original_copy);

        // Exceeding limits (e.g. 0.5, -0.8)
        let mut loud_data = [0.0005f32, 0.5, -0.8, -0.0001, 1.0];
        let v2 = guard.process_f32(&mut loud_data);
        assert_eq!(v2, 3);
        assert_eq!(guard.violations(), 3);

        // Clamped values check
        assert_eq!(loud_data[0], 0.0005);
        assert_eq!(loud_data[1], QUIET_MAX_AMPLITUDE_F32);
        assert_eq!(loud_data[2], -QUIET_MAX_AMPLITUDE_F32);
        assert_eq!(loud_data[3], -0.0001);
        assert_eq!(loud_data[4], QUIET_MAX_AMPLITUDE_F32);

        // Process another batch accumulates violations
        let mut more_loud = [0.2f32, -0.3];
        guard.process_f32(&mut more_loud);
        assert_eq!(guard.violations(), 5);

        // Reset
        guard.reset_violations();
        assert_eq!(guard.violations(), 0);
    }

    #[test]
    fn test_quiet_guard_s16_clamping() {
        let guard = QuietGuard::new(true);

        let mut data = [0i16, 20, -25, 1000, -5000, 32, -32];
        let v = guard.process_s16(&mut data);
        assert_eq!(v, 2);
        assert_eq!(guard.violations(), 2);

        assert_eq!(data[0], 0);
        assert_eq!(data[1], 20);
        assert_eq!(data[2], -25);
        assert_eq!(data[3], QUIET_MAX_AMPLITUDE_S16);
        assert_eq!(data[4], -QUIET_MAX_AMPLITUDE_S16);
        assert_eq!(data[5], 32);
        assert_eq!(data[6], -32);
    }

    #[test]
    fn test_quiet_guard_disabled() {
        let guard = QuietGuard::new(false);
        let mut loud = [0.9f32, -0.9];
        let v = guard.process_f32(&mut loud);
        assert_eq!(v, 0);
        assert_eq!(guard.violations(), 0);
        assert_eq!(loud, [0.9, -0.9]);
    }
}
