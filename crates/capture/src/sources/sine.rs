//! Sine wave audio source generating pure tone at specified frequency and dBFS.

use crate::{AudioChunk, AudioSource};
use std::f64::consts::PI;
use std::time::{Duration, Instant};

/// Audio source producing a pure sinusoidal tone.
pub struct SineSource {
    freq: f32,
    dbfs: f32,
    amplitude: f32,
    sample_rate: u32,
    channels: u16,
    total_frames: Option<usize>,
    frames_produced: usize,
    phase: f64,
    start_time: Instant,
    beeps: usize,
    beep_frames: usize,
    silence_frames: usize,
}

impl SineSource {
    /// Create a new sine tone generator.
    ///
    /// Amplitude is computed from dBFS: `10^(dbfs / 20)`.
    /// 0 dBFS = amplitude 1.0, -60 dBFS = amplitude 0.001.
    pub fn new(freq: f32, dbfs: f32, sample_rate: u32, channels: u16) -> Self {
        let amplitude = 10.0f32.powf(dbfs / 20.0);
        let beep_frames = (0.150 * sample_rate as f64).round() as usize;
        let silence_frames = (0.100 * sample_rate as f64).round() as usize;
        Self {
            freq,
            dbfs,
            amplitude,
            sample_rate,
            channels,
            total_frames: None,
            frames_produced: 0,
            phase: 0.0,
            start_time: Instant::now(),
            beeps: 0,
            beep_frames,
            silence_frames,
        }
    }

    /// Set total frame count for finite duration.
    pub fn with_total_frames(mut self, total_frames: usize) -> Self {
        self.total_frames = Some(total_frames);
        self
    }

    /// Set total duration for finite stream.
    pub fn with_duration(mut self, duration: Duration) -> Self {
        let frames = (duration.as_secs_f64() * self.sample_rate as f64).round() as usize;
        self.total_frames = Some(frames);
        self
    }

    /// Configure leading beeps at the start of the stream before continuous tone.
    pub fn with_beeps(mut self, beeps: usize) -> Self {
        self.beeps = beeps;
        self
    }

    /// Configure custom beep tone and silence durations.
    pub fn with_beep_timing(
        mut self,
        beeps: usize,
        beep_duration: Duration,
        silence_duration: Duration,
    ) -> Self {
        self.beeps = beeps;
        self.beep_frames = (beep_duration.as_secs_f64() * self.sample_rate as f64).round() as usize;
        self.silence_frames =
            (silence_duration.as_secs_f64() * self.sample_rate as f64).round() as usize;
        self
    }

    /// Target frequency in Hz.
    pub fn freq(&self) -> f32 {
        self.freq
    }

    /// Target dBFS level.
    pub fn dbfs(&self) -> f32 {
        self.dbfs
    }

    /// Peak amplitude computed from dBFS.
    pub fn amplitude(&self) -> f32 {
        self.amplitude
    }
}

impl AudioSource for SineSource {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn read_chunk(
        &mut self,
        max_frames: usize,
    ) -> Result<Option<AudioChunk>, Box<dyn std::error::Error + Send + Sync>> {
        if max_frames == 0 {
            return Ok(Some(AudioChunk::new(
                Vec::new(),
                self.sample_rate,
                self.channels,
                Instant::now(),
            )));
        }

        let frames_to_generate = if let Some(total) = self.total_frames {
            if self.frames_produced >= total {
                return Ok(None);
            }
            max_frames.min(total - self.frames_produced)
        } else {
            max_frames
        };

        if frames_to_generate == 0 {
            return Ok(None);
        }

        let chunk_time = {
            let elapsed_frames = self.frames_produced as u64;
            let secs = elapsed_frames / self.sample_rate as u64;
            let rem = elapsed_frames % self.sample_rate as u64;
            let nanos = (rem * 1_000_000_000) / self.sample_rate as u64;
            self.start_time + Duration::new(secs, nanos as u32)
        };

        let phase_step = 2.0 * PI * (self.freq as f64) / (self.sample_rate as f64);
        let mut data = Vec::with_capacity(frames_to_generate * self.channels as usize);
        let total_intro_frames = self.beeps * (self.beep_frames + self.silence_frames);
        let cycle_len = self.beep_frames + self.silence_frames;

        for i in 0..frames_to_generate {
            let frame = self.frames_produced + i;
            let sample = if frame < total_intro_frames {
                let cycle = frame % cycle_len;
                if cycle < self.beep_frames {
                    (self.amplitude as f64 * self.phase.sin()) as f32
                } else {
                    0.0f32
                }
            } else {
                (self.amplitude as f64 * self.phase.sin()) as f32
            };
            for _ in 0..self.channels {
                data.push(sample);
            }
            self.phase += phase_step;
            if self.phase >= 2.0 * PI {
                self.phase -= 2.0 * PI;
            }
        }

        self.frames_produced += frames_to_generate;

        Ok(Some(AudioChunk::new(
            data,
            self.sample_rate,
            self.channels,
            chunk_time,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sine_amplitude_matches_dbfs() {
        // -6.0 dBFS: amplitude ≈ 0.501187
        let mut source_minus_6 = SineSource::new(1000.0, -6.0, 44100, 2).with_total_frames(44100);
        let chunk = source_minus_6.read_chunk(44100).unwrap().unwrap();
        let expected_amp = 10.0f32.powf(-6.0 / 20.0);
        let peak = chunk.data.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
        assert!(
            (peak - expected_amp).abs() < 0.005,
            "Peak amplitude {peak} should match expected {expected_amp}"
        );

        // -60.0 dBFS: amplitude = 0.001
        let mut source_minus_60 = SineSource::new(1000.0, -60.0, 44100, 2).with_total_frames(44100);
        let chunk60 = source_minus_60.read_chunk(44100).unwrap().unwrap();
        let expected_60 = 0.001f32;
        let peak60 = chunk60.data.iter().fold(0.0f32, |acc, &s| acc.max(s.abs()));
        assert!(
            (peak60 - expected_60).abs() < 0.0001,
            "Peak amplitude {peak60} should match expected {expected_60}"
        );
    }

    #[test]
    fn test_sine_frequency_via_zero_crossings() {
        let freq = 1000.0f32;
        let sample_rate = 44100u32;
        // 1 second of audio
        let mut source =
            SineSource::new(freq, -3.0, sample_rate, 1).with_total_frames(sample_rate as usize);
        let chunk = source.read_chunk(sample_rate as usize).unwrap().unwrap();

        // Count positive-going zero crossings (transitions from <= 0 to > 0)
        let mut zero_crossings = 0usize;
        let mut prev = chunk.data[0];
        for &curr in &chunk.data[1..] {
            if prev <= 0.0 && curr > 0.0 {
                zero_crossings += 1;
            }
            prev = curr;
        }

        // In 1.0 second of a 1000 Hz sine wave, there should be exactly 1000 cycles (±1 tolerance)
        assert!(
            (zero_crossings as i64 - 1000).abs() <= 1,
            "Expected ~1000 zero crossings for 1000 Hz, got {zero_crossings}"
        );
    }

    #[test]
    fn test_sine_with_beeps_generates_silence_gaps() {
        let sample_rate = 1000u32;
        // 1 beep: 100ms (100 samples) tone, 50ms (50 samples) silence, then continuous tone
        let mut source = SineSource::new(100.0, 0.0, sample_rate, 1)
            .with_beep_timing(1, Duration::from_millis(100), Duration::from_millis(50))
            .with_total_frames(250);

        let chunk = source.read_chunk(250).unwrap().unwrap();
        let samples = chunk.data;

        // Samples 0..100: non-zero tone
        let has_tone_1 = samples[0..100].iter().any(|&s| s.abs() > 0.01);
        assert!(has_tone_1);

        // Samples 100..150: silence
        let max_silence = samples[100..150]
            .iter()
            .fold(0.0f32, |acc, &s| acc.max(s.abs()));
        assert_eq!(max_silence, 0.0);

        // Samples 150..250: continuous tone
        let has_tone_2 = samples[150..250].iter().any(|&s| s.abs() > 0.01);
        assert!(has_tone_2);
    }
}
