use crate::Error;
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};

/// An ed25519 trading key. Register its public key on the settlement contract
/// (`register_key`) before trading.
#[derive(Clone)]
pub struct Signer {
    key: SigningKey,
}

impl Signer {
    pub fn generate() -> Self {
        Signer {
            key: SigningKey::generate(&mut rand::rngs::OsRng),
        }
    }

    pub fn from_seed(seed: [u8; 32]) -> Self {
        Signer {
            key: SigningKey::from_bytes(&seed),
        }
    }

    /// NEAR secret key format: `ed25519:<base58 of 64 bytes (seed || public key)>` (or 32-byte seed).
    pub fn from_near_secret(s: &str) -> Result<Self, Error> {
        let b58 = s
            .strip_prefix("ed25519:")
            .ok_or_else(|| Error::Key("expected ed25519:<base58>".into()))?;
        let b = bs58::decode(b58).into_vec().map_err(|e| Error::Key(e.to_string()))?;
        let seed: [u8; 32] = b
            .get(..32)
            .and_then(|x| x.try_into().ok())
            .ok_or_else(|| Error::Key("too short".into()))?;
        let s = Self::from_seed(seed);
        if b.len() == 64 && b[32..] != s.public_key_bytes() {
            return Err(Error::Key("public half does not match seed".into()));
        }
        Ok(s)
    }

    pub fn near_secret(&self) -> String {
        let mut b = self.key.to_bytes().to_vec();
        b.extend_from_slice(&self.public_key_bytes());
        format!("ed25519:{}", bs58::encode(b).into_string())
    }

    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// `ed25519:<base58>`, the format NEAR and the node use.
    pub fn public_key(&self) -> String {
        format!("ed25519:{}", bs58::encode(self.public_key_bytes()).into_string())
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.key.sign(msg).to_bytes()
    }

    pub fn sign_b64(&self, msg: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.sign(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_secret_roundtrip() {
        let s = Signer::from_seed([3; 32]);
        let t = Signer::from_near_secret(&s.near_secret()).unwrap();
        assert_eq!(s.public_key(), t.public_key());
        assert!(s.public_key().starts_with("ed25519:"));
    }
}
