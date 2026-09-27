//! HAP TLV8 encoding (values longer than 255 bytes are split into consecutive items of the
//! same type).
//!
//! Source: pyatv/auth/hap_tlv8.py write_tlv/read_tlv (MIT); HAP spec R2 §14.1.

use std::collections::BTreeMap;

/// TLV types used by pair-setup. Source: pyatv/auth/hap_tlv8.py TlvValue.
pub mod tag {
    pub const METHOD: u8 = 0x00;
    pub const SALT: u8 = 0x02;
    pub const PUBLIC_KEY: u8 = 0x03;
    pub const PROOF: u8 = 0x04;
    pub const SEQ_NO: u8 = 0x06;
    pub const ERROR: u8 = 0x07;
    pub const FLAGS: u8 = 0x13;
}

/// Pair-setup flag requesting transient pairing. Source: pyatv hap_tlv8.Flags.TransientPairing.
pub const FLAG_TRANSIENT: u8 = 0x10;

/// Encodes items in the given order.
pub fn write(items: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(t, value) in items {
        if value.is_empty() {
            out.extend_from_slice(&[t, 0]);
            continue;
        }
        for chunk in value.chunks(255) {
            out.push(t);
            out.push(chunk.len() as u8);
            out.extend_from_slice(chunk);
        }
    }
    out
}

/// Decodes TLV8, concatenating fragments of the same type that follow each other.
pub fn read(data: &[u8]) -> Result<BTreeMap<u8, Vec<u8>>, &'static str> {
    let mut out: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    let mut i = 0;
    let mut last: Option<u8> = None;
    while i < data.len() {
        if i + 2 > data.len() {
            return Err("truncated TLV header");
        }
        let (t, len) = (data[i], data[i + 1] as usize);
        let end = i + 2 + len;
        if end > data.len() {
            return Err("truncated TLV value");
        }
        let v = &data[i + 2..end];
        match (last, out.get_mut(&t)) {
            (Some(prev), Some(existing)) if prev == t => existing.extend_from_slice(v),
            _ => {
                out.insert(t, v.to_vec());
            }
        }
        last = Some(t);
        i = end;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn matches_pyatv_m1_and_m3() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/ap2_crypto_vectors.json"))
                .unwrap();
        let s: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/hap_srp_vector.json")).unwrap();
        let m1 = write(&[
            (tag::METHOD, &[0]),
            (tag::SEQ_NO, &[1]),
            (tag::FLAGS, &[FLAG_TRANSIENT]),
        ]);
        assert_eq!(m1, unhex(v["tlv_m1"].as_str().unwrap()));
        let a = unhex(s["A"].as_str().unwrap());
        let proof = unhex(s["M1"].as_str().unwrap());
        let m3 = write(&[
            (tag::SEQ_NO, &[3]),
            (tag::PUBLIC_KEY, &a),
            (tag::PROOF, &proof),
        ]);
        assert_eq!(m3, unhex(v["tlv_m3"].as_str().unwrap()));
        let back = read(&m3).unwrap();
        assert_eq!(back[&tag::PUBLIC_KEY], a);
        assert_eq!(back[&tag::PROOF], proof);
    }

    #[test]
    fn rejects_truncated() {
        assert!(read(&[0x03, 0x05, 1, 2]).is_err());
        assert!(read(&[0x03]).is_err());
    }
}
