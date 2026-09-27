//! Audio format parsing and sample conversion for WASAPI mix formats.
//!
//! Handles standard Windows formats: Float32, PCM16, PCM24 (packed or in 32-bit
//! containers), PCM32, and WAVEFORMATEXTENSIBLE with KSDATAFORMAT subtypes,
//! converting them to interleaved IEEE `f32` samples.

use thiserror::Error;

/// Format parsing and conversion errors.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FormatError {
    #[error("Unsupported audio format tag: 0x{0:04x}")]
    UnsupportedFormatTag(u16),

    #[error("Unsupported subformat GUID: {0:?}")]
    UnsupportedSubFormat([u8; 16]),

    #[error("Unsupported bit depth: {bits} bits per sample (container: {container})")]
    UnsupportedBitDepth { bits: u16, container: u16 },

    #[error("Buffer too short: expected at least {expected} bytes, got {actual}")]
    BufferTooShort { expected: usize, actual: usize },

    #[error("Invalid channel count: {0}")]
    InvalidChannelCount(u16),

    #[error("Invalid sample rate: {0}")]
    InvalidSampleRate(u32),
}

/// Supported sample formats encountered in WASAPI mix formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// 32-bit IEEE floating-point.
    Float32,
    /// 16-bit signed integer PCM.
    Pcm16,
    /// 24-bit signed PCM packed into 3 bytes.
    Pcm24Packed,
    /// 24-bit signed PCM left-aligned in a 32-bit container (standard WASAPI).
    Pcm24In32,
    /// 32-bit signed integer PCM.
    Pcm32,
}

/// Parsed metadata describing a Windows audio format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveFormatInfo {
    /// Sampling rate in Hz (e.g. 48000, 44100).
    pub sample_rate: u32,
    /// Number of audio channels (e.g. 2 for stereo).
    pub channels: u16,
    /// Container bits per sample (e.g. 16, 24, 32).
    pub bits_per_sample: u16,
    /// Valid audio bits within the container (e.g. 24 valid bits in 32-bit container).
    pub valid_bits_per_sample: u16,
    /// Concrete sample format representation.
    pub sample_format: SampleFormat,
    /// Channel mask (e.g. SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT = 0x3).
    pub channel_mask: u32,
    /// Number of bytes per frame (`channels * (bits_per_sample / 8)`).
    pub bytes_per_frame: usize,
}

// Standard Windows format tags
pub const WAVE_FORMAT_PCM: u16 = 1;
pub const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
pub const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

// Standard KSDATAFORMAT subtype GUIDs (little-endian byte order as in memory)
// KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: 00000003-0000-0010-8000-00AA00389B71
pub const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: [u8; 16] = [
    0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];

// KSDATAFORMAT_SUBTYPE_PCM: 00000001-0000-0010-8000-00AA00389B71
pub const KSDATAFORMAT_SUBTYPE_PCM: [u8; 16] = [
    0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71,
];

impl WaveFormatInfo {
    /// Parse a Windows `WAVEFORMATEX` or `WAVEFORMATEXTENSIBLE` structure.
    pub fn parse(
        w_format_tag: u16,
        n_channels: u16,
        n_samples_per_sec: u32,
        w_bits_per_sample: u16,
        cb_size: u16,
        extra_bytes: &[u8],
    ) -> Result<Self, FormatError> {
        if n_channels == 0 {
            return Err(FormatError::InvalidChannelCount(n_channels));
        }
        if n_samples_per_sec == 0 {
            return Err(FormatError::InvalidSampleRate(n_samples_per_sec));
        }

        let (sample_format, valid_bits, channel_mask) = match w_format_tag {
            WAVE_FORMAT_IEEE_FLOAT => {
                if w_bits_per_sample != 32 {
                    return Err(FormatError::UnsupportedBitDepth {
                        bits: w_bits_per_sample,
                        container: w_bits_per_sample,
                    });
                }
                (SampleFormat::Float32, 32, 0)
            }
            WAVE_FORMAT_PCM => match w_bits_per_sample {
                16 => (SampleFormat::Pcm16, 16, 0),
                24 => (SampleFormat::Pcm24Packed, 24, 0),
                32 => (SampleFormat::Pcm32, 32, 0),
                other => {
                    return Err(FormatError::UnsupportedBitDepth {
                        bits: other,
                        container: other,
                    });
                }
            },
            WAVE_FORMAT_EXTENSIBLE => {
                if cb_size < 22 || extra_bytes.len() < 22 {
                    return Err(FormatError::BufferTooShort {
                        expected: 22,
                        actual: extra_bytes.len(),
                    });
                }
                let valid_bits = u16::from_le_bytes([extra_bytes[0], extra_bytes[1]]);
                let dw_channel_mask = u32::from_le_bytes([
                    extra_bytes[2],
                    extra_bytes[3],
                    extra_bytes[4],
                    extra_bytes[5],
                ]);
                let mut subformat = [0u8; 16];
                subformat.copy_from_slice(&extra_bytes[6..22]);

                let effective_valid_bits = if valid_bits == 0 {
                    w_bits_per_sample
                } else {
                    valid_bits
                };

                let sample_fmt = if subformat == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                    if w_bits_per_sample != 32 {
                        return Err(FormatError::UnsupportedBitDepth {
                            bits: effective_valid_bits,
                            container: w_bits_per_sample,
                        });
                    }
                    SampleFormat::Float32
                } else if subformat == KSDATAFORMAT_SUBTYPE_PCM {
                    match (w_bits_per_sample, effective_valid_bits) {
                        (16, 16) => SampleFormat::Pcm16,
                        (24, 24) => SampleFormat::Pcm24Packed,
                        (32, 24) => SampleFormat::Pcm24In32,
                        (32, 32) => SampleFormat::Pcm32,
                        (container, bits) => {
                            return Err(FormatError::UnsupportedBitDepth { bits, container });
                        }
                    }
                } else {
                    return Err(FormatError::UnsupportedSubFormat(subformat));
                };

                (sample_fmt, effective_valid_bits, dw_channel_mask)
            }
            other => return Err(FormatError::UnsupportedFormatTag(other)),
        };

        let bytes_per_frame = (n_channels as usize) * ((w_bits_per_sample as usize) / 8);

        Ok(Self {
            sample_rate: n_samples_per_sec,
            channels: n_channels,
            bits_per_sample: w_bits_per_sample,
            valid_bits_per_sample: valid_bits,
            sample_format,
            channel_mask,
            bytes_per_frame,
        })
    }

    /// Convert a raw byte buffer captured from WASAPI into interleaved IEEE `f32` samples.
    ///
    /// If `is_silent` is true (e.g. `AUDCLNT_BUFFERFLAGS_SILENT` was set),
    /// writes `0.0f32` for all requested frames.
    pub fn convert_to_interleaved_f32(
        &self,
        raw_bytes: &[u8],
        num_frames: usize,
        is_silent: bool,
        out: &mut Vec<f32>,
    ) -> Result<(), FormatError> {
        let total_samples = num_frames * (self.channels as usize);
        out.clear();
        out.reserve(total_samples);

        if is_silent {
            out.resize(total_samples, 0.0);
            return Ok(());
        }

        let expected_bytes = num_frames * self.bytes_per_frame;
        if raw_bytes.len() < expected_bytes {
            return Err(FormatError::BufferTooShort {
                expected: expected_bytes,
                actual: raw_bytes.len(),
            });
        }

        match self.sample_format {
            SampleFormat::Float32 => {
                for chunk in raw_bytes[..expected_bytes].chunks_exact(4) {
                    let s = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    out.push(s);
                }
            }
            SampleFormat::Pcm16 => {
                for chunk in raw_bytes[..expected_bytes].chunks_exact(2) {
                    let s = i16::from_le_bytes([chunk[0], chunk[1]]);
                    out.push(s as f32 / 32768.0);
                }
            }
            SampleFormat::Pcm24Packed => {
                for chunk in raw_bytes[..expected_bytes].chunks_exact(3) {
                    // Sign extend 24-bit integer
                    let s = ((chunk[2] as i8 as i32) << 16)
                        | ((chunk[1] as i32) << 8)
                        | (chunk[0] as i32);
                    out.push(s as f32 / 8388608.0);
                }
            }
            SampleFormat::Pcm24In32 => {
                // In standard WASAPI, 24 valid bits in 32-bit container are left-aligned
                // (bits 8..31 contain data, bits 0..7 contain dither/zero padding).
                // Dividing the full 32-bit signed integer by 2^31 normalizes to [-1.0, 1.0].
                for chunk in raw_bytes[..expected_bytes].chunks_exact(4) {
                    let s = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    out.push(s as f32 / 2147483648.0);
                }
            }
            SampleFormat::Pcm32 => {
                for chunk in raw_bytes[..expected_bytes].chunks_exact(4) {
                    let s = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    out.push(s as f32 / 2147483648.0);
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_wav_format_ieee_float() {
        let fmt = WaveFormatInfo::parse(WAVE_FORMAT_IEEE_FLOAT, 2, 48000, 32, 0, &[]).unwrap();
        assert_eq!(fmt.sample_rate, 48000);
        assert_eq!(fmt.channels, 2);
        assert_eq!(fmt.sample_format, SampleFormat::Float32);
        assert_eq!(fmt.bytes_per_frame, 8);
    }

    #[test]
    fn test_parse_wav_format_pcm16() {
        let fmt = WaveFormatInfo::parse(WAVE_FORMAT_PCM, 2, 44100, 16, 0, &[]).unwrap();
        assert_eq!(fmt.sample_rate, 44100);
        assert_eq!(fmt.channels, 2);
        assert_eq!(fmt.sample_format, SampleFormat::Pcm16);
        assert_eq!(fmt.bytes_per_frame, 4);
    }

    #[test]
    fn test_parse_waveformat_extensible_float() {
        let mut extra = vec![0u8; 22];
        // valid bits = 32
        extra[0] = 32;
        extra[1] = 0;
        // channel mask = 3 (stereo)
        extra[2] = 3;
        // SubFormat = KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        extra[6..22].copy_from_slice(&KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);

        let fmt = WaveFormatInfo::parse(WAVE_FORMAT_EXTENSIBLE, 2, 48000, 32, 22, &extra).unwrap();
        assert_eq!(fmt.sample_format, SampleFormat::Float32);
        assert_eq!(fmt.channel_mask, 3);
        assert_eq!(fmt.valid_bits_per_sample, 32);
    }

    #[test]
    fn test_parse_waveformat_extensible_pcm24_in_32() {
        let mut extra = vec![0u8; 22];
        // valid bits = 24
        extra[0] = 24;
        extra[1] = 0;
        // SubFormat = KSDATAFORMAT_SUBTYPE_PCM
        extra[6..22].copy_from_slice(&KSDATAFORMAT_SUBTYPE_PCM);

        let fmt = WaveFormatInfo::parse(WAVE_FORMAT_EXTENSIBLE, 2, 96000, 32, 22, &extra).unwrap();
        assert_eq!(fmt.sample_format, SampleFormat::Pcm24In32);
        assert_eq!(fmt.valid_bits_per_sample, 24);
        assert_eq!(fmt.bytes_per_frame, 8);
    }

    #[test]
    fn test_convert_float32() {
        let fmt = WaveFormatInfo::parse(WAVE_FORMAT_IEEE_FLOAT, 2, 48000, 32, 0, &[]).unwrap();
        let samples = [0.5f32, -0.5f32, 1.0f32, 0.0f32];
        let mut raw = Vec::new();
        for &s in &samples {
            raw.extend_from_slice(&s.to_le_bytes());
        }

        let mut out = Vec::new();
        fmt.convert_to_interleaved_f32(&raw, 2, false, &mut out)
            .unwrap();
        assert_eq!(out, samples);
    }

    #[test]
    fn test_convert_pcm16() {
        let fmt = WaveFormatInfo::parse(WAVE_FORMAT_PCM, 2, 44100, 16, 0, &[]).unwrap();
        let pcm_samples: [i16; 4] = [0, 32767, -32768, 16384];
        let mut raw = Vec::new();
        for &s in &pcm_samples {
            raw.extend_from_slice(&s.to_le_bytes());
        }

        let mut out = Vec::new();
        fmt.convert_to_interleaved_f32(&raw, 2, false, &mut out)
            .unwrap();
        assert_eq!(out[0], 0.0);
        assert!((out[1] - 0.9999695).abs() < 1e-4);
        assert_eq!(out[2], -1.0);
        assert!((out[3] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn test_convert_pcm24_packed() {
        let fmt = WaveFormatInfo::parse(WAVE_FORMAT_PCM, 1, 48000, 24, 0, &[]).unwrap();
        // 0, max 24-bit (0x7FFFFF = 8388607), min 24-bit (0x800000 = -8388608)
        let raw = [
            0x00, 0x00, 0x00, // 0
            0xFF, 0xFF, 0x7F, // +8388607
            0x00, 0x00, 0x80, // -8388608
        ];

        let mut out = Vec::new();
        fmt.convert_to_interleaved_f32(&raw, 3, false, &mut out)
            .unwrap();
        assert_eq!(out[0], 0.0);
        assert!((out[1] - 1.0).abs() < 1e-5);
        assert_eq!(out[2], -1.0);
    }

    #[test]
    fn test_convert_silent_override() {
        let fmt = WaveFormatInfo::parse(WAVE_FORMAT_IEEE_FLOAT, 2, 48000, 32, 0, &[]).unwrap();
        let raw = vec![0xFFu8; 16]; // non-zero noise
        let mut out = Vec::new();
        fmt.convert_to_interleaved_f32(&raw, 2, true, &mut out)
            .unwrap();
        assert_eq!(out, vec![0.0, 0.0, 0.0, 0.0]);
    }
}
