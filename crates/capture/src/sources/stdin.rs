//! Stdin/byte-stream audio source reading raw PCM (s16le or f32le).

use crate::{AudioChunk, AudioSource};
use std::io::{Read, Stdin, stdin};
use std::time::{Duration, Instant};

/// Raw PCM sample format for stream input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdinFormat {
    /// 16-bit signed integer, little endian.
    S16Le,
    /// 32-bit IEEE float, little endian.
    F32Le,
}

/// Audio source that streams raw PCM data from standard input or any readable stream.
pub struct StdinSource<R: Read + Send> {
    reader: R,
    format: StdinFormat,
    sample_rate: u32,
    channels: u16,
    frames_produced: usize,
    start_time: Instant,
}

impl StdinSource<Stdin> {
    /// Create a new source reading directly from standard input.
    pub fn from_stdin(format: StdinFormat, sample_rate: u32, channels: u16) -> Self {
        Self::new(stdin(), format, sample_rate, channels)
    }
}

impl<R: Read + Send> StdinSource<R> {
    /// Create a new StdinSource wrapping any `Read` stream.
    pub fn new(reader: R, format: StdinFormat, sample_rate: u32, channels: u16) -> Self {
        Self {
            reader,
            format,
            sample_rate,
            channels,
            frames_produced: 0,
            start_time: Instant::now(),
        }
    }
}

impl<R: Read + Send> AudioSource for StdinSource<R> {
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

        let bytes_per_sample = match self.format {
            StdinFormat::S16Le => 2,
            StdinFormat::F32Le => 4,
        };

        let frame_size = bytes_per_sample * self.channels as usize;
        let mut byte_buffer = vec![0u8; max_frames * frame_size];

        let mut total_bytes_read = 0;
        while total_bytes_read < byte_buffer.len() {
            match self.reader.read(&mut byte_buffer[total_bytes_read..]) {
                Ok(0) => break, // EOF
                Ok(n) => total_bytes_read += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Box::new(e)),
            }
        }

        if total_bytes_read == 0 {
            return Ok(None);
        }

        // Align to full frames
        let complete_frames = total_bytes_read / frame_size;
        if complete_frames == 0 {
            return Ok(None);
        }

        let mut data = Vec::with_capacity(complete_frames * self.channels as usize);

        match self.format {
            StdinFormat::S16Le => {
                for chunk in byte_buffer[..complete_frames * frame_size].chunks_exact(2) {
                    let s = i16::from_le_bytes([chunk[0], chunk[1]]);
                    data.push(s as f32 / 32768.0);
                }
            }
            StdinFormat::F32Le => {
                for chunk in byte_buffer[..complete_frames * frame_size].chunks_exact(4) {
                    let s = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    data.push(s);
                }
            }
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
    use std::io::Cursor;

    #[test]
    fn test_stdin_source_s16le() {
        let mut raw = Vec::new();
        // 2 frames of stereo: [1000, -1000], [2000, -2000]
        raw.extend_from_slice(&1000i16.to_le_bytes());
        raw.extend_from_slice(&(-1000i16).to_le_bytes());
        raw.extend_from_slice(&2000i16.to_le_bytes());
        raw.extend_from_slice(&(-2000i16).to_le_bytes());

        let mut source = StdinSource::new(Cursor::new(raw), StdinFormat::S16Le, 44100, 2);
        let chunk = source.read_chunk(10).unwrap().unwrap();
        assert_eq!(chunk.frame_count(), 2);
        assert_eq!(chunk.data.len(), 4);
        assert!((chunk.data[0] - (1000.0 / 32768.0)).abs() < 1e-6);
        assert!((chunk.data[1] - (-1000.0 / 32768.0)).abs() < 1e-6);
        assert!((chunk.data[2] - (2000.0 / 32768.0)).abs() < 1e-6);
        assert!((chunk.data[3] - (-2000.0 / 32768.0)).abs() < 1e-6);

        assert!(source.read_chunk(10).unwrap().is_none());
    }

    #[test]
    fn test_stdin_source_f32le() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&0.25f32.to_le_bytes());
        raw.extend_from_slice(&(-0.5f32).to_le_bytes());

        let mut source = StdinSource::new(Cursor::new(raw), StdinFormat::F32Le, 44100, 1);
        let chunk = source.read_chunk(10).unwrap().unwrap();
        assert_eq!(chunk.frame_count(), 2);
        assert_eq!(chunk.data[0], 0.25);
        assert_eq!(chunk.data[1], -0.5);

        assert!(source.read_chunk(10).unwrap().is_none());
    }
}
