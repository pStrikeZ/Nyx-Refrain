//! Runtime-adjustable sample rate conversion using `rubato` async resampler.
//!
//! Provides:
//! - Resampling from source sample rate (e.g. 48000 Hz) to RAOP standard 44100 Hz.
//! - Runtime ratio adjustment for clock drift compensation (`set_relative_ratio`).
//! - Passthrough mode when input is 44100 Hz and no drift adjustment is active.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Adjustable, Async, FixedAsync, PolynomialDegree, Resampler, SincInterpolationParameters,
};

/// Default internal chunk size in frames for the resampler (10 ms at 48 kHz).
pub const DEFAULT_RESAMPLER_CHUNK_FRAMES: usize = 480;

/// Target output sample rate for RAOP audio (44.1 kHz).
pub const TARGET_SAMPLE_RATE: u32 = 44_100;

/// Interpolation used by the resampler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    /// Cubic polynomial: cheapest, but no anti-aliasing filter (high-frequency roll-off and
    /// aliasing of 22.05–24 kHz content when converting 48 kHz to 44.1 kHz).
    Cubic,
    /// Windowed sinc (256 taps, Blackman-Harris², automatic cutoff): band-limited, flat to
    /// ~20 kHz with strong alias rejection; adds ~128 input frames of delay.
    Sinc,
}

/// Sample rate converter with runtime ratio adjustment for drift compensation.
pub struct ResamplePipeline {
    resampler: Option<Async<f32>>,
    source_sample_rate: u32,
    chunk_frames: usize,
    /// Interleaved stereo staging buffer for incoming samples.
    staging_in: Vec<f32>,
    /// Intermediate buffer for resampler output.
    resampler_out: Vec<f32>,
    /// Current relative ratio applied.
    current_relative_ratio: f64,
}

impl ResamplePipeline {
    /// Create a new resample pipeline from `source_sample_rate` to 44100 Hz.
    ///
    /// If `source_sample_rate == 44100`, creates resampler with base ratio 1.0 to allow
    /// dynamic drift adjustment, or can operate in passthrough mode.
    pub fn new(source_sample_rate: u32, chunk_frames: usize) -> Result<Self, String> {
        Self::with_interpolation(source_sample_rate, chunk_frames, Interpolation::Sinc)
    }

    /// Like [`ResamplePipeline::new`] with an explicit interpolation type.
    pub fn with_interpolation(
        source_sample_rate: u32,
        chunk_frames: usize,
        interpolation: Interpolation,
    ) -> Result<Self, String> {
        let chunk_size = if chunk_frames == 0 {
            DEFAULT_RESAMPLER_CHUNK_FRAMES
        } else {
            chunk_frames
        };

        let base_ratio = (TARGET_SAMPLE_RATE as f64) / (source_sample_rate as f64);

        // Maximum relative ratio ±1% (10,000 ppm), easily covering the ±500 ppm drift window
        let max_relative_ratio = 1.01;

        let resampler = match interpolation {
            Interpolation::Cubic => Async::<f32>::new_poly(
                base_ratio,
                max_relative_ratio,
                PolynomialDegree::Cubic,
                chunk_size,
                2,
                FixedAsync::Input,
            ),
            Interpolation::Sinc => Async::<f32>::new_sinc(
                base_ratio,
                max_relative_ratio,
                &SincInterpolationParameters::default(),
                chunk_size,
                2,
                FixedAsync::Input,
            ),
        }
        .map_err(|e| format!("Failed to create rubato resampler: {e}"))?;

        let max_out_frames = resampler.output_frames_max();

        Ok(Self {
            resampler: Some(resampler),
            source_sample_rate,
            chunk_frames: chunk_size,
            staging_in: Vec::with_capacity(chunk_size * 4),
            resampler_out: vec![0.0f32; max_out_frames * 2],
            current_relative_ratio: 1.0,
        })
    }

    /// Delay added by the resampler, in output frames (0 in 44.1 kHz passthrough).
    pub fn output_delay_frames(&self) -> usize {
        self.resampler.as_ref().map_or(0, |r| r.output_delay())
    }

    /// Source sample rate in Hz.
    pub fn source_sample_rate(&self) -> u32 {
        self.source_sample_rate
    }

    /// Update resampler ratio relative to nominal base ratio (e.g. `1.0002` for +200 ppm).
    pub fn set_relative_ratio(&mut self, relative_ratio: f64) -> Result<(), String> {
        if (self.current_relative_ratio - relative_ratio).abs() < 1e-7 {
            return Ok(());
        }
        self.current_relative_ratio = relative_ratio;
        if let Some(ref mut resampler) = self.resampler {
            resampler
                .set_resample_ratio_relative(relative_ratio, false)
                .map_err(|e| format!("Failed to set resample ratio: {e}"))?;
        }
        Ok(())
    }

    /// Current relative ratio applied.
    pub fn current_relative_ratio(&self) -> f64 {
        self.current_relative_ratio
    }

    /// Push interleaved stereo `f32` samples into resampler, appending 44.1 kHz output
    /// samples into `output`.
    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) -> Result<(), String> {
        // If 44.1 kHz passthrough and ratio is exactly 1.0, bypass resampler
        if self.source_sample_rate == TARGET_SAMPLE_RATE
            && (self.current_relative_ratio - 1.0).abs() < 1e-9
        {
            if !self.staging_in.is_empty() {
                output.extend_from_slice(&self.staging_in);
                self.staging_in.clear();
            }
            output.extend_from_slice(input);
            return Ok(());
        }

        self.staging_in.extend_from_slice(input);

        let resampler = match self.resampler.as_mut() {
            Some(r) => r,
            None => {
                output.extend_from_slice(&self.staging_in);
                self.staging_in.clear();
                return Ok(());
            }
        };

        let chunk_samples = self.chunk_frames * 2;
        let mut consumed_samples = 0;

        while self.staging_in.len() - consumed_samples >= chunk_samples {
            let slice = &self.staging_in[consumed_samples..consumed_samples + chunk_samples];
            let in_adapter = InterleavedSlice::new(slice, 2, self.chunk_frames)
                .map_err(|e| format!("Input slice error: {e}"))?;

            let max_out_frames = resampler.output_frames_max();
            if self.resampler_out.len() < max_out_frames * 2 {
                self.resampler_out.resize(max_out_frames * 2, 0.0);
            }

            let mut out_adapter =
                InterleavedSlice::new_mut(&mut self.resampler_out, 2, max_out_frames)
                    .map_err(|e| format!("Output slice error: {e}"))?;

            let (_used_frames, produced_frames) = resampler
                .process_into_buffer(&in_adapter, &mut out_adapter, None)
                .map_err(|e| format!("Resampling error: {e}"))?;

            output.extend_from_slice(&self.resampler_out[..produced_frames * 2]);
            consumed_samples += chunk_samples;
        }

        if consumed_samples > 0 {
            if consumed_samples == self.staging_in.len() {
                self.staging_in.clear();
            } else {
                self.staging_in.drain(..consumed_samples);
            }
        }

        Ok(())
    }

    /// Flush remaining staged samples by padding with silence if needed.
    pub fn flush(&mut self, output: &mut Vec<f32>) -> Result<(), String> {
        if self.staging_in.is_empty() {
            return Ok(());
        }

        let chunk_samples = self.chunk_frames * 2;
        while self.staging_in.len() < chunk_samples {
            self.staging_in.push(0.0);
        }

        let dummy = Vec::new();
        self.process(&dummy, output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resample_48000_to_44100() {
        let mut pipeline = ResamplePipeline::new(48000, 480).unwrap();
        assert_eq!(pipeline.source_sample_rate(), 48000);

        // Feed 1 second of 48 kHz stereo audio = 48000 frames = 96000 samples
        let input: Vec<f32> = (0..96000)
            .map(|i| ((i as f32) * 0.01).sin() * 0.1)
            .collect();

        let mut output = Vec::new();
        pipeline.process(&input, &mut output).unwrap();
        pipeline.flush(&mut output).unwrap();

        let output_frames = output.len() / 2;
        // Output should be ~44100 frames (within ±10 frames due to filter ramp/delay)
        assert!(
            (output_frames as i64 - 44100).abs() < 50,
            "Expected ~44100 frames, got {}",
            output_frames
        );
    }

    #[test]
    fn test_resample_ratio_adjustment() {
        let mut pipeline = ResamplePipeline::new(48000, 480).unwrap();
        // Adjust ratio by +200 ppm
        pipeline.set_relative_ratio(1.0002).unwrap();
        assert_eq!(pipeline.current_relative_ratio(), 1.0002);

        let input = vec![0.0f32; 960];
        let mut output = Vec::new();
        pipeline.process(&input, &mut output).unwrap();
        assert!(!output.is_empty());
    }

    #[test]
    fn test_passthrough_44100() {
        let mut pipeline = ResamplePipeline::new(44100, 441).unwrap();
        let input = vec![0.1f32, 0.2, 0.3, 0.4];
        let mut output = Vec::new();
        pipeline.process(&input, &mut output).unwrap();
        assert_eq!(output, input);
    }

    /// Amplitude of `freq` in `x` (Hann-windowed Goertzel), relative to a full-scale sine.
    fn tone_amplitude(x: &[f32], rate: f64, freq: f64) -> f64 {
        let n = x.len();
        let w = 2.0 * std::f64::consts::PI * freq / rate;
        let coeff = 2.0 * w.cos();
        let (mut s1, mut s2, mut wsum) = (0.0f64, 0.0f64, 0.0f64);
        for (i, &v) in x.iter().enumerate() {
            let hann = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
            wsum += hann;
            let s0 = v as f64 * hann + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let power = s1 * s1 + s2 * s2 - coeff * s1 * s2;
        2.0 * power.max(0.0).sqrt() / wsum
    }

    fn db(a: f64) -> f64 {
        20.0 * a.max(1e-12).log10()
    }

    /// Resamples one second of a 48 kHz stereo sine (amplitude 0.5) and returns the left
    /// channel of the 44.1 kHz output with the filter start-up skipped.
    fn resample_tone(interp: Interpolation, freq: f64) -> Vec<f32> {
        let mut p = ResamplePipeline::with_interpolation(48_000, 480, interp).unwrap();
        let input: Vec<f32> = (0..48_000)
            .flat_map(|i| {
                let v =
                    (0.5 * (2.0 * std::f64::consts::PI * freq * i as f64 / 48_000.0).sin()) as f32;
                [v, v]
            })
            .collect();
        let mut out = Vec::new();
        p.process(&input, &mut out).unwrap();
        let skip = p.output_delay_frames() + 1024;
        out.chunks_exact(2)
            .skip(skip)
            .take(32_768)
            .map(|f| f[0])
            .collect()
    }

    #[test]
    fn sinc_is_flat_to_20k_and_rejects_aliases() {
        for (interp, name) in [
            (Interpolation::Cubic, "cubic"),
            (Interpolation::Sinc, "sinc"),
        ] {
            let gains: Vec<f64> = [1_000.0, 10_000.0, 18_000.0, 20_000.0]
                .iter()
                .map(|&f| db(tone_amplitude(&resample_tone(interp, f), 44_100.0, f) / 0.5))
                .collect();
            // 23 kHz cannot be represented at 44.1 kHz; it folds to 44.1 - 23 = 21.1 kHz.
            let alias =
                db(tone_amplitude(&resample_tone(interp, 23_000.0), 44_100.0, 21_100.0) / 0.5);
            eprintln!(
                "{name:>5}: 1k {:+.2} dB, 10k {:+.2} dB, 18k {:+.2} dB, 20k {:+.2} dB, alias of 23k at 21.1k {:.1} dB",
                gains[0], gains[1], gains[2], gains[3], alias
            );
            if interp == Interpolation::Sinc {
                assert!(
                    gains.iter().all(|g| g.abs() < 0.2),
                    "sinc passband {gains:?}"
                );
                assert!(alias < -60.0, "sinc alias {alias:.1} dB");
            }
        }
    }

    #[test]
    fn sinc_delay_is_small() {
        let p = ResamplePipeline::new(48_000, 480).unwrap();
        let ms = p.output_delay_frames() as f64 * 1000.0 / 44_100.0;
        eprintln!(
            "sinc resampler delay: {} frames ({ms:.2} ms)",
            p.output_delay_frames()
        );
        assert!(ms < 5.0, "delay {ms:.2} ms");
    }

    /// `cargo test --release -p pipeline resample_cost -- --ignored --nocapture`
    #[test]
    #[ignore = "timing benchmark; run in release"]
    fn resample_cost() {
        for interp in [Interpolation::Cubic, Interpolation::Sinc] {
            let mut p = ResamplePipeline::with_interpolation(48_000, 480, interp).unwrap();
            let chunk: Vec<f32> = (0..960).map(|i| ((i as f32) * 0.01).sin() * 0.3).collect();
            let mut out = Vec::with_capacity(4096);
            let t = std::time::Instant::now();
            for _ in 0..6_000 {
                out.clear();
                p.process(&chunk, &mut out).unwrap();
            }
            let secs = t.elapsed().as_secs_f64();
            eprintln!(
                "{interp:?}: 60 s of 48 kHz stereo in {:.1} ms ({:.3}% of one core)",
                secs * 1000.0,
                secs / 60.0 * 100.0
            );
        }
    }
}
