//! Relayer key material: one 32-byte seed per relayer (0600 file) derives
//! an ed25519 key (NEAR mint/admin attestations) and an SLH-DSA-SHA2-128s
//! key (native release/admin approvals, enforced by ZeroFlux consensus).

use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};
use zfx_verify::tx::LeafKey;

pub struct RelayerKeys {
    pub ed25519: SigningKey,
    pub slh: LeafKey,
}

impl RelayerKeys {
    pub fn from_seed(seed: &[u8; 32]) -> RelayerKeys {
        let ed: [u8; 32] = Sha256::digest([b"ZFX-relayer-ed25519-v1".as_slice(), seed].concat()).into();
        RelayerKeys { ed25519: SigningKey::from_bytes(&ed), slh: zfx_verify::bridge::member_key(seed) }
    }
    pub fn ed25519_hex(&self) -> String {
        hex::encode(self.ed25519.verifying_key().to_bytes())
    }
    pub fn slh_hex(&self) -> String {
        hex::encode(self.slh.public_key_bytes())
    }
}

pub fn read_seed(path: &str) -> Result<[u8; 32], String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {}", path, e))?;
    hex::decode(s.trim()).ok().and_then(|v| v.try_into().ok()).ok_or_else(|| format!("{}: seed must be 32-byte hex", path))
}

pub fn write_secret(path: &str, contents: &str) -> Result<(), String> {
    if let Some(p) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, contents).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    }
    Ok(())
}
