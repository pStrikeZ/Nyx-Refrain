//! Silence audio source producing digital silence (0.0f32).

use crate::{AudioChunk, AudioSource};
use std::time::{Duration, Instant};

/// Audio source that generates digital silence.
pub struct SilenceSource {
    sample_rate: u32,
    channels: u16,
    total_frames: Option<usize>,
    frames_produced: usize,
    start_time: Instant,
}

impl SilenceSource {
    /// Create an infinite silence source.
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        Self {
            sample_rate,
            channels,
            total_frames: None,
            frames_produced: 0,
            start_time: Instant::now(),
        }
    }

    /// Create a silence source with fixed duration.
    pub fn with_duration(sample_rate: u32, channels: u16, duration: Duration) -> Self {
        let total_frames = ((duration.as_secs_f64()) * (sample_rate as f64)).round() as usize;
        Self {
            sample_rate,
            channels,
            total_frames: Some(total_frames),
            frames_produced: 0,
            start_time: Instant::now(),
        }
    }

    /// Create a silence source with fixed total frame count.
    pub fn with_total_frames(sample_rate: u32, channels: u16, total_frames: usize) -> Self {
        Self {
            sample_rate,
            channels,
            total_frames: Some(total_frames),
            frames_produced: 0,
            start_time: Instant::now(),
        }
    }
}

impl AudioSource for SilenceSource {
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

        let sample_count = frames_to_generate * self.channels as usize;
        let data = vec![0.0f32; sample_count];
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
    fn test_silence_source_fixed_duration() {
        let mut source = SilenceSource::with_total_frames(44100, 2, 882);
        let chunk = source.read_chunk(500).unwrap().unwrap();
        assert_eq!(chunk.frame_count(), 500);
        assert_eq!(chunk.data.len(), 1000);
        assert!(chunk.data.iter().all(|&s| s == 0.0));

        let chunk2 = source.read_chunk(500).unwrap().unwrap();
        assert_eq!(chunk2.frame_count(), 382);
        assert_eq!(chunk2.data.len(), 764);

        let eof = source.read_chunk(500).unwrap();
        assert!(eof.is_none());
    }
}
