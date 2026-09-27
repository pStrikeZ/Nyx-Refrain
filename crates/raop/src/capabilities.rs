//! Device capabilities reading and TXT record parsing.
//!
//! Parses `_raop._tcp` and `_airplay._tcp` TXT records into structured [`DeviceCapabilities`].
//! Handles UTF-8 device name normalization.
//!
//! # Upstream & Specification References
//! - `references/PROTOCOL_SUMMARY.md` §1
//! - Apple AirPlay TXT record format:
//!   - `cn`: Audio codecs (0=PCM, 1=ALAC, 2=AAC, 3=AAC-ELD)
//!   - `et`: Encryption types (0=None, 1=RSA, 3=FairPlay, 4=MFiSAP, 5=FairPlay2.5)
//!   - `sr`: Sample rate (e.g. 44100)
//!   - `ss`: Sample size (e.g. 16)
//!   - `ch`: Channels (e.g. 2)
//!   - `tp`: Transport protocol (e.g. "UDP", "TCP")
//!   - `md`: Metadata types (0=text, 1=artwork, 2=progress)
//!   - `vn`: Version number (e.g. 65537)
//!   - `am`: Device model (e.g. "AudioAccessory5,1")
//!   - `pk`: Public key hex (e.g. 32-byte Ed25519 pairing key)
//!   - `da`: Direct audio bool
//!   - `sf`: Status flags (bitfield)
//!   - `vs`: Server version string
//!   - `ov`: OS version string
//!   - `vv`: Protocol version integer

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use unicode_normalization::UnicodeNormalization;

/// Structured representation of RAOP TXT record capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DeviceCapabilities {
    /// Supported audio codecs (`cn`): 0=PCM, 1=ALAC, 2=AAC, 3=AAC-ELD.
    pub codecs: Vec<u8>,
    /// Supported encryption types (`et`): 0=None, 1=RSA, 3=FairPlay, etc.
    pub encryption_types: Vec<u8>,
    /// Sample rate (`sr`), typically 44100.
    pub sample_rate: Option<u32>,
    /// Sample size (`ss`), typically 16.
    pub sample_size: Option<u16>,
    /// Number of audio channels (`ch`), typically 2.
    pub channels: Option<u8>,
    /// Transport protocol (`tp`), typically "UDP".
    pub transport: Option<String>,
    /// Metadata types (`md`): 0=text, 1=artwork, 2=progress.
    pub metadata_types: Vec<u8>,
    /// Device version number (`vn`), e.g. 65537.
    pub version_number: Option<u32>,
    /// Device model (`am`), e.g. "AudioAccessory5,1" (HomePod) or "ShairportSync".
    pub model: Option<String>,
    /// Device public key (`pk`) hex string.
    pub public_key: Option<String>,
    /// Direct audio (`da`).
    pub direct_audio: Option<bool>,
    /// Status flags (`sf`).
    pub status_flags: Option<u64>,
    /// Version string (`vs`).
    pub version_str: Option<String>,
    /// OS version (`ov`).
    pub os_version: Option<String>,
    /// Protocol version (`vv`).
    pub protocol_version: Option<u32>,
    /// Any unparsed or additional raw TXT entries.
    pub raw_entries: HashMap<String, String>,
}

impl DeviceCapabilities {
    /// Parses TXT record entries provided as strings (e.g. `["cn=0,1,2,3", "et=0,3,5"]`).
    pub fn from_txt_strings<'a, I>(records: I) -> Self
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut caps = Self::default();

        for record in records {
            let record = record.trim();
            if record.is_empty() {
                continue;
            }

            let (key, val) = match record.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => (record, ""),
            };

            caps.apply_entry(key, val);
        }

        caps
    }

    /// Parses raw DNS-SD length-prefixed TXT record bytes.
    ///
    /// Each entry in DNS TXT RDATA consists of a 1-byte length prefix followed by string data.
    pub fn from_dns_txt_bytes(bytes: &[u8]) -> Self {
        let mut caps = Self::default();
        let mut offset = 0;

        while offset < bytes.len() {
            let len = bytes[offset] as usize;
            offset += 1;
            if offset + len > bytes.len() {
                break;
            }

            let entry_bytes = &bytes[offset..offset + len];
            offset += len;

            if let Ok(entry_str) = std::str::from_utf8(entry_bytes) {
                let entry_str = entry_str.trim();
                if entry_str.is_empty() {
                    continue;
                }
                let (key, val) = match entry_str.split_once('=') {
                    Some((k, v)) => (k.trim(), v.trim()),
                    None => (entry_str, ""),
                };
                caps.apply_entry(key, val);
            }
        }

        caps
    }

    fn apply_entry(&mut self, key: &str, val: &str) {
        self.raw_entries.insert(key.to_string(), val.to_string());

        match key {
            "cn" => {
                self.codecs = parse_comma_u8_list(val);
            }
            "et" => {
                self.encryption_types = parse_comma_u8_list(val);
            }
            "sr" => {
                self.sample_rate = val.parse().ok();
            }
            "ss" => {
                self.sample_size = val.parse().ok();
            }
            "ch" => {
                self.channels = val.parse().ok();
            }
            "tp" => {
                self.transport = Some(val.to_string());
            }
            "md" => {
                self.metadata_types = parse_comma_u8_list(val);
            }
            "vn" => {
                self.version_number = val.parse().ok();
            }
            "am" => {
                self.model = Some(val.to_string());
            }
            "pk" => {
                self.public_key = Some(val.to_string());
            }
            "da" => {
                self.direct_audio = match val {
                    "true" | "1" => Some(true),
                    "false" | "0" => Some(false),
                    _ => None,
                };
            }
            "sf" => {
                self.status_flags = parse_int_hex_or_dec(val);
            }
            "vs" => {
                self.version_str = Some(val.to_string());
            }
            "ov" => {
                self.os_version = Some(val.to_string());
            }
            "vv" => {
                self.protocol_version = val.parse().ok();
            }
            _ => {}
        }
    }
}

/// Helper to parse comma-separated u8 lists (e.g. "0,1,2,3" -> vec![0, 1, 2, 3]).
fn parse_comma_u8_list(val: &str) -> Vec<u8> {
    val.split(',')
        .filter_map(|s| s.trim().parse::<u8>().ok())
        .collect()
}

/// Helper to parse decimal or hex string (e.g. "0x98404" or "623620").
fn parse_int_hex_or_dec(val: &str) -> Option<u64> {
    let s = val.trim();
    if let Some(hex_str) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex_str, 16).ok()
    } else {
        s.parse::<u64>().ok()
    }
}

/// Extracts and NFC-normalizes a device name from a full RAOP/AirPlay mDNS service instance string.
///
/// Handles:
/// - `<MAC>@<Name>._raop._tcp.local`
/// - `<MAC>@<Name>._raop._tcp.local.`
/// - `<Name>._airplay._tcp.local`
/// - Standalone device names with full UTF-8 support (e.g. "客厅", "東京", emojis, accents).
///
/// Output is guaranteed to be in Unicode Normalization Form C (NFC).
pub fn extract_and_normalize_device_name(instance: &str) -> String {
    let clean = instance.trim_end_matches('.');
    let prefix = if let Some((before_service, _)) = clean.split_once("._raop.") {
        before_service
    } else if let Some((before_service, _)) = clean.split_once("._airplay.") {
        before_service
    } else {
        clean
    };

    let raw_name = if let Some((_mac, name)) = prefix.split_once('@') {
        name
    } else {
        prefix
    };

    // Unicode NFC normalization
    raw_name.nfc().collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_txt_parsing() {
        // TXT record sample advertised by an AirPlay receiver
        let txt = [
            "cn=0,1,2,3",
            "da=true",
            "et=0,3,5",
            "ft=0x4A7FCA00,0x3C354BD0",
            "sf=0x98404",
            "md=0,1,2",
            "am=AudioAccessory5,1",
            "pk=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "tp=UDP",
            "vn=65537",
            "vs=980.77.2",
            "ov=27.0",
            "vv=1",
        ];

        let caps = DeviceCapabilities::from_txt_strings(txt);

        assert_eq!(caps.codecs, vec![0, 1, 2, 3]);
        assert_eq!(caps.encryption_types, vec![0, 3, 5]);
        assert_eq!(caps.direct_audio, Some(true));
        assert_eq!(caps.status_flags, Some(0x98404));
        assert_eq!(caps.metadata_types, vec![0, 1, 2]);
        assert_eq!(caps.model.as_deref(), Some("AudioAccessory5,1"));
        assert_eq!(
            caps.public_key.as_deref(),
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
        );
        assert_eq!(caps.transport.as_deref(), Some("UDP"));
        assert_eq!(caps.version_number, Some(65537));
        assert_eq!(caps.version_str.as_deref(), Some("980.77.2"));
        assert_eq!(caps.os_version.as_deref(), Some("27.0"));
        assert_eq!(caps.protocol_version, Some(1));
    }

    #[test]
    fn test_dns_txt_bytes_parsing() {
        // DNS length-prefixed format: [len, text..., len, text...]
        let mut buf = Vec::new();
        let records = ["cn=0,1", "et=0", "am=TestModel"];
        for r in records {
            buf.push(r.len() as u8);
            buf.extend_from_slice(r.as_bytes());
        }

        let caps = DeviceCapabilities::from_dns_txt_bytes(&buf);
        assert_eq!(caps.codecs, vec![0, 1]);
        assert_eq!(caps.encryption_types, vec![0]);
        assert_eq!(caps.model.as_deref(), Some("TestModel"));
    }

    #[test]
    fn test_utf8_device_name_extraction_and_normalization() {
        // 客厅 (Chinese characters)
        assert_eq!(
            extract_and_normalize_device_name("AABBCCDDEEFF@客厅._raop._tcp.local."),
            "客厅"
        );
        assert_eq!(
            extract_and_normalize_device_name("AABBCCDDEEFF@客厅._raop._tcp.local"),
            "客厅"
        );
        assert_eq!(
            extract_and_normalize_device_name("客厅._airplay._tcp.local"),
            "客厅"
        );

        // NFD vs NFC decomposition: 'é' can be \u{00E9} (NFC) or 'e' + \u{0301} (NFD)
        let nfd_name = "AABBCCDDEEFF@Cafe\u{0301}._raop._tcp.local";
        let nfc_expected = "Caf\u{00E9}";
        assert_eq!(extract_and_normalize_device_name(nfd_name), nfc_expected);

        // Japanese kanji & kana
        assert_eq!(
            extract_and_normalize_device_name("AABBCCDDEEFF@東京・リビング._raop._tcp.local."),
            "東京・リビング"
        );

        // Name with spaces and emoji
        assert_eq!(
            extract_and_normalize_device_name("112233445566@Living Room 🎵._raop._tcp.local"),
            "Living Room 🎵"
        );

        // Plain string without service suffix or MAC
        assert_eq!(extract_and_normalize_device_name("Bed Room"), "Bed Room");
    }

    #[test]
    fn test_malformed_and_truncated_txt_records() {
        // Empty strings, records without values, entries with extra spaces
        let raw = [
            "",
            "   ",
            "ch=2",
            "ss=16",
            "sr=44100",
            "invalid_key_no_equal",
            "cn=bad,1,abc,2",
        ];
        let caps = DeviceCapabilities::from_txt_strings(raw);

        assert_eq!(caps.channels, Some(2));
        assert_eq!(caps.sample_size, Some(16));
        assert_eq!(caps.sample_rate, Some(44100));
        assert_eq!(caps.codecs, vec![1, 2]); // only valid u8s parsed
        assert!(caps.raw_entries.contains_key("invalid_key_no_equal"));

        // Truncated DNS bytes (length exceeds buffer)
        let truncated_dns_bytes = [10, b'a', b'b', b'c']; // claims 10 bytes, only 3 provided
        let caps_trunc = DeviceCapabilities::from_dns_txt_bytes(&truncated_dns_bytes);
        assert!(caps_trunc.raw_entries.is_empty());
    }
}
