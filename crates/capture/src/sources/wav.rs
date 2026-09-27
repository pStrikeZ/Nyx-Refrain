//! WAV audio file source supporting 16-bit PCM and 32-bit float audio.

use crate::{AudioChunk, AudioSource};
use hound::{SampleFormat, WavIntoSamples, WavReader};
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;
use std::time::{Duration, Instant};

enum SampleStream<R: Read + Seek> {
    Int16(WavIntoSamples<R, i16>),
    Float32(WavIntoSamples<R, f32>),
}

/// Convenience alias for a WavSource reading from a filesystem file.
pub type FileWavSource = WavSource<BufReader<File>>;

/// Audio source reading uncompressed WAV streams (s16 or f32).
pub struct WavSource<R: Read + Seek + Send> {
    stream: SampleStream<R>,
    sample_rate: u32,
    channels: u16,
    frames_produced: usize,
    start_time: Instant,
}

impl WavSource<BufReader<File>> {
    /// Open a WAV file from the filesystem.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        Self::from_reader(reader)
    }
}

impl<R: Read + Seek + Send> WavSource<R> {
    /// Create a WavSource from any readable and seekable stream.
    pub fn from_reader(reader: R) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let wav_reader = WavReader::new(reader)?;
        let spec = wav_reader.spec();
        let sample_rate = spec.sample_rate;
        let channels = spec.channels;

        let stream = match (spec.sample_format, spec.bits_per_sample) {
            (SampleFormat::Int, 16) => SampleStream::Int16(wav_reader.into_samples::<i16>()),
            (SampleFormat::Float, 32) => SampleStream::Float32(wav_reader.into_samples::<f32>()),
            (fmt, bits) => {
                return Err(format!(
                    "Unsupported WAV format: format={fmt:?}, bits={bits} (only s16 and f32 supported)"
                )
                .into());
            }
        };

        Ok(Self {
            stream,
            sample_rate,
            channels,
            frames_produced: 0,
            start_time: Instant::now(),
        })
    }
}

impl<R: Read + Seek + Send> AudioSource for WavSource<R> {
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
        if max_frames == 0 || self.channels == 0 {
            return Ok(Some(AudioChunk::new(
                Vec::new(),
                self.sample_rate,
                self.channels,
                Instant::now(),
            )));
        }

        let total_samples = max_frames * self.channels as usize;
        let mut data = Vec::with_capacity(total_samples);

        match &mut self.stream {
            SampleStream::Int16(iter) => {
                for _ in 0..total_samples {
                    match iter.next() {
                        Some(Ok(s)) => data.push(s as f32 / 32768.0),
                        Some(Err(e)) => return Err(Box::new(e)),
                        None => break,
                    }
                }
            }
            SampleStream::Float32(iter) => {
                for _ in 0..total_samples {
                    match iter.next() {
                        Some(Ok(s)) => data.push(s),
                        Some(Err(e)) => return Err(Box::new(e)),
                        None => break,
                    }
                }
            }
        }

        if data.is_empty() {
            return Ok(None);
        }

        // Align to whole frames
        let complete_frames = data.len() / self.channels as usize;
        data.truncate(complete_frames * self.channels as usize);

        if complete_frames == 0 {
            return Ok(None);
        }

        let chunk_time = {
            let elapsed = self.frames_produced as u64;
            let secs = elapsed / self.sample_rate as u64;
            let rem = elapsed % self.sample_rate as u64;
            let nanos = (rem * 1_000_000_000) / self.sample_rate as u64;
            self.start_time + Duration::new(secs, nanos as u32)
        };

        self.frames_produced += complete_frames;

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
    use hound::{WavSpec, WavWriter};
    use std::io::Cursor;

    #[test]
    fn test_wav_source_s16_round_trip() {
        let spec = WavSpec {
            channels: 2,
            sample_rate: 44100,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        };

        let mut buf = Vec::new();
        {
            let mut writer = WavWriter::new(Cursor::new(&mut buf), spec).unwrap();
            // Write 100 stereo frames (200 samples)
            for i in 0..100 {
                let l = (i * 200) as i16;
                let r = -(i * 200) as i16;
                writer.write_sample(l).unwrap();
                writer.write_sample(r).unwrap();
            }
            writer.finalize().unwrap();
        }

        let mut source = WavSource::from_reader(Cursor::new(buf)).unwrap();
        assert_eq!(source.sample_rate(), 44100);
        assert_eq!(source.channels(), 2);

        let chunk = source.read_chunk(100).unwrap().unwrap();
        assert_eq!(chunk.frame_count(), 100);
        assert_eq!(chunk.data.len(), 200);

        for i in 0..100 {
            let expected_l = (i * 200) as f32 / 32768.0;
            let expected_r = -((i * 200) as i16) as f32 / 32768.0;
            assert!((chunk.data[i * 2] - expected_l).abs() < 1e-6);
            assert!((chunk.data[i * 2 + 1] - expected_r).abs() < 1e-6);
        }

        assert!(source.read_chunk(100).unwrap().is_none());
    }

    #[test]
    fn test_wav_source_f32_round_trip() {
        let spec = WavSpec {
            channels: 1,
            sample_rate: 48000,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };

        let mut buf = Vec::new();
        {
            let mut writer = WavWriter::new(Cursor::new(&mut buf), spec).unwrap();
            for i in 0..50 {
                let val = (i as f32) / 50.0;
                writer.write_sample(val).unwrap();
            }
            writer.finalize().unwrap();
        }

        let mut source = WavSource::from_reader(Cursor::new(buf)).unwrap();
        assert_eq!(source.sample_rate(), 48000);
        assert_eq!(source.channels(), 1);

        let chunk = source.read_chunk(50).unwrap().unwrap();
        assert_eq!(chunk.frame_count(), 50);
        for i in 0..50 {
            let expected = (i as f32) / 50.0;
            assert!((chunk.data[i] - expected).abs() < 1e-6);
        }

        assert!(source.read_chunk(50).unwrap().is_none());
    }
}
