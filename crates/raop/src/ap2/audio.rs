//! AirPlay 2 realtime audio packet encryption.
//!
//! Packet = RTP header (12) || ChaCha20-Poly1305(shk, nonce, aad = header[4..12])(payload)
//! || tag (16) || nonce counter (8, little endian). Nonce = 4 zero bytes || u64 LE counter,
//! incremented per packet. Source: pyatv/protocols/raop/protocols/airplayv2.py
//! send_audio_packet + support/chacha20.py Chacha20Cipher8byteNonce (MIT).
//! RT-safe: encrypts in place into a caller-provided buffer, no allocation.

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

use super::hap::nonce_for;

pub const AUDIO_TRAILER_LEN: usize = 16 + 8;

pub struct AudioCipher {
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl AudioCipher {
    pub fn new(shk: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(shk)),
            counter: 0,
        }
    }

    /// Builds the encrypted packet in `out` and returns its length.
    /// `out` must hold at least `12 + payload.len() + AUDIO_TRAILER_LEN` bytes.
    pub fn seal(&mut self, rtp_header: &[u8; 12], payload: &[u8], out: &mut [u8]) -> usize {
        let body_end = 12 + payload.len();
        let total = body_end + AUDIO_TRAILER_LEN;
        assert!(out.len() >= total, "audio packet buffer too small");
        out[..12].copy_from_slice(rtp_header);
        out[12..body_end].copy_from_slice(payload);
        let nonce = nonce_for(self.counter);
        let tag = self
            .cipher
            .encrypt_in_place_detached(
                Nonce::from_slice(&nonce),
                &rtp_header[4..12],
                &mut out[12..body_end],
            )
            .expect("chacha20poly1305 encryption cannot fail for audio-sized payloads");
        out[body_end..body_end + 16].copy_from_slice(&tag);
        out[body_end + 16..total].copy_from_slice(&nonce[4..]);
        self.counter += 1;
        total
    }
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
    fn matches_pyatv_packets() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/ap2_crypto_vectors.json"))
                .unwrap();
        let shk: [u8; 32] = unhex(v["audio_shk"].as_str().unwrap()).try_into().unwrap();
        let hdr: [u8; 12] = unhex(v["audio_hdr"].as_str().unwrap()).try_into().unwrap();
        let payload = unhex(v["audio_payload"].as_str().unwrap());
        let mut c = AudioCipher::new(&shk);
        let mut buf = [0u8; 2048];
        for expected in v["audio_packets"].as_array().unwrap() {
            let n = c.seal(&hdr, &payload, &mut buf);
            assert_eq!(buf[..n].to_vec(), unhex(expected.as_str().unwrap()));
        }
    }
}
