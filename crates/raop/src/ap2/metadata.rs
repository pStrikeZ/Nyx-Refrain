//! AirPlay 2 metadata (track info, artwork, progress) encoding and features.

use std::time::Duration;

/// Metadata for a playing track.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrackInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
}

impl TrackInfo {
    pub fn new(
        title: impl Into<String>,
        artist: impl Into<String>,
        album: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
        }
    }
}

/// AirPlay 2 device features advertised in mDNS TXT record (`features`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Ap2Features(pub u64);

impl Ap2Features {
    /// Bit 15: Supports artwork via SET_PARAMETER.
    pub const ARTWORK: u64 = 1 << 15;
    /// Bit 16: Supports progress via SET_PARAMETER.
    pub const PROGRESS: u64 = 1 << 16;
    /// Bit 17: Supports metadata / text via DAAP (application/x-dmap-tagged).
    pub const TEXT_DAAP: u64 = 1 << 17;
    /// Bit 50: Supports NowPlaying via binary plist.
    pub const NOW_PLAYING_BPLIST: u64 = 1 << 50;

    /// Parses the `features` TXT record value.
    /// Supports single hex strings (`0x4A7FCA00`) and comma-separated 32-bit words
    /// (`0x4A7FCA00,0x3C354BD0` = low, high words).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        if let Some((low_str, high_str)) = s.split_once(',') {
            let low = parse_hex_word(low_str)?;
            let high = parse_hex_word(high_str)?;
            Some(Self(((high as u64) << 32) | (low as u64)))
        } else {
            let clean = s.trim_start_matches("0x").trim_start_matches("0X");
            let val = u64::from_str_radix(clean, 16).ok()?;
            Some(Self(val))
        }
    }

    /// Whether bit 15 (artwork) is set.
    pub fn supports_artwork(self) -> bool {
        (self.0 & Self::ARTWORK) != 0
    }

    /// Whether bit 16 (progress) is set.
    pub fn supports_progress(self) -> bool {
        (self.0 & Self::PROGRESS) != 0
    }

    /// Whether bit 17 (text via DAAP) is set.
    pub fn supports_text(self) -> bool {
        (self.0 & Self::TEXT_DAAP) != 0
    }

    /// Whether bit 50 (bplist NowPlaying) is set.
    pub fn supports_now_playing_bplist(self) -> bool {
        (self.0 & Self::NOW_PLAYING_BPLIST) != 0
    }
}

fn parse_hex_word(s: &str) -> Option<u32> {
    let clean = s.trim().trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(clean, 16).ok()
}

/// Encodes `TrackInfo` into a DMAP container (`mlit`) holding `minm` (title),
/// `asar` (artist), and `asal` (album).
pub fn encode_dmap(info: &TrackInfo) -> Vec<u8> {
    let mut inner = Vec::new();
    fn push_item(buf: &mut Vec<u8>, tag: &[u8; 4], val: &str) {
        let bytes = val.as_bytes();
        buf.extend_from_slice(tag);
        buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        buf.extend_from_slice(bytes);
    }
    push_item(&mut inner, b"minm", &info.title);
    push_item(&mut inner, b"asar", &info.artist);
    push_item(&mut inner, b"asal", &info.album);

    let mut out = Vec::with_capacity(8 + inner.len());
    out.extend_from_slice(b"mlit");
    out.extend_from_slice(&(inner.len() as u32).to_be_bytes());
    out.extend_from_slice(&inner);
    out
}

/// Decodes a DMAP `mlit` container into `TrackInfo`.
pub fn decode_dmap(data: &[u8]) -> Result<TrackInfo, String> {
    if data.len() < 8 {
        return Err("data too short for DMAP container".into());
    }
    if &data[0..4] != b"mlit" {
        return Err(format!("expected mlit tag, got {:?}", &data[0..4]));
    }
    let total_len = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
    if data.len() < 8 + total_len {
        return Err("incomplete DMAP container".into());
    }
    let mut info = TrackInfo::default();
    let mut offset = 8;
    let end = 8 + total_len;
    while offset + 8 <= end {
        let tag = &data[offset..offset + 4];
        let item_len = u32::from_be_bytes([
            data[offset + 4],
            data[offset + 5],
            data[offset + 6],
            data[offset + 7],
        ]) as usize;
        offset += 8;
        if offset + item_len > end {
            return Err("truncated DMAP item".into());
        }
        let val_bytes = &data[offset..offset + item_len];
        let s = String::from_utf8_lossy(val_bytes).to_string();
        match tag {
            b"minm" => info.title = s,
            b"asar" => info.artist = s,
            b"asal" => info.album = s,
            _ => {}
        }
        offset += item_len;
    }
    Ok(info)
}

/// Maximum allowed artwork size: 1 MiB.
pub const MAX_ARTWORK_BYTES: usize = 1024 * 1024;

/// Detects JPEG or PNG by magic bytes. Returns MIME type if supported and <= 1 MiB.
pub fn sniff_image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() > MAX_ARTWORK_BYTES {
        return None;
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else {
        None
    }
}

/// Formats the progress parameter body:
/// `progress: <start>/<current>/<end>\r\n`
/// in 44.1 kHz RTP timestamps.
pub fn format_progress_body(current_rtp: u32, position: Duration, duration: Duration) -> String {
    let pos_frames =
        crate::clock::duration_to_frames(position, crate::clock::DEFAULT_SAMPLE_RATE) as u32;
    let dur_frames =
        crate::clock::duration_to_frames(duration, crate::clock::DEFAULT_SAMPLE_RATE) as u32;
    let start = current_rtp.wrapping_sub(pos_frames);
    let end = start.wrapping_add(dur_frames);
    format!("progress: {start}/{current_rtp}/{end}\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_features_parsing_single_and_pair() {
        // HomePod mini features from prompt: 0x4A7FCA00,0x3C354BD0
        let f = Ap2Features::parse("0x4A7FCA00,0x3C354BD0").expect("parse pair");
        assert!(f.supports_artwork(), "bit 15 should be set");
        assert!(f.supports_progress(), "bit 16 should be set");
        assert!(f.supports_text(), "bit 17 should be set");
        assert!(f.supports_now_playing_bplist(), "bit 50 should be set");

        // Without 0x prefix
        let f2 = Ap2Features::parse("4A7FCA00,3C354BD0").expect("parse without 0x");
        assert_eq!(f, f2);

        // Single word hex: only text (bit 17 = 0x20000)
        let f_single = Ap2Features::parse("0x20000").expect("parse single");
        assert!(f_single.supports_text());
        assert!(!f_single.supports_artwork());
        assert!(!f_single.supports_progress());

        // Empty / invalid strings
        assert_eq!(Ap2Features::parse(""), None);
        assert_eq!(Ap2Features::parse("not-hex"), None);
        assert_eq!(Ap2Features::parse("0x123,invalid"), None);
    }

    #[test]
    fn test_dmap_roundtrip() {
        let original = TrackInfo {
            title: "夜に駆ける (Racing into the night)".into(),
            artist: "YOASOBI".into(),
            album: "THE BOOK".into(),
        };

        let encoded = encode_dmap(&original);
        assert_eq!(&encoded[0..4], b"mlit");
        let decoded = decode_dmap(&encoded).expect("decode dmap");
        assert_eq!(original, decoded);

        // Empty fields
        let empty = TrackInfo::default();
        let enc_empty = encode_dmap(&empty);
        let dec_empty = decode_dmap(&enc_empty).expect("decode empty dmap");
        assert_eq!(empty, dec_empty);
    }

    #[test]
    fn test_dmap_malformed_rejection() {
        assert!(decode_dmap(&[]).is_err());
        assert!(decode_dmap(b"abcd\0\0\0\0").is_err());
        assert!(decode_dmap(b"mlit\0\0\0\x10minm").is_err());
    }

    #[test]
    fn test_image_sniffing() {
        let jpeg = [0xff, 0xd8, 0xff, 0xdb, 0x00];
        assert_eq!(sniff_image_type(&jpeg), Some("image/jpeg"));

        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        assert_eq!(sniff_image_type(&png), Some("image/png"));

        let bmp = [0x42, 0x4d];
        assert_eq!(sniff_image_type(&bmp), None);

        let oversized = vec![0xff; MAX_ARTWORK_BYTES + 1];
        assert_eq!(sniff_image_type(&oversized), None);
    }

    #[test]
    fn test_progress_body_formatting() {
        let current_rtp = 100_000;
        let pos = Duration::from_secs(10); // 441,000 frames
        let dur = Duration::from_secs(180); // 7,938,000 frames

        let body = format_progress_body(current_rtp, pos, dur);
        let start = current_rtp.wrapping_sub(441_000);
        let end = start.wrapping_add(7_938_000);
        assert_eq!(body, format!("progress: {start}/{current_rtp}/{end}\r\n"));
    }
}
