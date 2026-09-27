//! Audio format conversion: channel downmixing and TPDF dithered quantization.
//!
//! Provides:
//! - Channel downmix: arbitrary input channel count downmixed to stereo (L/R)
//!   following ITU-R BS.775 recommendations.
//! - Quantization: 32-bit floating point `[-1.0, 1.0]` to 16-bit signed integer
//!   `[-32768, 32767]` with Triangular Probability Density Function (TPDF) dither.

/// Standard ITU-R BS.775 center / surround attenuation coefficient: 1 / sqrt(2) ≈ 0.70710678.
pub const DOWNMIX_ATTENUATION_3DB: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Downmix an arbitrary number of interleaved audio channels to stereo (2 channels).
///
/// # Channel Layout and Downmixing Rules
///
/// 1. **1 Channel (Mono)**:
///    - `L = M`
///    - `R = M`
///      (Dual-mono presentation).
///
/// 2. **2 Channels (Stereo: L, R)**:
///    - `L = L`
///    - `R = R`
///      (Passthrough).
///
/// 3. **3 Channels (3.0 / 2.1: L, R, C)**:
///    - `L = L + 0.7071 * C`
///    - `R = R + 0.7071 * C`
///      (ITU-R BS.775 center channel folding).
///
/// 4. **4 Channels (Quadraphonic: L, R, SL, SR)**:
///    - `L = L + 0.7071 * SL`
///    - `R = R + 0.7071 * SR`
///
/// 5. **6 Channels (5.1: L, R, C, LFE, SL, SR)**:
///    - `L = L + 0.7071 * C + 0.7071 * SL`
///    - `R = R + 0.7071 * C + 0.7071 * SR`
///      (Per ITU-R BS.775 §2.2, LFE channel is omitted in standard stereo downmix
///      to prevent bass distortion / phase cancellation).
///
/// 6. **8 Channels (7.1: L, R, C, LFE, SL, SR, BL, BR)**:
///    - `L = L + 0.7071 * C + 0.7071 * SL + 0.7071 * BL`
///    - `R = R + 0.7071 * C + 0.7071 * SR + 0.7071 * BR`
///
/// 7. **Generic N > 2 Channels**:
///    - `L = L_in + sum(even channels >= 2) * (0.7071 / num_extra_l)`
///    - `R = R_in + sum(odd channels >= 3) * (0.7071 / num_extra_r)`
pub fn downmix_to_stereo(input: &[f32], channels: u16, output: &mut Vec<f32>) {
    if channels == 0 {
        output.clear();
        return;
    }

    let ch = channels as usize;
    let frames = input.len() / ch;
    output.clear();
    output.reserve(frames * 2);

    match channels {
        1 => {
            for &s in input.iter().take(frames) {
                output.push(s);
                output.push(s);
            }
        }
        2 => {
            output.extend_from_slice(&input[..frames * 2]);
        }
        3 => {
            for f in 0..frames {
                let base = f * 3;
                let l = input[base];
                let r = input[base + 1];
                let c = input[base + 2];
                let folded = c * DOWNMIX_ATTENUATION_3DB;
                output.push(l + folded);
                output.push(r + folded);
            }
        }
        4 => {
            for f in 0..frames {
                let base = f * 4;
                let l = input[base];
                let r = input[base + 1];
                let sl = input[base + 2];
                let sr = input[base + 3];
                output.push(l + sl * DOWNMIX_ATTENUATION_3DB);
                output.push(r + sr * DOWNMIX_ATTENUATION_3DB);
            }
        }
        6 => {
            // 5.1: L, R, C, LFE, SL, SR
            for f in 0..frames {
                let base = f * 6;
                let l = input[base];
                let r = input[base + 1];
                let c = input[base + 2];
                // input[base + 3] is LFE (omitted)
                let sl = input[base + 4];
                let sr = input[base + 5];
                let c_folded = c * DOWNMIX_ATTENUATION_3DB;
                output.push(l + c_folded + sl * DOWNMIX_ATTENUATION_3DB);
                output.push(r + c_folded + sr * DOWNMIX_ATTENUATION_3DB);
            }
        }
        8 => {
            // 7.1: L, R, C, LFE, SL, SR, BL, BR
            for f in 0..frames {
                let base = f * 8;
                let l = input[base];
                let r = input[base + 1];
                let c = input[base + 2];
                // base + 3 is LFE
                let sl = input[base + 4];
                let sr = input[base + 5];
                let bl = input[base + 6];
                let br = input[base + 7];
                let c_folded = c * DOWNMIX_ATTENUATION_3DB;
                output.push(l + c_folded + (sl + bl) * DOWNMIX_ATTENUATION_3DB);
                output.push(r + c_folded + (sr + br) * DOWNMIX_ATTENUATION_3DB);
            }
        }
        _ => {
            // Generic N channels
            let extra_l_count = (ch - 2).div_ceil(2);
            let extra_r_count = (ch - 2) / 2;
            let scale_l = if extra_l_count > 0 {
                DOWNMIX_ATTENUATION_3DB / (extra_l_count as f32)
            } else {
                0.0
            };
            let scale_r = if extra_r_count > 0 {
                DOWNMIX_ATTENUATION_3DB / (extra_r_count as f32)
            } else {
                0.0
            };

            for f in 0..frames {
                let base = f * ch;
                let mut l = input[base];
                let mut r = input[base + 1];
                for k in 2..ch {
                    if (k % 2) == 0 {
                        l += input[base + k] * scale_l;
                    } else {
                        r += input[base + k] * scale_r;
                    }
                }
                output.push(l);
                output.push(r);
            }
        }
    }
}

/// Fast, deterministic, lock-free 64-bit Xorshift PRNG for audio dithering.
///
/// Zero heap allocations, zero system calls, highly suitable for real-time audio threads.
#[derive(Debug, Clone)]
pub struct Xorshift64 {
    state: u64,
}

impl Default for Xorshift64 {
    fn default() -> Self {
        Self::new(0x853c49e6748fea9b)
    }
}

impl Xorshift64 {
    /// Create a new PRNG with non-zero seed.
    pub const fn new(seed: u64) -> Self {
        let state = if seed == 0 { 0x853c49e6748fea9b } else { seed };
        Self { state }
    }

    /// Generate next pseudo-random u64.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// Generate uniform random f32 in `[-0.5, 0.5)`.
    #[inline]
    pub fn next_uniform_sym(&mut self) -> f32 {
        let u = (self.next_u64() >> 40) as u32; // 24-bit random integer
        (u as f32) / (16_777_216.0) - 0.5
    }
}

/// Triangular Probability Density Function (TPDF) dither generator.
///
/// TPDF dither is generated by summing two independent uniform random variables
/// in `[-0.5, 0.5)`, producing a triangular PDF centered at 0 with support in `(-1.0, 1.0)` LSB.
///
/// When added prior to quantization, TPDF dither completely eliminates harmonic
/// distortion and limit cycles without modulating the noise floor.
#[derive(Debug, Clone)]
pub struct TpdfDither {
    rng: Xorshift64,
}

impl Default for TpdfDither {
    fn default() -> Self {
        Self::new(0xdeadbeef_cafebabe)
    }
}

impl TpdfDither {
    /// Create a new TpdfDither with specified seed.
    pub const fn new(seed: u64) -> Self {
        Self {
            rng: Xorshift64::new(seed),
        }
    }

    /// Quantize a single normalized `f32` sample `[-1.0, 1.0]` to `i16` with TPDF dither.
    #[inline]
    pub fn quantize_sample(&mut self, sample: f32) -> i16 {
        // Two independent uniform random variables in [-0.5, 0.5)
        let r1 = self.rng.next_uniform_sym();
        let r2 = self.rng.next_uniform_sym();
        let dither_lsb = r1 - r2; // TPDF in (-1.0, 1.0) LSB

        // Standard 16-bit signed scale: 32767.0
        let scaled = sample * 32767.0 + dither_lsb;
        let clamped = scaled.clamp(-32768.0, 32767.0);
        clamped.round() as i16
    }

    /// Quantize a slice of normalized `f32` samples into `output` `i16` buffer with TPDF dither.
    #[inline]
    pub fn quantize_slice(&mut self, input: &[f32], output: &mut [i16]) {
        let len = input.len().min(output.len());
        for i in 0..len {
            output[i] = self.quantize_sample(input[i]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_downmix_mono_to_stereo() {
        let mono = vec![0.5f32, -0.25, 0.8];
        let mut stereo = Vec::new();
        downmix_to_stereo(&mono, 1, &mut stereo);
        assert_eq!(stereo.len(), 6);
        assert_eq!(stereo[0], 0.5);
        assert_eq!(stereo[1], 0.5);
        assert_eq!(stereo[2], -0.25);
        assert_eq!(stereo[3], -0.25);
    }

    #[test]
    fn test_downmix_stereo_passthrough() {
        let stereo_in = vec![0.1f32, -0.1, 0.2, -0.2];
        let mut stereo_out = Vec::new();
        downmix_to_stereo(&stereo_in, 2, &mut stereo_out);
        assert_eq!(stereo_out, stereo_in);
    }

    #[test]
    fn test_downmix_5_1_to_stereo() {
        // L=0.5, R=0.4, C=0.2, LFE=0.9, SL=0.1, SR=0.15
        let surround = vec![0.5f32, 0.4, 0.2, 0.9, 0.1, 0.15];
        let mut stereo = Vec::new();
        downmix_to_stereo(&surround, 6, &mut stereo);
        assert_eq!(stereo.len(), 2);
        let expected_l = 0.5 + 0.2 * DOWNMIX_ATTENUATION_3DB + 0.1 * DOWNMIX_ATTENUATION_3DB;
        let expected_r = 0.4 + 0.2 * DOWNMIX_ATTENUATION_3DB + 0.15 * DOWNMIX_ATTENUATION_3DB;
        assert!((stereo[0] - expected_l).abs() < 1e-6);
        assert!((stereo[1] - expected_r).abs() < 1e-6);
    }

    #[test]
    fn test_tpdf_dither_distribution() {
        let mut dither = TpdfDither::default();
        let num_samples = 10_000;
        let mut sum_diff = 0.0f64;
        let mut sum_sq_diff = 0.0f64;

        // Convert DC level 0.0; quantization error should be centered at 0 with variance ~ 1/3 LSB^2
        for _ in 0..num_samples {
            let s16 = dither.quantize_sample(0.0);
            sum_diff += s16 as f64;
            sum_sq_diff += (s16 as f64) * (s16 as f64);
        }

        let mean = sum_diff / (num_samples as f64);
        let var = sum_sq_diff / (num_samples as f64) - mean * mean;

        // Mean should be very close to 0 LSB (< 0.05)
        assert!(mean.abs() < 0.05, "Mean dither error too large: {}", mean);
        // Variance of sum of two U(-0.5, 0.5) is 1/12 + 1/12 = 1/6 ≈ 0.1667 (before rounding)
        // After rounding, variance is close to 0.33 - 0.45
        assert!(
            var > 0.2 && var < 0.6,
            "Dither variance unexpected: {}",
            var
        );
    }

    #[test]
    fn test_tpdf_clamping() {
        let mut dither = TpdfDither::default();
        // High values must not overflow i16
        for _ in 0..100 {
            let max_val = dither.quantize_sample(1.0);
            assert!(max_val >= 32760);
            let min_val = dither.quantize_sample(-1.0);
            assert!(min_val <= -32760);
        }
    }
}
