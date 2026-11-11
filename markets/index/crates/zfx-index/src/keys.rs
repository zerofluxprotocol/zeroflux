//! Reporter signing key. The 32-byte ed25519 seed lives outside the repository
//! (default `~/.config/zeroflux/oracle-reporter.key`, mode 0600, hex encoded).
//! Only the public key is committed.

use crate::engine::Signer;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn default_key_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/zeroflux/oracle-reporter.key")
}

pub fn generate(path: &Path) -> Result<[u8; 32], String> {
    if path.exists() {
        return Err(format!("{} exists; refusing to overwrite a reporter key", path.display()));
    }
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|e| e.to_string())?;
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| e.to_string())?;
    writeln!(f, "{}", hex::encode(seed)).map_err(|e| e.to_string())?;
    Ok(zfx_oracle_print::public_key(&seed))
}

pub fn load(path: &Path) -> Result<Signer, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(format!("{} is readable by group/others (mode {:o}); chmod 600 it", path.display(), mode & 0o777));
        }
    }
    let s = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let b = hex::decode(s.trim()).map_err(|e| e.to_string())?;
    let seed: [u8; 32] = b.try_into().map_err(|_| "key must be 32 bytes hex".to_string())?;
    Ok(Signer { seed })
}
