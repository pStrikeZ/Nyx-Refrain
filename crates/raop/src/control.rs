//! Control port handling (sync packets and retransmission).
//!
//! Sends periodic sync packets matching stream timeline and responds to
//! retransmission requests from the receiver.
//!
//! # Protocol Specification Reference
//! - References: `references/PROTOCOL_SUMMARY.md` §5.1, §5.2, §5.3
//! - Upstream references:
//!   - `shairport-sync/rtp.c:415-440` (sync packet flag 0x0007 vs 0x0004)
//!   - `pyatv/protocols/raop/packets.py:18-24, 33-35` (sync & retransmit packet definitions)
//!   - `libraop/src/raop_client.c:1225-1267, 1386-1434` (sync generator & retransmit handler)

use thiserror::Error;

/// Size of an AirPlay sync packet (20 bytes).
pub const SYNC_PACKET_SIZE: usize = 20;

/// Size of an AirPlay retransmit request packet (8 bytes).
pub const RETRANSMIT_REQ_SIZE: usize = 8;

/// Configurable sequence / flag field for sync packets (bytes 2..3).
///
/// // Note: receiver sync flag handling may vary.
/// Defaults to iTunes (0x0007) per pyatv/libraop convention, with AirPlay (0x0004) configurable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncFlag {
    /// 0x0007: iTunes flag (signals receiver to add default legacy buffering offset).
    #[default]
    ITunes = 0x0007,
    /// 0x0004: AirPlay flag (signals receiver not to add extra legacy delay).
    AirPlay = 0x0004,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncPacket {
    pub is_first: bool,
    pub sync_flag: u16,
    pub dac_rtp_timestamp: u32,
    pub ntp_timestamp: u64,
    pub head_rtp_timestamp: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetransmitRequest {
    pub seq: u16,
    pub lost_seqno: u16,
    pub lost_packets: u16,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ControlError {
    #[error("packet too short: expected at least {expected} bytes, got {actual}")]
    PacketTooShort { expected: usize, actual: usize },

    #[error("unexpected packet type or marker: byte 0=0x{byte0:02x}, byte 1=0x{byte1:02x}")]
    InvalidPacketType { byte0: u8, byte1: u8 },
}

/// Builds a 20-byte sync packet into `out`.
///
/// # Field Layout (PROTOCOL_SUMMARY.md §5.1)
/// - Byte 0: `0x90` on the very first sync packet (extension header flag set); `0x80` subsequently.
/// - Byte 1: `0xD4` (PT=84 with Marker bit set).
/// - Bytes 2..3: `sync_flag` (0x0007 or 0x0004, big-endian).
/// - Bytes 4..7: `rtp_now - latency` (playback timestamp at speaker DAC at NTP time `now`).
/// - Bytes 8..15: 64-bit NTP timestamp (32-bit seconds, 32-bit fraction).
/// - Bytes 16..19: `rtp_now` (current stream head RTP timestamp).
pub fn build_sync_packet(
    is_first: bool,
    sync_flag: SyncFlag,
    rtp_now: u32,
    latency_frames: u32,
    ntp_ts: u64,
    out: &mut [u8; SYNC_PACKET_SIZE],
) {
    out[0] = if is_first { 0x90 } else { 0x80 };
    out[1] = 0xD4;
    out[2..4].copy_from_slice(&(sync_flag as u16).to_be_bytes());

    let dac_ts = rtp_now.wrapping_sub(latency_frames);
    out[4..8].copy_from_slice(&dac_ts.to_be_bytes());
    out[8..16].copy_from_slice(&ntp_ts.to_be_bytes());
    out[16..20].copy_from_slice(&rtp_now.to_be_bytes());
}

/// Parses a 20-byte sync packet.
pub fn parse_sync_packet(data: &[u8]) -> Result<SyncPacket, ControlError> {
    if data.len() < SYNC_PACKET_SIZE {
        return Err(ControlError::PacketTooShort {
            expected: SYNC_PACKET_SIZE,
            actual: data.len(),
        });
    }

    let byte0 = data[0];
    let byte1 = data[1];
    let is_first = match (byte0, byte1) {
        (0x90, 0xD4) => true,
        (0x80, 0xD4) => false,
        _ => return Err(ControlError::InvalidPacketType { byte0, byte1 }),
    };

    let sync_flag = u16::from_be_bytes([data[2], data[3]]);
    let dac_rtp_timestamp = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let ntp_timestamp = u64::from_be_bytes([
        data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
    ]);
    let head_rtp_timestamp = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);

    Ok(SyncPacket {
        is_first,
        sync_flag,
        dac_rtp_timestamp,
        ntp_timestamp,
        head_rtp_timestamp,
    })
}

/// Parses an 8-byte retransmit request from the receiver (`0x80 0xD5`).
///
/// # Field Layout (PROTOCOL_SUMMARY.md §5.2)
/// - Byte 0: `0x80`
/// - Byte 1: `0xD5` (PT=85 with Marker bit set)
/// - Bytes 2..3: Request sequence number
/// - Bytes 4..5: `lost_seqno` (first lost packet sequence number)
/// - Bytes 6..7: `lost_packets` (count of consecutive lost packets)
pub fn parse_retransmit_request(data: &[u8]) -> Result<RetransmitRequest, ControlError> {
    if data.len() < RETRANSMIT_REQ_SIZE {
        return Err(ControlError::PacketTooShort {
            expected: RETRANSMIT_REQ_SIZE,
            actual: data.len(),
        });
    }

    if data[0] != 0x80 || data[1] != 0xD5 {
        return Err(ControlError::InvalidPacketType {
            byte0: data[0],
            byte1: data[1],
        });
    }

    let seq = u16::from_be_bytes([data[2], data[3]]);
    let lost_seqno = u16::from_be_bytes([data[4], data[5]]);
    let lost_packets = u16::from_be_bytes([data[6], data[7]]);

    Ok(RetransmitRequest {
        seq,
        lost_seqno,
        lost_packets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_packet_first_and_subsequent() {
        let mut buf = [0u8; SYNC_PACKET_SIZE];
        let ntp = 0x83AA7E80_00000000u64;

        // First packet
        build_sync_packet(true, SyncFlag::ITunes, 44100, 11025, ntp, &mut buf);
        assert_eq!(buf[0], 0x90);
        assert_eq!(buf[1], 0xD4);
        assert_eq!(&buf[2..4], &0x0007u16.to_be_bytes());

        let parsed = parse_sync_packet(&buf).expect("parse sync 1");
        assert!(parsed.is_first);
        assert_eq!(parsed.sync_flag, 0x0007);
        assert_eq!(parsed.dac_rtp_timestamp, 44100 - 11025);
        assert_eq!(parsed.ntp_timestamp, ntp);
        assert_eq!(parsed.head_rtp_timestamp, 44100);

        // Subsequent packet with AirPlay flag
        build_sync_packet(
            false,
            SyncFlag::AirPlay,
            88200,
            11025,
            ntp + (1 << 32),
            &mut buf,
        );
        assert_eq!(buf[0], 0x80);
        assert_eq!(buf[1], 0xD4);
        assert_eq!(&buf[2..4], &0x0004u16.to_be_bytes());

        let parsed2 = parse_sync_packet(&buf).expect("parse sync 2");
        assert!(!parsed2.is_first);
        assert_eq!(parsed2.sync_flag, 0x0004);
        assert_eq!(parsed2.dac_rtp_timestamp, 88200 - 11025);
    }

    #[test]
    fn test_retransmit_request() {
        let raw_req = [0x80, 0xD5, 0x00, 0x01, 0x04, 0xD2, 0x00, 0x03];
        let req = parse_retransmit_request(&raw_req).expect("parse retransmit req");
        assert_eq!(req.seq, 1);
        assert_eq!(req.lost_seqno, 1234);
        assert_eq!(req.lost_packets, 3);
    }

    #[test]
    fn test_sync_packet_truncated_and_malformed() {
        // Truncated sync packets
        assert_eq!(
            parse_sync_packet(&[]),
            Err(ControlError::PacketTooShort {
                expected: 20,
                actual: 0
            })
        );
        assert_eq!(
            parse_sync_packet(&[0x80, 0xD4, 0x00]),
            Err(ControlError::PacketTooShort {
                expected: 20,
                actual: 3
            })
        );
        assert_eq!(
            parse_sync_packet(&[0u8; 19]),
            Err(ControlError::PacketTooShort {
                expected: 20,
                actual: 19
            })
        );

        // Invalid marker / header bytes
        let mut bad_marker = [0u8; 20];
        bad_marker[0] = 0x80;
        bad_marker[1] = 0x00; // Expected 0xD4
        assert_eq!(
            parse_sync_packet(&bad_marker),
            Err(ControlError::InvalidPacketType {
                byte0: 0x80,
                byte1: 0x00
            })
        );

        bad_marker[0] = 0x70; // Expected 0x80 or 0x90
        bad_marker[1] = 0xD4;
        assert_eq!(
            parse_sync_packet(&bad_marker),
            Err(ControlError::InvalidPacketType {
                byte0: 0x70,
                byte1: 0xD4
            })
        );
    }

    #[test]
    fn test_retransmit_request_truncated_and_malformed() {
        // Truncated retransmit request
        assert_eq!(
            parse_retransmit_request(&[]),
            Err(ControlError::PacketTooShort {
                expected: 8,
                actual: 0
            })
        );
        assert_eq!(
            parse_retransmit_request(&[0x80, 0xD5, 0x00]),
            Err(ControlError::PacketTooShort {
                expected: 8,
                actual: 3
            })
        );

        // Invalid marker
        let bad_req = [0x80, 0x00, 0x00, 0x01, 0x04, 0xD2, 0x00, 0x03];
        assert_eq!(
            parse_retransmit_request(&bad_req),
            Err(ControlError::InvalidPacketType {
                byte0: 0x80,
                byte1: 0x00
            })
        );

        let bad_ver = [0x90, 0xD5, 0x00, 0x01, 0x04, 0xD2, 0x00, 0x03];
        assert_eq!(
            parse_retransmit_request(&bad_ver),
            Err(ControlError::InvalidPacketType {
                byte0: 0x90,
                byte1: 0xD5
            })
        );
    }
}
