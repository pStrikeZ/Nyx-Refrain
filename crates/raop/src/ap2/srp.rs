//! HAP SRP-6a client (3072-bit group, SHA-512), byte-compatible with srptools as used by
//! pyatv for AirPlay 2 transient pairing.
//!
//! Source: pyatv/auth/hap_srp.py SRPAuthHandler.step1/step2 (MIT) and srptools/context.py
//! (MIT): k = H(N | PAD(g)), u = H(PAD(A) | PAD(B)), x = H(s | H(I ":" P)),
//! S = (B - k*g^x)^(a + u*x) mod N, K = H(S), M1 = H(H(N) xor H(g) | H(I) | s | A | B | K),
//! M2 = H(A | M1 | K). Integers are serialized as minimal big-endian bytes except where
//! PAD() is written (left-padded to len(N)). Verified against srptools test vectors in
//! tests/fixtures/hap_srp_vector.json.

use num_bigint::BigUint;
use sha2::{Digest, Sha512};

/// RFC 5054 3072-bit group prime (hex), generator 5.
const N_HEX: &str = concat!(
    "FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74",
    "020BBEA63B139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F1437",
    "4FE1356D6D51C245E485B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7ED",
    "EE386BFB5A899FA5AE9F24117C4B1FE649286651ECE45B3DC2007CB8A163BF05",
    "98DA48361C55D39A69163FA8FD24CF5F83655D23DCA3AD961C62F356208552BB",
    "9ED529077096966D670C354E4ABC9804F1746C08CA18217C32905E462E36CE3B",
    "E39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF695581718",
    "3995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33",
    "A85521ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7",
    "ABF5AE8CDB0933D71E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864",
    "D87602733EC86A64521F2B18177B200CBBE117577A615D6C770988C0BAD946E2",
    "08E24FA074E5AB3143DB5BFCE0FD108E4B82D120A93AD2CAFFFFFFFFFFFFFFFF",
);
const G: u32 = 5;

/// Username fixed by HAP pair-setup.
pub const SRP_USERNAME: &str = "Pair-Setup";
/// PIN used by AirPlay 2 transient pairing.
// Source: pyatv/protocols/airplay/auth/hap_transient.py TRANSIENT_PIN.
pub const TRANSIENT_PIN: &str = "3939";

fn n() -> BigUint {
    BigUint::parse_bytes(N_HEX.as_bytes(), 16).expect("valid prime")
}

fn h(parts: &[&[u8]]) -> [u8; 64] {
    let mut hasher = Sha512::new();
    for p in parts {
        hasher.update(p);
    }
    hasher.finalize().into()
}

/// Minimal big-endian bytes (srptools `int_to_bytes`): zero is a single 0x00 byte.
fn be(v: &BigUint) -> Vec<u8> {
    v.to_bytes_be()
}

/// Minimal big-endian bytes of a digest interpreted as an integer (drops leading zero bytes).
fn digest_as_int_bytes(d: &[u8]) -> Vec<u8> {
    be(&BigUint::from_bytes_be(d))
}

fn pad(v: &BigUint, len: usize) -> Vec<u8> {
    let b = be(v);
    let mut out = vec![0u8; len.saturating_sub(b.len())];
    out.extend_from_slice(&b);
    out
}

/// Result of processing the accessory's salt and public key.
#[derive(Debug, Clone)]
pub struct SrpProof {
    /// Client public key A (minimal big-endian), sent in pair-setup M3.
    pub a_pub: Vec<u8>,
    /// Client proof M1, sent in pair-setup M3.
    pub m1: [u8; 64],
    /// Expected accessory proof M2 (pair-setup M4).
    pub m2: [u8; 64],
    /// Shared session key K = H(S); input to all HKDF derivations.
    pub session_key: [u8; 64],
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SrpError {
    #[error("accessory public key B is invalid (B mod N == 0)")]
    InvalidServerKey,
}

/// Runs the client side of SRP-6a for a given private exponent `a` (32 random bytes in pyatv).
pub fn client_proof(
    a_private: &[u8],
    salt: &[u8],
    b_pub: &[u8],
    username: &str,
    password: &str,
) -> Result<SrpProof, SrpError> {
    let n = n();
    let g = BigUint::from(G);
    let n_len = be(&n).len();

    let b = BigUint::from_bytes_be(b_pub);
    if (&b % &n) == BigUint::from(0u32) {
        return Err(SrpError::InvalidServerKey);
    }
    let a = BigUint::from_bytes_be(a_private);
    let a_pub = g.modpow(&a, &n);

    let k = BigUint::from_bytes_be(&h(&[&be(&n), &pad(&g, n_len)]));
    let u = BigUint::from_bytes_be(&h(&[&pad(&a_pub, n_len), &pad(&b, n_len)]));
    // srptools passes the salt as an integer, so it is re-serialized minimally.
    let salt_bytes = be(&BigUint::from_bytes_be(salt));
    let inner = h(&[username.as_bytes(), b":", password.as_bytes()]);
    let x = BigUint::from_bytes_be(&h(&[&salt_bytes, &inner]));

    let v = g.modpow(&x, &n);
    let kv = (&k * &v) % &n;
    let base = ((&b + &n) - kv) % &n;
    let exp = &a + &u * &x;
    let s = base.modpow(&exp, &n);
    let session_key = h(&[&be(&s)]);

    let hn = BigUint::from_bytes_be(&h(&[&be(&n)]));
    let hg = BigUint::from_bytes_be(&h(&[&be(&g)]));
    let hn_xor_hg = be(&(hn ^ hg));
    let hi = digest_as_int_bytes(&h(&[username.as_bytes()]));
    let a_bytes = be(&a_pub);
    let m1 = h(&[
        &hn_xor_hg,
        &hi,
        &salt_bytes,
        &a_bytes,
        &be(&b),
        &session_key,
    ]);
    let m2 = h(&[&a_bytes, &m1, &session_key]);

    Ok(SrpProof {
        a_pub: a_bytes,
        m1,
        m2,
        session_key,
    })
}

/// Accessory (server) side of the same SRP-6a variant. Used by mock receivers in tests
/// (`tests/ap2_fake_receiver.rs`); not needed by the sender itself.
pub mod server {
    use super::*;

    pub struct SrpServer {
        n: BigUint,
        v: BigUint,
        b: BigUint,
        b_pub: BigUint,
        salt: Vec<u8>,
    }

    impl SrpServer {
        pub fn new(salt: &[u8], b_private: &[u8], username: &str, password: &str) -> Self {
            let n = n();
            let g = BigUint::from(G);
            let n_len = be(&n).len();
            let salt_bytes = be(&BigUint::from_bytes_be(salt));
            let inner = h(&[username.as_bytes(), b":", password.as_bytes()]);
            let x = BigUint::from_bytes_be(&h(&[&salt_bytes, &inner]));
            let v = g.modpow(&x, &n);
            let k = BigUint::from_bytes_be(&h(&[&be(&n), &pad(&g, n_len)]));
            let b = BigUint::from_bytes_be(b_private);
            let b_pub = (&k * &v + g.modpow(&b, &n)) % &n;
            Self {
                n,
                v,
                b,
                b_pub,
                salt: salt.to_vec(),
            }
        }

        pub fn b_pub(&self) -> Vec<u8> {
            be(&self.b_pub)
        }

        /// Verifies the client's M1; returns (session key K, M2) on success.
        pub fn verify(
            &self,
            a_pub: &[u8],
            m1: &[u8],
            username: &str,
        ) -> Option<([u8; 64], [u8; 64])> {
            let n_len = be(&self.n).len();
            let a = BigUint::from_bytes_be(a_pub);
            if (&a % &self.n) == BigUint::from(0u32) {
                return None;
            }
            let u = BigUint::from_bytes_be(&h(&[&pad(&a, n_len), &pad(&self.b_pub, n_len)]));
            let s = (&a * self.v.modpow(&u, &self.n)).modpow(&self.b, &self.n);
            let key = h(&[&be(&s)]);
            let g = BigUint::from(G);
            let hn = BigUint::from_bytes_be(&h(&[&be(&self.n)]));
            let hg = BigUint::from_bytes_be(&h(&[&be(&g)]));
            let salt_bytes = be(&BigUint::from_bytes_be(&self.salt));
            let expected = h(&[
                &be(&(hn ^ hg)),
                &digest_as_int_bytes(&h(&[username.as_bytes()])),
                &salt_bytes,
                &be(&a),
                &be(&self.b_pub),
                &key,
            ]);
            if expected.as_slice() != m1 {
                return None;
            }
            let m2 = h(&[&be(&a), m1, &key]);
            Some((key, m2))
        }
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
    fn matches_srptools_vector() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/hap_srp_vector.json")).unwrap();
        let p = client_proof(
            &unhex(v["a"].as_str().unwrap()),
            &unhex(v["salt"].as_str().unwrap()),
            &unhex(v["B"].as_str().unwrap()),
            SRP_USERNAME,
            TRANSIENT_PIN,
        )
        .unwrap();
        assert_eq!(p.a_pub, unhex(v["A"].as_str().unwrap()), "A");
        assert_eq!(p.session_key.to_vec(), unhex(v["K"].as_str().unwrap()), "K");
        assert_eq!(p.m1.to_vec(), unhex(v["M1"].as_str().unwrap()), "M1");
        assert_eq!(p.m2.to_vec(), unhex(v["M2"].as_str().unwrap()), "M2");
    }

    #[test]
    fn client_and_server_agree() {
        let srv = server::SrpServer::new(&[7; 16], &[9; 32], SRP_USERNAME, TRANSIENT_PIN);
        let p = client_proof(
            &[3; 32],
            &[7; 16],
            &srv.b_pub(),
            SRP_USERNAME,
            TRANSIENT_PIN,
        )
        .unwrap();
        let (k, m2) = srv
            .verify(&p.a_pub, &p.m1, SRP_USERNAME)
            .expect("proof accepted");
        assert_eq!(k, p.session_key);
        assert_eq!(m2, p.m2);
        let wrong = client_proof(&[3; 32], &[7; 16], &srv.b_pub(), SRP_USERNAME, "0000").unwrap();
        assert!(srv.verify(&wrong.a_pub, &wrong.m1, SRP_USERNAME).is_none());
    }

    #[test]
    fn rejects_zero_b() {
        let n_bytes = be(&n());
        assert_eq!(
            client_proof(&[1; 32], &[2; 16], &n_bytes, SRP_USERNAME, TRANSIENT_PIN).unwrap_err(),
            SrpError::InvalidServerKey
        );
    }
}
