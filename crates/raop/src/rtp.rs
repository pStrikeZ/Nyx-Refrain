//! RTP packet framing for audio streaming.
//!
//! Writes and parses the 12-byte RTP audio header (PT=96).
//!
//! # Protocol Specification Reference
//! - References: `references/PROTOCOL_SUMMARY.md` §3.1, §6.2
//! - Upstream references:
//!   - `libraop/src/raop_client.c:563-570` (RTP header layout)
//!   - `pyatv/protocols/raop/packets.py:27-31` (header parsing)

use thiserror::Error;

/// Length of the fixed RTP header (12 bytes).
pub const RTP_HEADER_SIZE: usize = 12;

/// Dynamic audio payload type (96 / 0x60), also used by AirPlay 2 PCM.
pub const RTP_PAYLOAD_TYPE_ALAC: u8 = 96;

/// Marker bit flag in byte 1 (0x80).
pub const RTP_MARKER_BIT: u8 = 0x80;

/// Byte 1 value for the very first audio packet (0x80 | 96 = 0xE0).
pub const RTP_FIRST_PACKET_BYTE1: u8 = RTP_MARKER_BIT | RTP_PAYLOAD_TYPE_ALAC;

/// Byte 1 value for all subsequent audio packets (96 = 0x60).
pub const RTP_SUBSEQUENT_PACKET_BYTE1: u8 = RTP_PAYLOAD_TYPE_ALAC;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpHeader {
    pub marker: bool,
    pub payload_type: u8,
    pub seq: u16,
    pub timestamp: u32,
    pub ssrc: u32,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RtpError {
    #[error("packet too short: expected at least {expected} bytes, got {actual}")]
    PacketTooShort { expected: usize, actual: usize },

    #[error("unsupported RTP version: expected 2, got {0}")]
    UnsupportedVersion(u8),
}

/// Encodes an RTP header into the first 12 bytes of a buffer.
pub fn write_rtp_header(
    marker: bool,
    seq: u16,
    timestamp: u32,
    ssrc: u32,
    out: &mut [u8; RTP_HEADER_SIZE],
) {
    // Byte 0: V=2 (0b10), P=0, X=0, CC=0 -> 0x80
    out[0] = 0x80;
    // Byte 1: Marker bit (0x80) | PT (0x60)
    out[1] = if marker {
        RTP_FIRST_PACKET_BYTE1
    } else {
        RTP_SUBSEQUENT_PACKET_BYTE1
    };
    out[2..4].copy_from_slice(&seq.to_be_bytes());
    out[4..8].copy_from_slice(&timestamp.to_be_bytes());
    out[8..12].copy_from_slice(&ssrc.to_be_bytes());
}

/// Parses an RTP header from a byte slice.
pub fn parse_rtp_header(data: &[u8]) -> Result<RtpHeader, RtpError> {
    if data.len() < RTP_HEADER_SIZE {
        return Err(RtpError::PacketTooShort {
            expected: RTP_HEADER_SIZE,
            actual: data.len(),
        });
    }

    let version = (data[0] >> 6) & 0x03;
    if version != 2 {
        return Err(RtpError::UnsupportedVersion(version));
    }

    let marker = (data[1] & RTP_MARKER_BIT) != 0;
    let payload_type = data[1] & 0x7F;
    let seq = u16::from_be_bytes([data[2], data[3]]);
    let timestamp = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let ssrc = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);

    Ok(RtpHeader {
        marker,
        payload_type,
        seq,
        timestamp,
        ssrc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtp_header_encode_decode() {
        let mut hdr_bytes = [0u8; RTP_HEADER_SIZE];
        write_rtp_header(true, 1001, 44100, 0x12345678, &mut hdr_bytes);

        assert_eq!(hdr_bytes[0], 0x80);
        assert_eq!(hdr_bytes[1], 0xE0); // Marker bit set
        assert_eq!(&hdr_bytes[2..4], &1001u16.to_be_bytes());
        assert_eq!(&hdr_bytes[4..8], &44100u32.to_be_bytes());
        assert_eq!(&hdr_bytes[8..12], &0x12345678u32.to_be_bytes());

        let parsed = parse_rtp_header(&hdr_bytes).expect("parse header");
        assert!(parsed.marker);
        assert_eq!(parsed.payload_type, 96);
        assert_eq!(parsed.seq, 1001);
        assert_eq!(parsed.timestamp, 44100);
        assert_eq!(parsed.ssrc, 0x12345678);

        // Subsequent packet without marker
        write_rtp_header(false, 1002, 44452, 0x12345678, &mut hdr_bytes);
        assert_eq!(hdr_bytes[1], 0x60);
        let parsed2 = parse_rtp_header(&hdr_bytes).expect("parse subsequent");
        assert!(!parsed2.marker);
        assert_eq!(parsed2.seq, 1002);
    }
}
