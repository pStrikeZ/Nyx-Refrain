//! Timing port UDP responder and clock sync exchange.
//!
//! Receives timing sync requests (0x80 0xD2) from the receiver and returns
//! reference, receive, and send NTP timestamps (0x80 0xD3).
//!
//! # Protocol Specification Reference
//! - References: `references/PROTOCOL_SUMMARY.md` §5.4
//! - Upstream references:
//!   - `pyatv/protocols/raop/protocols/__init__.py:124-140` (timing request/response layout)
//!   - `libraop/src/raop_client.c:1308-1331` (timing responder implementation)
//!
//! // Timing sync requests (0x80 0xD2) are sent by receivers during SETUP and every ~2.5s; 32-byte 0x80 0xD3 response required.
//! // Timing responder needed before SETUP (otherwise error 500 after ~32 s).

use std::time::Duration;
use thiserror::Error;

/// Standard size of a RAOP timing packet (32 bytes).
pub const TIMING_PACKET_SIZE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimingRequest {
    pub seq: u16,
    pub remote_send_ntp: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimingResponse {
    pub seq: u16,
    pub reference_ntp: u64,
    pub receive_ntp: u64,
    pub send_ntp: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TimingError {
    #[error("packet too short: expected {expected} bytes, got {actual}")]
    PacketTooShort { expected: usize, actual: usize },

    #[error("invalid packet type: byte 0=0x{byte0:02x}, byte 1=0x{byte1:02x}")]
    InvalidPacketType { byte0: u8, byte1: u8 },
}

/// Parses an incoming 32-byte timing request (`0x80 0xD2`).
pub fn parse_timing_request(data: &[u8]) -> Result<TimingRequest, TimingError> {
    if data.len() < TIMING_PACKET_SIZE {
        return Err(TimingError::PacketTooShort {
            expected: TIMING_PACKET_SIZE,
            actual: data.len(),
        });
    }

    if data[0] != 0x80 || data[1] != 0xD2 {
        return Err(TimingError::InvalidPacketType {
            byte0: data[0],
            byte1: data[1],
        });
    }

    let seq = u16::from_be_bytes([data[2], data[3]]);
    let remote_send_ntp = u64::from_be_bytes([
        data[24], data[25], data[26], data[27], data[28], data[29], data[30], data[31],
    ]);

    Ok(TimingRequest {
        seq,
        remote_send_ntp,
    })
}

/// Builds a 32-byte timing response (`0x80 0xD3`).
///
/// # Field Layout (PROTOCOL_SUMMARY.md §5.4)
/// - Bytes 0..3: `0x80, 0xD3, seq[0], seq[1]`
/// - Bytes 4..7: `0x00, 0x00, 0x00, 0x00` (dummy padding)
/// - Bytes 8..15: `Reference timestamp` = echoed remote send timestamp (T1)
/// - Bytes 16..23: `Receive timestamp` T2 (sender NTP time on receipt)
/// - Bytes 24..31: `Send timestamp` T3 (sender NTP time on transmit)
pub fn build_timing_response(
    req: &TimingRequest,
    receive_ntp: u64,
    send_ntp: u64,
    out: &mut [u8; TIMING_PACKET_SIZE],
) {
    out[0] = 0x80;
    out[1] = 0xD3;
    out[2..4].copy_from_slice(&req.seq.to_be_bytes());
    out[4..8].fill(0);
    out[8..16].copy_from_slice(&req.remote_send_ntp.to_be_bytes());
    out[16..24].copy_from_slice(&receive_ntp.to_be_bytes());
    out[24..32].copy_from_slice(&send_ntp.to_be_bytes());
}

/// Helper to build an outgoing timing query (if sender probes receiver timing).
pub fn build_timing_request(seq: u16, send_ntp: u64, out: &mut [u8; TIMING_PACKET_SIZE]) {
    out[0] = 0x80;
    out[1] = 0xD2;
    out[2..4].copy_from_slice(&seq.to_be_bytes());
    out[4..24].fill(0);
    out[24..32].copy_from_slice(&send_ntp.to_be_bytes());
}

/// Helper to parse an incoming timing response (`0x80 0xD3`).
pub fn parse_timing_response(data: &[u8]) -> Result<TimingResponse, TimingError> {
    if data.len() < TIMING_PACKET_SIZE {
        return Err(TimingError::PacketTooShort {
            expected: TIMING_PACKET_SIZE,
            actual: data.len(),
        });
    }

    if data[0] != 0x80 || data[1] != 0xD3 {
        return Err(TimingError::InvalidPacketType {
            byte0: data[0],
            byte1: data[1],
        });
    }

    let seq = u16::from_be_bytes([data[2], data[3]]);
    let reference_ntp = u64::from_be_bytes([
        data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
    ]);
    let receive_ntp = u64::from_be_bytes([
        data[16], data[17], data[18], data[19], data[20], data[21], data[22], data[23],
    ]);
    let send_ntp = u64::from_be_bytes([
        data[24], data[25], data[26], data[27], data[28], data[29], data[30], data[31],
    ]);

    Ok(TimingResponse {
        seq,
        reference_ntp,
        receive_ntp,
        send_ntp,
    })
}

/// Calculates RTT between 4 NTP timestamps (T1, T2, T3, T4).
/// RTT = (T4 - T1) - (T3 - T2).
pub fn calculate_ntp_rtt(t1: u64, t2: u64, t3: u64, t4: u64) -> Option<Duration> {
    if t4 >= t1 && t3 >= t2 {
        let total_diff = t4 - t1;
        let remote_diff = t3 - t2;
        if total_diff >= remote_diff {
            let rtt_ntp = total_diff - remote_diff;
            let secs = rtt_ntp >> 32;
            let frac = (rtt_ntp & 0xFFFFFFFF) as f64 / 4294967296.0;
            let nanos = (frac * 1_000_000_000.0) as u32;
            return Some(Duration::new(secs, nanos));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timing_request_and_response_round_trip() {
        let mut req_buf = [0u8; TIMING_PACKET_SIZE];
        let t1 = 0x83AA7E80_12345678u64;
        build_timing_request(42, t1, &mut req_buf);

        let parsed_req = parse_timing_request(&req_buf).expect("parse timing req");
        assert_eq!(parsed_req.seq, 42);
        assert_eq!(parsed_req.remote_send_ntp, t1);

        let mut resp_buf = [0u8; TIMING_PACKET_SIZE];
        let t2 = 0x83AA7E80_23456789u64;
        let t3 = 0x83AA7E80_3456789Au64;
        build_timing_response(&parsed_req, t2, t3, &mut resp_buf);

        let parsed_resp = parse_timing_response(&resp_buf).expect("parse timing resp");
        assert_eq!(parsed_resp.seq, 42);
        assert_eq!(parsed_resp.reference_ntp, t1);
        assert_eq!(parsed_resp.receive_ntp, t2);
        assert_eq!(parsed_resp.send_ntp, t3);
    }

    #[test]
    fn test_rtt_calculation() {
        // T1 = 0.0s, T2 = 0.010s, T3 = 0.015s, T4 = 0.030s
        // total = 0.030s, remote = 0.005s -> RTT = 0.025s (25 ms)
        let ntp_unit = 4294967296.0;
        let t1 = 0u64;
        let t2 = (0.010 * ntp_unit) as u64;
        let t3 = (0.015 * ntp_unit) as u64;
        let t4 = (0.030 * ntp_unit) as u64;

        let rtt = calculate_ntp_rtt(t1, t2, t3, t4).expect("calc rtt");
        let millis = rtt.as_secs_f64() * 1000.0;
        assert!((millis - 25.0).abs() < 0.1);
    }

    #[test]
    fn test_timing_request_truncated_and_malformed() {
        // Truncated requests
        assert_eq!(
            parse_timing_request(&[]),
            Err(TimingError::PacketTooShort {
                expected: 32,
                actual: 0
            })
        );
        assert_eq!(
            parse_timing_request(&[0x80, 0xD2]),
            Err(TimingError::PacketTooShort {
                expected: 32,
                actual: 2
            })
        );
        assert_eq!(
            parse_timing_request(&[0u8; 31]),
            Err(TimingError::PacketTooShort {
                expected: 32,
                actual: 31
            })
        );

        // Invalid marker
        let mut bad_marker = [0u8; 32];
        bad_marker[0] = 0x80;
        bad_marker[1] = 0xD3; // 0xD3 is response, not request (0xD2)
        assert_eq!(
            parse_timing_request(&bad_marker),
            Err(TimingError::InvalidPacketType {
                byte0: 0x80,
                byte1: 0xD3
            })
        );
    }

    #[test]
    fn test_timing_response_truncated_and_malformed() {
        // Truncated responses
        assert_eq!(
            parse_timing_response(&[]),
            Err(TimingError::PacketTooShort {
                expected: 32,
                actual: 0
            })
        );
        assert_eq!(
            parse_timing_response(&[0u8; 31]),
            Err(TimingError::PacketTooShort {
                expected: 32,
                actual: 31
            })
        );

        // Invalid marker
        let mut bad_marker = [0u8; 32];
        bad_marker[0] = 0x80;
        bad_marker[1] = 0xD2; // 0xD2 is request, not response (0xD3)
        assert_eq!(
            parse_timing_response(&bad_marker),
            Err(TimingError::InvalidPacketType {
                byte0: 0x80,
                byte1: 0xD2
            })
        );
    }
}
