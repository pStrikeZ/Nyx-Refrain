//! HKDF key derivation and the HAP encrypted byte-stream framing used for the AirPlay 2
//! RTSP control connection (and the events channel).
//!
//! Framing (source: pyatv/auth/hap_session.py HAPSession, MIT; HAP spec R2 §6.5.2):
//! plaintext is split into frames of at most 1024 bytes; each frame is sent as
//! `len (u16 LE) || ChaCha20-Poly1305(key, nonce, aad = len)(frame) || tag(16)`.
//! Nonce = 4 zero bytes || u64 LE counter, one counter per direction starting at 0
//! (pyatv/support/chacha20.py Chacha20Cipher with nonce_length=8).

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};
use hkdf::Hkdf;
use sha2::Sha512;

pub const CONTROL_SALT: &str = "Control-Salt";
pub const CONTROL_WRITE_INFO: &str = "Control-Write-Encryption-Key";
pub const CONTROL_READ_INFO: &str = "Control-Read-Encryption-Key";
pub const EVENTS_SALT: &str = "Events-Salt";
pub const EVENTS_WRITE_INFO: &str = "Events-Write-Encryption-Key";
pub const EVENTS_READ_INFO: &str = "Events-Read-Encryption-Key";

const FRAME_LEN: usize = 1024;
const TAG_LEN: usize = 16;

/// HKDF-SHA512 with a 32-byte output. Source: pyatv/auth/hap_srp.py hkdf_expand.
pub fn hkdf_expand(salt: &str, info: &str, secret: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha512>::new(Some(salt.as_bytes()), secret);
    let mut okm = [0u8; 32];
    hk.expand(info.as_bytes(), &mut okm)
        .expect("32 bytes is a valid HKDF-SHA512 length");
    okm
}

/// 12-byte nonce: 4 zero bytes followed by the little-endian 64-bit counter.
pub fn nonce_for(counter: u64) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_le_bytes());
    n
}

#[derive(Debug, thiserror::Error)]
pub enum HapError {
    #[error("HAP frame authentication failed")]
    Decrypt,
}

/// Bidirectional HAP framing state for one TCP connection.
pub struct HapSession {
    out: ChaCha20Poly1305,
    inp: ChaCha20Poly1305,
    out_ctr: u64,
    in_ctr: u64,
    pending: Vec<u8>,
}

impl HapSession {
    pub fn new(write_key: &[u8; 32], read_key: &[u8; 32]) -> Self {
        Self {
            out: ChaCha20Poly1305::new(Key::from_slice(write_key)),
            inp: ChaCha20Poly1305::new(Key::from_slice(read_key)),
            out_ctr: 0,
            in_ctr: 0,
            pending: Vec::new(),
        }
    }

    /// Encrypts plaintext into one or more frames.
    pub fn encrypt(&mut self, plain: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(plain.len() + (plain.len() / FRAME_LEN + 1) * 18);
        for frame in plain.chunks(FRAME_LEN) {
            let len = (frame.len() as u16).to_le_bytes();
            let mut buf = frame.to_vec();
            let tag = self
                .out
                .encrypt_in_place_detached(
                    Nonce::from_slice(&nonce_for(self.out_ctr)),
                    &len,
                    &mut buf,
                )
                .expect("chacha20poly1305 encryption cannot fail for <= 1024 bytes");
            self.out_ctr += 1;
            out.extend_from_slice(&len);
            out.extend_from_slice(&buf);
            out.extend_from_slice(&tag);
        }
        out
    }

    /// Feeds received ciphertext; returns all plaintext of the frames completed so far.
    pub fn decrypt(&mut self, data: &[u8]) -> Result<Vec<u8>, HapError> {
        self.pending.extend_from_slice(data);
        let mut plain = Vec::new();
        loop {
            if self.pending.len() < 2 {
                break;
            }
            let len = u16::from_le_bytes([self.pending[0], self.pending[1]]) as usize;
            if self.pending.len() < 2 + len + TAG_LEN {
                break;
            }
            let aad = [self.pending[0], self.pending[1]];
            let mut buf = self.pending[2..2 + len].to_vec();
            let tag = Tag::clone_from_slice(&self.pending[2 + len..2 + len + TAG_LEN]);
            self.inp
                .decrypt_in_place_detached(
                    Nonce::from_slice(&nonce_for(self.in_ctr)),
                    &aad,
                    &mut buf,
                    &tag,
                )
                .map_err(|_| HapError::Decrypt)?;
            self.in_ctr += 1;
            plain.extend_from_slice(&buf);
            self.pending.drain(..2 + len + TAG_LEN);
        }
        Ok(plain)
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

    fn vectors() -> (serde_json::Value, Vec<u8>) {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/ap2_crypto_vectors.json"))
                .unwrap();
        let s: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/hap_srp_vector.json")).unwrap();
        let k = unhex(s["K"].as_str().unwrap());
        (v, k)
    }

    #[test]
    fn hkdf_matches_pyatv() {
        let (v, k) = vectors();
        assert_eq!(
            hkdf_expand(CONTROL_SALT, CONTROL_WRITE_INFO, &k).to_vec(),
            unhex(v["control_write"].as_str().unwrap())
        );
        assert_eq!(
            hkdf_expand(CONTROL_SALT, CONTROL_READ_INFO, &k).to_vec(),
            unhex(v["control_read"].as_str().unwrap())
        );
        assert_eq!(
            hkdf_expand(EVENTS_SALT, EVENTS_WRITE_INFO, &k).to_vec(),
            unhex(v["events_write"].as_str().unwrap())
        );
        assert_eq!(
            hkdf_expand(EVENTS_SALT, EVENTS_READ_INFO, &k).to_vec(),
            unhex(v["events_read"].as_str().unwrap())
        );
    }

    #[test]
    fn framing_matches_pyatv_and_round_trips() {
        let (v, k) = vectors();
        let w = hkdf_expand(CONTROL_SALT, CONTROL_WRITE_INFO, &k);
        let r = hkdf_expand(CONTROL_SALT, CONTROL_READ_INFO, &k);
        let mut tx = HapSession::new(&w, &r);
        let p1 = unhex(v["hap_plain1"].as_str().unwrap());
        let p2 = unhex(v["hap_plain2"].as_str().unwrap());
        let c1 = tx.encrypt(&p1);
        let c2 = tx.encrypt(&p2);
        assert_eq!(c1, unhex(v["hap_cipher1"].as_str().unwrap()));
        assert_eq!(c2, unhex(v["hap_cipher2"].as_str().unwrap()));
        // The peer decrypts with the keys swapped; feed byte-by-byte to test reassembly.
        let mut rx = HapSession::new(&r, &w);
        let mut got = Vec::new();
        for b in c1.iter().chain(c2.iter()) {
            got.extend(rx.decrypt(std::slice::from_ref(b)).unwrap());
        }
        assert_eq!(got, [p1, p2].concat());
    }

    #[test]
    fn tampered_frame_is_rejected() {
        let mut tx = HapSession::new(&[1; 32], &[2; 32]);
        let mut c = tx.encrypt(b"hello");
        c[3] ^= 1;
        let mut rx = HapSession::new(&[2; 32], &[1; 32]);
        assert!(rx.decrypt(&c).is_err());
    }
}
