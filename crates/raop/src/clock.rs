//! Clock model and timestamp mapping.
//!
//! Maintains mapping between the monotonic clock (`Instant`), NTP timestamps
//! (64-bit, 1900 epoch, 32.32 fixed-point), and 44.1 kHz RTP timestamps:
//! `rtp_ts(t) = rtp_base + (t - t_base) * 44100`.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// SOURCE: references/owntone-server/src/outputs/rtp_common.c:46 (#define NTP_EPOCH_DELTA 0x83aa7e80)
// SOURCE: RFC 5905 §6 (NTP timestamp format 32.32, 1900 epoch)
// 1900-01-01 to 1970-01-01 in seconds: 70 years + 17 leap days = 2,208,988,800 seconds.
pub const NTP_EPOCH_OFFSET_SECS: u64 = 2_208_988_800;

/// Standard RAOP audio sample rate (44.1 kHz).
// SOURCE: references/libraop/src/raop_client.c:212
pub const DEFAULT_SAMPLE_RATE: u32 = 44_100;

/// 64-bit NTP timestamp in 32.32 fixed-point representation (1900 epoch).
///
/// Upper 32 bits: integer seconds since 1900-01-01 00:00:00 UTC.
/// Lower 32 bits: fractional second (1 unit = 2^-32 seconds ≈ 0.2328 ns).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct NtpTimestamp {
    pub seconds: u32,
    pub fraction: u32,
}

impl NtpTimestamp {
    /// Create a new NTP timestamp with given seconds and fraction.
    pub const fn new(seconds: u32, fraction: u32) -> Self {
        Self { seconds, fraction }
    }

    /// Construct from raw 64-bit integer (`(seconds << 32) | fraction`).
    pub const fn from_u64(val: u64) -> Self {
        Self {
            seconds: (val >> 32) as u32,
            fraction: val as u32,
        }
    }

    /// Convert to raw 64-bit integer.
    pub const fn to_u64(self) -> u64 {
        ((self.seconds as u64) << 32) | (self.fraction as u64)
    }

    /// Convert to 8-byte big-endian network wire format.
    pub const fn to_be_bytes(self) -> [u8; 8] {
        self.to_u64().to_be_bytes()
    }

    /// Construct from 8-byte big-endian network wire format.
    pub const fn from_be_bytes(bytes: [u8; 8]) -> Self {
        Self::from_u64(u64::from_be_bytes(bytes))
    }

    /// Capture NTP timestamp from standard `SystemTime`.
    pub fn from_system_time(st: SystemTime) -> Self {
        let d = st.duration_since(UNIX_EPOCH).unwrap_or_default();
        let secs = d.as_secs().wrapping_add(NTP_EPOCH_OFFSET_SECS) as u32;
        let frac = (((d.subsec_nanos() as u64) << 32) / 1_000_000_000) as u32;
        Self {
            seconds: secs,
            fraction: frac,
        }
    }

    /// Add a `Duration` to the NTP timestamp using exact integer math.
    pub fn add_duration(self, duration: Duration) -> Self {
        let add_secs = duration.as_secs();
        let add_nanos = duration.subsec_nanos();
        let add_frac = ((add_nanos as u64) << 32) / 1_000_000_000;
        let total_frac = (self.fraction as u64) + add_frac;
        let carry = (total_frac >> 32) as u32;
        let new_secs = self
            .seconds
            .wrapping_add((add_secs as u32).wrapping_add(carry));
        let new_frac = total_frac as u32;
        Self {
            seconds: new_secs,
            fraction: new_frac,
        }
    }

    /// Subtract a `Duration` from the NTP timestamp using exact integer math.
    pub fn sub_duration(self, duration: Duration) -> Self {
        let sub_secs = duration.as_secs();
        let sub_nanos = duration.subsec_nanos();
        let sub_frac = ((sub_nanos as u64) << 32) / 1_000_000_000;
        let (new_frac, borrow) = if (self.fraction as u64) >= sub_frac {
            (((self.fraction as u64) - sub_frac) as u32, 0u32)
        } else {
            (
                ((self.fraction as u64) + (1u64 << 32) - sub_frac) as u32,
                1u32,
            )
        };
        let new_secs = self
            .seconds
            .wrapping_sub((sub_secs as u32).wrapping_add(borrow));
        Self {
            seconds: new_secs,
            fraction: new_frac,
        }
    }
}

/// Convert a `Duration` to sample frames at given sample rate with exact integer math.
#[inline]
pub fn duration_to_frames(d: Duration, sample_rate: u32) -> u64 {
    let secs = d.as_secs();
    let nanos = d.subsec_nanos() as u64;
    let frames_from_secs = secs.saturating_mul(sample_rate as u64);
    let frames_from_nanos = (nanos * (sample_rate as u64)) / 1_000_000_000;
    frames_from_secs.wrapping_add(frames_from_nanos)
}

/// Convert a frame count to `Duration` at given sample rate with exact integer math.
#[inline]
pub fn frames_to_duration(frames: u64, sample_rate: u32) -> Duration {
    if sample_rate == 0 {
        return Duration::ZERO;
    }
    let secs = frames / (sample_rate as u64);
    let rem = frames % (sample_rate as u64);
    // Round up so that `duration_to_frames(frames_to_duration(n)) == n` exactly;
    // flooring here made the first audio packet's RTP timestamp one frame earlier
    // than the RTP-Info rtptime announced in RECORD.
    let nanos = (rem * 1_000_000_000).div_ceil(sample_rate as u64);
    Duration::new(secs, nanos as u32)
}

/// Monotonic clock model and coordinate mapper for RAOP sessions.
///
/// Clock synchronization model:
/// - Single monotonic time source (`Instant`), free from wall-clock jumps.
/// - Fixed base offset captured at session start.
/// - RTP timestamp formula: `rtp_ts(t) = rtp_base + (t - t_base) * sample_rate`.
///
/// Safe for real-time threads: zero allocations, lock-free, no I/O, no async.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    t_base: Instant,
    ntp_base: NtpTimestamp,
    rtp_base: u32,
    sample_rate: u32,
}

impl Clock {
    /// Initialize clock with explicit base points and default 44.1 kHz sample rate.
    pub const fn new(t_base: Instant, ntp_base: NtpTimestamp, rtp_base: u32) -> Self {
        Self {
            t_base,
            ntp_base,
            rtp_base,
            sample_rate: DEFAULT_SAMPLE_RATE,
        }
    }

    /// Initialize clock with explicit base points and custom sample rate.
    pub const fn with_sample_rate(
        t_base: Instant,
        ntp_base: NtpTimestamp,
        rtp_base: u32,
        sample_rate: u32,
    ) -> Self {
        Self {
            t_base,
            ntp_base,
            rtp_base,
            sample_rate,
        }
    }

    /// Initialize clock capturing current instant and wall-clock NTP offset, with a random
    /// rtp_base (as iTunes/pyatv/libraop do) so timestamps never start near zero and
    /// `rtp_now - latency` in sync packets does not wrap.
    pub fn init() -> Self {
        let t_base = Instant::now();
        let ntp_base = NtpTimestamp::from_system_time(SystemTime::now());
        Self::new(t_base, ntp_base, rand::random::<u32>())
    }

    /// Monotonic base instant.
    #[inline]
    pub const fn t_base(&self) -> Instant {
        self.t_base
    }

    /// NTP timestamp corresponding to `t_base`.
    #[inline]
    pub const fn ntp_base(&self) -> NtpTimestamp {
        self.ntp_base
    }

    /// RTP timestamp corresponding to `t_base`.
    #[inline]
    pub const fn rtp_base(&self) -> u32 {
        self.rtp_base
    }

    /// Active sample rate in Hz.
    #[inline]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Compute NTP timestamp at given monotonic instant.
    #[inline]
    pub fn ntp_at(&self, t: Instant) -> NtpTimestamp {
        if t >= self.t_base {
            let elapsed = t.duration_since(self.t_base);
            self.ntp_base.add_duration(elapsed)
        } else {
            let elapsed = self.t_base.duration_since(t);
            self.ntp_base.sub_duration(elapsed)
        }
    }

    /// Compute current NTP timestamp.
    #[inline]
    pub fn now_ntp(&self) -> NtpTimestamp {
        self.ntp_at(Instant::now())
    }

    /// Compute 32-bit RTP timestamp at given monotonic instant using exact math without drift.
    #[inline]
    pub fn rtp_ts(&self, t: Instant) -> u32 {
        if t >= self.t_base {
            let elapsed = t.duration_since(self.t_base);
            let frames = duration_to_frames(elapsed, self.sample_rate);
            self.rtp_base.wrapping_add(frames as u32)
        } else {
            let elapsed = self.t_base.duration_since(t);
            let frames = duration_to_frames(elapsed, self.sample_rate);
            self.rtp_base.wrapping_sub(frames as u32)
        }
    }

    /// Compute current 32-bit RTP timestamp.
    #[inline]
    pub fn now_rtp(&self) -> u32 {
        self.rtp_ts(Instant::now())
    }

    /// Monotonic instant corresponding to given RTP timestamp.
    #[inline]
    pub fn instant_at_rtp_ts(&self, ts: u32) -> Instant {
        let diff = ts.wrapping_sub(self.rtp_base);
        self.t_base + frames_to_duration(diff as u64, self.sample_rate)
    }
}

#[cfg(test)]
mod roundtrip_tests {
    use super::*;

    #[test]
    fn frames_duration_roundtrip_is_exact() {
        for n in [
            0u64,
            1,
            351,
            352,
            2253,
            44_099,
            44_100,
            1_234_567,
            44_100 * 3600 + 17,
        ] {
            assert_eq!(
                duration_to_frames(frames_to_duration(n, 44_100), 44_100),
                n,
                "n={n}"
            );
        }
    }

    #[test]
    fn rtp_ts_of_instant_at_rtp_ts_is_identity() {
        let clock = Clock::init();
        for k in 0u32..2000 {
            let ts = clock.rtp_base().wrapping_add(k * 352 + 1);
            assert_eq!(clock.rtp_ts(clock.instant_at_rtp_ts(ts)), ts);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ntp_encoding_round_trip() {
        let ts = NtpTimestamp::new(0x83aa7e80, 0x12345678);

        // to_u64 / from_u64
        let raw = ts.to_u64();
        assert_eq!(raw, 0x83aa7e80_12345678);
        assert_eq!(NtpTimestamp::from_u64(raw), ts);

        // to_be_bytes / from_be_bytes
        let bytes = ts.to_be_bytes();
        assert_eq!(bytes, [0x83, 0xaa, 0x7e, 0x80, 0x12, 0x34, 0x56, 0x78]);
        assert_eq!(NtpTimestamp::from_be_bytes(bytes), ts);
    }

    #[test]
    fn test_ntp_fraction_precision() {
        // 0.5 seconds = 500_000_000 ns -> fraction should be 0x8000_0000 (half of 2^32)
        let t0 = NtpTimestamp::new(100, 0);
        let t_half = t0.add_duration(Duration::from_nanos(500_000_000));
        assert_eq!(t_half.seconds, 100);
        assert_eq!(t_half.fraction, 0x8000_0000);

        // 0.25 seconds = 250_000_000 ns -> fraction should be 0x4000_0000
        let t_quarter = t0.add_duration(Duration::from_nanos(250_000_000));
        assert_eq!(t_quarter.seconds, 100);
        assert_eq!(t_quarter.fraction, 0x4000_0000);

        // Adding 1 second worth of nanoseconds carries exactly into seconds
        let t_carry = t0.add_duration(Duration::from_nanos(1_000_000_000));
        assert_eq!(t_carry.seconds, 101);
        assert_eq!(t_carry.fraction, 0);

        // Subtracting duration
        let t_back = t_carry.sub_duration(Duration::from_nanos(500_000_000));
        assert_eq!(t_back.seconds, 100);
        assert_eq!(t_back.fraction, 0x8000_0000);
    }

    #[test]
    fn test_rtp_ts_monotonic_and_exact_over_10_hours() {
        let t_base = Instant::now();
        let clock = Clock::new(t_base, NtpTimestamp::new(1000, 0), 10_000);

        // Verify exact mapping at 1 second
        let t_1s = t_base + Duration::from_secs(1);
        assert_eq!(clock.rtp_ts(t_1s), 10_000 + 44_100);

        // Verify exact mapping at 10 seconds
        let t_10s = t_base + Duration::from_secs(10);
        assert_eq!(clock.rtp_ts(t_10s), 10_000 + 441_000);

        // Verify exact mapping at 1 hour (3600 s)
        let t_1h = t_base + Duration::from_secs(3600);
        assert_eq!(clock.rtp_ts(t_1h), 10_000 + 3600 * 44_100);

        // Verify exact mapping at 10 hours (36,000 s)
        // 36_000 * 44_100 = 1_587_600_000 frames
        let t_10h = t_base + Duration::from_secs(36_000);
        let expected_10h = 10_000 + 1_587_600_000;
        assert_eq!(clock.rtp_ts(t_10h), expected_10h);

        // Verify strict monotonicity across 10 hours at 1-minute steps
        let mut prev_ts = clock.rtp_ts(t_base);
        for min in 1..=600 {
            let t = t_base + Duration::from_secs(min * 60);
            let ts = clock.rtp_ts(t);
            assert!(
                ts > prev_ts,
                "RTP ts must be strictly monotonic: prev={prev_ts}, curr={ts}"
            );
            assert_eq!(ts, 10_000 + (min as u32 * 60 * 44_100));
            prev_ts = ts;
        }
    }

    #[test]
    fn test_rtp_ts_wraparound() {
        let t_base = Instant::now();
        // Base is near u32::MAX
        let rtp_base = u32::MAX - 22_050; // Half a second before overflow
        let clock = Clock::new(t_base, NtpTimestamp::new(1000, 0), rtp_base);

        // Before wraparound (0.25 s = 11025 frames)
        let t_before = t_base + Duration::from_millis(250);
        assert_eq!(clock.rtp_ts(t_before), rtp_base + 11_025);

        // Exactly at wraparound point (0.5 s = 22050 frames -> exactly u32::MAX)
        let t_at_max = t_base + Duration::from_millis(500);
        assert_eq!(clock.rtp_ts(t_at_max), u32::MAX);

        // After wraparound (1.0 s = 44100 frames -> wraps around to 22049)
        let t_after = t_base + Duration::from_secs(1);
        let expected = rtp_base.wrapping_add(44_100);
        assert_eq!(clock.rtp_ts(t_after), expected);
        assert_eq!(expected, 22_049);
    }

    #[test]
    fn test_duration_to_frames_and_frames_to_duration() {
        let d = Duration::from_secs(2) + Duration::from_millis(500); // 2.5s
        let frames = duration_to_frames(d, 44100);
        assert_eq!(frames, 110250);

        let d_recovered = frames_to_duration(frames, 44100);
        assert_eq!(d, d_recovered);

        // Edge case: sample_rate == 0
        assert_eq!(frames_to_duration(100, 0), Duration::ZERO);
    }

    #[test]
    fn test_clock_accessors_and_custom_sample_rate() {
        let t_base = Instant::now();
        let ntp = NtpTimestamp::new(500, 100);
        let clock = Clock::with_sample_rate(t_base, ntp, 1234, 48000);

        assert_eq!(clock.sample_rate(), 48000);
        assert_eq!(clock.t_base(), t_base);
        assert_eq!(clock.rtp_base(), 1234);
        assert_eq!(clock.ntp_base(), ntp);

        let t_1s = t_base + Duration::from_secs(1);
        assert_eq!(clock.rtp_ts(t_1s), 1234 + 48000);
    }

    #[test]
    fn test_ntp_timestamp_serialization() {
        let ntp = NtpTimestamp::new(0x12345678, 0x9ABCDEF0);
        let raw = ntp.to_u64();
        assert_eq!(raw, 0x123456789ABCDEF0);
        assert_eq!(NtpTimestamp::from_u64(raw), ntp);

        let bytes = ntp.to_be_bytes();
        assert_eq!(bytes, [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0]);
        assert_eq!(NtpTimestamp::from_be_bytes(bytes), ntp);
    }
}
