//! RTSP response parsing shared by the AirPlay 2 control and pairing channels.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RtspError {
    #[error("malformed RTSP response: {0}")]
    Malformed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtspResponse {
    pub status_code: u16,
    pub reason_phrase: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RtspResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn is_success(&self) -> bool {
        self.status_code >= 200 && self.status_code < 300
    }

    pub fn is_busy(&self) -> bool {
        self.status_code == 453
    }
}

/// Parses an RTSP response from a buffer.
///
/// Returns `Ok(Some((response, consumed_bytes)))` on complete message.
/// Returns `Ok(None)` if the message is incomplete and more data is needed.
/// Returns `Err(RtspError)` on malformed protocol data (never panics).
pub fn parse_rtsp_response(buf: &[u8]) -> Result<Option<(RtspResponse, usize)>, RtspError> {
    // Find double CRLF separating headers from body
    let header_end_pos = match buf.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(pos) => pos,
        None => return Ok(None), // Headers incomplete
    };

    let header_bytes = &buf[..header_end_pos];
    let header_str = std::str::from_utf8(header_bytes)
        .map_err(|e| RtspError::Malformed(format!("invalid UTF-8 in headers: {}", e)))?;

    let mut lines = header_str.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| RtspError::Malformed("empty response".to_string()))?;

    // Example status line: "RTSP/1.0 200 OK"
    let mut parts = status_line.splitn(3, ' ');
    let version = parts
        .next()
        .ok_or_else(|| RtspError::Malformed("missing version in status line".to_string()))?;
    if !version.starts_with("RTSP/1.") && !version.starts_with("HTTP/1.") {
        return Err(RtspError::Malformed(format!(
            "unexpected protocol version: {}",
            version
        )));
    }

    let code_str = parts
        .next()
        .ok_or_else(|| RtspError::Malformed("missing status code".to_string()))?;
    let status_code: u16 = code_str
        .parse()
        .map_err(|_| RtspError::Malformed(format!("invalid status code: {}", code_str)))?;

    let reason_phrase = parts.next().unwrap_or("").to_string();

    let mut headers = Vec::new();
    let mut content_length: usize = 0;

    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, val)) = line.split_once(':') {
            let key = name.trim().to_string();
            let value = val.trim().to_string();
            if key.eq_ignore_ascii_case("Content-Length") {
                content_length = value.parse().map_err(|_| {
                    RtspError::Malformed(format!("invalid Content-Length: {}", value))
                })?;
            }
            headers.push((key, value));
        }
    }

    let body_start = header_end_pos + 4;
    // Hard limit on body size to prevent memory exhaustion DoS (16 MiB max).
    const MAX_RTSP_BODY_SIZE: usize = 16 * 1024 * 1024;
    if content_length > MAX_RTSP_BODY_SIZE {
        return Err(RtspError::Malformed(format!(
            "Content-Length ({}) exceeds maximum allowed body size ({})",
            content_length, MAX_RTSP_BODY_SIZE
        )));
    }

    let total_required = body_start
        .checked_add(content_length)
        .ok_or_else(|| RtspError::Malformed("Content-Length integer overflow".to_string()))?;

    if buf.len() < total_required {
        return Ok(None); // Body incomplete
    }

    let body = buf[body_start..total_required].to_vec();

    Ok(Some((
        RtspResponse {
            status_code,
            reason_phrase,
            headers,
            body,
        },
        total_required,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_rtsp_response_complete_and_body() {
        let raw = b"RTSP/1.0 200 OK\r\n\
                    CSeq: 1\r\n\
                    Content-Type: text/parameters\r\n\
                    Content-Length: 16\r\n\
                    \r\n\
                    volume: -144.0\r\n";

        let (resp, consumed) = parse_rtsp_response(raw).unwrap().expect("parsed");
        assert_eq!(consumed, raw.len());
        assert_eq!(resp.status_code, 200);
        assert_eq!(resp.reason_phrase, "OK");
        assert_eq!(resp.header("cseq"), Some("1"));
        assert_eq!(resp.header("content-type"), Some("text/parameters"));
        assert_eq!(resp.body, b"volume: -144.0\r\n");
    }

    #[test]
    fn test_parse_rtsp_response_incomplete() {
        // Headers incomplete
        let partial1 = b"RTSP/1.0 200 OK\r\nCSeq: 1";
        assert_eq!(parse_rtsp_response(partial1).unwrap(), None);

        // Headers complete but body incomplete
        let partial2 = b"RTSP/1.0 200 OK\r\nContent-Length: 10\r\n\r\n12345";
        assert_eq!(parse_rtsp_response(partial2).unwrap(), None);
    }

    #[test]
    fn test_parse_rtsp_response_malformed() {
        let bad = b"SIP/2.0 200 OK\r\n\r\n";
        assert!(parse_rtsp_response(bad).is_err());

        let bad_code = b"RTSP/1.0 ABC OK\r\n\r\n";
        assert!(parse_rtsp_response(bad_code).is_err());
    }

    #[test]
    fn test_busy_status_code_detection() {
        let raw = b"RTSP/1.0 453 Not Enough Bandwidth\r\n\
                    CSeq: 2\r\n\
                    \r\n";
        let (resp, _) = parse_rtsp_response(raw).unwrap().unwrap();
        assert!(resp.is_busy());
        assert_eq!(resp.status_code, 453);
    }

    #[test]
    fn test_parse_rtsp_response_malformed_details() {
        // Invalid UTF-8 in header
        let bad_utf8 = b"RTSP/1.0 200 OK\r\nBadHeader: \xFF\xFE\r\n\r\n";
        assert!(matches!(
            parse_rtsp_response(bad_utf8),
            Err(RtspError::Malformed(_))
        ));

        // Invalid Content-Length format
        let bad_cl = b"RTSP/1.0 200 OK\r\nContent-Length: notanumber\r\n\r\n";
        assert!(matches!(
            parse_rtsp_response(bad_cl),
            Err(RtspError::Malformed(_))
        ));

        // Missing status code
        let no_code = b"RTSP/1.0\r\n\r\n";
        assert!(matches!(
            parse_rtsp_response(no_code),
            Err(RtspError::Malformed(_))
        ));

        // Wrong protocol prefix
        let sip = b"SIP/2.0 200 OK\r\n\r\n";
        assert!(matches!(
            parse_rtsp_response(sip),
            Err(RtspError::Malformed(_))
        ));
    }

    #[test]
    fn test_parse_rtsp_response_content_length_overflow_regression() {
        // Discovered via cargo-fuzz (crash-ab811ebc405488a274454e846d37169e70544914)
        let malformed = b"RTSP/1.0 200 OK\r\nContent-Length: 18446744073709551615\r\n\r\n";
        let res = parse_rtsp_response(malformed);
        assert!(matches!(res, Err(RtspError::Malformed(_))));

        let huge = b"RTSP/1.0 200 OK\r\nContent-Length: 20000000\r\n\r\n";
        let res2 = parse_rtsp_response(huge);
        assert!(matches!(res2, Err(RtspError::Malformed(_))));
    }
}
