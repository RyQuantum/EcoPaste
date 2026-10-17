//! Keys, signatures and the message cipher, wire-compatible with CrossPaste's
//! `CryptographyUtils`, `SecureMessageProcessor` and `SecureKeyPairSerializer`.
//!
//! - Identity = one P-256 ECDSA key (signing) + one P-256 ECDH key (encryption).
//! - Public keys travel as X.509 SubjectPublicKeyInfo DER, signatures as DER,
//!   both base64 in JSON.
//! - The long-term ECDH shared secret (raw 32-byte x coordinate) is used
//!   directly as an AES-256-CBC key; PKCS#7 padding, random 16-byte IV
//!   prepended to the ciphertext.

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{DerSignature, SigningKey, VerifyingKey};
use p256::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey};
use p256::{PublicKey, SecretKey};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};

type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("invalid key: {0}")]
    Key(String),
    #[error("decrypt failed")]
    Decrypt,
}

/// This device's long-term identity.
#[derive(Clone)]
pub struct KeyPair {
    sign: SigningKey,
    crypt: SecretKey,
}

impl KeyPair {
    pub fn generate() -> Self {
        Self {
            sign: SigningKey::random(&mut OsRng),
            crypt: SecretKey::random(&mut OsRng),
        }
    }

    /// Restores from PKCS#8 DER private keys.
    pub fn from_pkcs8(sign_der: &[u8], crypt_der: &[u8]) -> Result<Self, CryptoError> {
        let sign =
            SecretKey::from_pkcs8_der(sign_der).map_err(|e| CryptoError::Key(e.to_string()))?;
        let crypt =
            SecretKey::from_pkcs8_der(crypt_der).map_err(|e| CryptoError::Key(e.to_string()))?;
        Ok(Self {
            sign: SigningKey::from(sign),
            crypt,
        })
    }

    pub fn sign_pkcs8(&self) -> Vec<u8> {
        SecretKey::from(&self.sign)
            .to_pkcs8_der()
            .expect("encode sign key")
            .as_bytes()
            .to_vec()
    }

    pub fn crypt_pkcs8(&self) -> Vec<u8> {
        self.crypt
            .to_pkcs8_der()
            .expect("encode crypt key")
            .as_bytes()
            .to_vec()
    }

    pub fn sign_public_der(&self) -> Vec<u8> {
        VerifyingKey::from(&self.sign)
            .to_public_key_der()
            .expect("encode sign public key")
            .as_bytes()
            .to_vec()
    }

    pub fn crypt_public_der(&self) -> Vec<u8> {
        self.crypt
            .public_key()
            .to_public_key_der()
            .expect("encode crypt public key")
            .as_bytes()
            .to_vec()
    }

    /// ECDSA-SHA256, DER-encoded.
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        let sig: DerSignature = self.sign.sign(data);
        sig.as_bytes().to_vec()
    }

    /// Message cipher shared with the peer owning `peer_crypt_public_der`.
    pub fn cipher_for(&self, peer_crypt_public_der: &[u8]) -> Result<MessageCipher, CryptoError> {
        let peer = PublicKey::from_public_key_der(peer_crypt_public_der)
            .map_err(|e| CryptoError::Key(e.to_string()))?;
        let shared = p256::ecdh::diffie_hellman(self.crypt.to_nonzero_scalar(), peer.as_affine());
        let mut key = [0u8; 32];
        key.copy_from_slice(shared.raw_secret_bytes().as_slice());
        Ok(MessageCipher { key })
    }
}

/// Verifies a DER ECDSA-SHA256 signature against a SPKI DER public key.
pub fn verify(sign_public_der: &[u8], data: &[u8], signature_der: &[u8]) -> bool {
    let Ok(key) = VerifyingKey::from_public_key_der(sign_public_der) else {
        return false;
    };
    let Ok(sig) = p256::ecdsa::Signature::from_der(signature_der) else {
        return false;
    };
    key.verify(data, &sig).is_ok()
}

/// Validates that `der` is a P-256 SPKI public key.
pub fn is_valid_public_der(der: &[u8]) -> bool {
    PublicKey::from_public_key_der(der).is_ok()
}

#[derive(Clone)]
pub struct MessageCipher {
    key: [u8; 32],
}

impl MessageCipher {
    pub fn encrypt(&self, data: &[u8]) -> Vec<u8> {
        let mut iv = [0u8; 16];
        OsRng.fill_bytes(&mut iv);
        let ct =
            Aes256CbcEnc::new(&self.key.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(data);
        let mut out = Vec::with_capacity(16 + ct.len());
        out.extend_from_slice(&iv);
        out.extend_from_slice(&ct);
        out
    }

    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if data.len() < 32 || !data.len().is_multiple_of(16) {
            return Err(CryptoError::Decrypt);
        }
        let (iv, ct) = data.split_at(16);
        let iv: [u8; 16] = iv.try_into().unwrap();
        Aes256CbcDec::new(&self.key.into(), &iv.into())
            .decrypt_padded_vec_mut::<Pkcs7>(ct)
            .map_err(|_| CryptoError::Decrypt)
    }
}

/// Short authentication string shown on both screens during v2 pairing:
/// SHA-256 over the two crypt public keys in byte order, first four bytes,
/// masked positive, modulo one million.
pub fn compute_sas(a: &[u8], b: &[u8]) -> u32 {
    let mut hasher = Sha256::new();
    if a <= b {
        hasher.update(a);
        hasher.update(b);
    } else {
        hasher.update(b);
        hasher.update(a);
    }
    let h = hasher.finalize();
    let value = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) & 0x7fff_ffff;
    value % 1_000_000
}

/// Signed payload of a v2 key exchange request or response.
pub fn key_exchange_message(sign_public: &[u8], crypt_public: &[u8], timestamp: i64) -> Vec<u8> {
    format!(
        "{}{}{}",
        B64.encode(sign_public),
        B64.encode(crypt_public),
        timestamp
    )
    .into_bytes()
}

/// Signed payload of a v2 trust confirm request or response.
pub fn trust_confirm_message(timestamp: i64) -> Vec<u8> {
    timestamp.to_string().into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_through_pkcs8() {
        let kp = KeyPair::generate();
        let restored = KeyPair::from_pkcs8(&kp.sign_pkcs8(), &kp.crypt_pkcs8()).unwrap();
        assert_eq!(kp.sign_public_der(), restored.sign_public_der());
        assert_eq!(kp.crypt_public_der(), restored.crypt_public_der());
    }

    #[test]
    fn public_keys_are_spki_der() {
        let kp = KeyPair::generate();
        // SEQUENCE { SEQUENCE { id-ecPublicKey, prime256v1 }, BIT STRING (65-byte point) }
        assert_eq!(kp.sign_public_der().len(), 91);
        assert_eq!(kp.crypt_public_der().len(), 91);
        assert!(is_valid_public_der(&kp.crypt_public_der()));
    }

    #[test]
    fn signature_verifies_and_rejects_tampering() {
        let kp = KeyPair::generate();
        let msg = key_exchange_message(&kp.sign_public_der(), &kp.crypt_public_der(), 42);
        let sig = kp.sign(&msg);
        assert!(verify(&kp.sign_public_der(), &msg, &sig));
        assert!(!verify(&kp.sign_public_der(), b"other", &sig));
    }

    #[test]
    fn both_sides_derive_the_same_cipher() {
        let a = KeyPair::generate();
        let b = KeyPair::generate();
        let ca = a.cipher_for(&b.crypt_public_der()).unwrap();
        let cb = b.cipher_for(&a.crypt_public_der()).unwrap();
        // 16-byte IV + 16 bytes of data + a full PKCS#7 padding block.
        let ct = ca.encrypt(b"hello crosspaste");
        assert_eq!(ct.len(), 48);
        assert_eq!(ca.encrypt(b"").len(), 32);
        assert_eq!(cb.decrypt(&ct).unwrap(), b"hello crosspaste");
        assert!(cb.decrypt(&ct[..16]).is_err());
    }

    #[test]
    fn sas_is_order_independent_and_six_digits() {
        let a = KeyPair::generate().crypt_public_der();
        let b = KeyPair::generate().crypt_public_der();
        assert_eq!(compute_sas(&a, &b), compute_sas(&b, &a));
        assert!(compute_sas(&a, &b) < 1_000_000);
    }
}
