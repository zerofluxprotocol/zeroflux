//! Request authentication: ed25519 signature over the canonical message, the key registered to
//! the account on-chain, a timestamp within `MAX_SKEW_MS`, and no replay of the same signature
//! inside that window. Plus a per-account token-bucket rate limit.

use crate::dto::{key_from_str, sig_from_str};
use crate::state::Shared;
use ed25519_dalek::{Signature, VerifyingKey};
use zfx_perps_engine::{AccountId, Key};
use zfx_perps_wire::MAX_SKEW_MS;
use parking_lot::Mutex;
use rustc_hash::FxHashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

pub fn now_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    BadKey(String),
    BadSignature,
    UnknownAccount,
    KeyNotRegistered,
    StaleTimestamp,
    Replay,
    RateLimited,
}

impl AuthError {
    pub fn code(&self) -> &'static str {
        match self {
            AuthError::BadKey(_) => "bad_key",
            AuthError::BadSignature => "bad_signature",
            AuthError::UnknownAccount => "unknown_account",
            AuthError::KeyNotRegistered => "key_not_registered",
            AuthError::StaleTimestamp => "stale_timestamp",
            AuthError::Replay => "replay",
            AuthError::RateLimited => "rate_limited",
        }
    }
}

pub fn verify(pk: &Key, msg: &[u8], sig: &[u8; 64]) -> bool {
    VerifyingKey::from_bytes(pk).is_ok_and(|vk| vk.verify_strict(msg, &Signature::from_bytes(sig)).is_ok())
}

pub struct Auth {
    seen: Mutex<FxHashMap<[u8; 64], u64>>,
    buckets: Mutex<FxHashMap<AccountId, (f64, Instant)>>,
    rate: f64,
}

impl Auth {
    pub fn new(rate_per_sec: u32) -> Self {
        Auth {
            seen: Mutex::new(FxHashMap::default()),
            buckets: Mutex::new(FxHashMap::default()),
            rate: rate_per_sec.max(1) as f64,
        }
    }

    /// Check a signed request. `ts_ms` = None skips the freshness and replay checks (withdrawals
    /// are protected by their nonce instead, on-chain and in the engine).
    pub fn check(
        &self,
        shared: &Shared,
        account: &str,
        public_key: &str,
        signature: &str,
        msg: &[u8],
        ts_ms: Option<u64>,
    ) -> Result<(AccountId, Key, [u8; 64]), AuthError> {
        let pk = key_from_str(public_key).map_err(AuthError::BadKey)?;
        let sig = sig_from_str(signature).map_err(AuthError::BadKey)?;
        let id = {
            let d = shared.dir.read();
            let id = *d.by_name.get(account).ok_or(AuthError::UnknownAccount)?;
            if d.key_owner.get(&pk) != Some(&id) {
                return Err(AuthError::KeyNotRegistered);
            }
            id
        };
        if let Some(ts) = ts_ms
            && now_ms().abs_diff(ts) > MAX_SKEW_MS
        {
            return Err(AuthError::StaleTimestamp);
        }
        if !verify(&pk, msg, &sig) {
            return Err(AuthError::BadSignature);
        }
        if let Some(ts) = ts_ms {
            let mut seen = self.seen.lock();
            if seen.insert(sig, ts).is_some() {
                return Err(AuthError::Replay);
            }
            if seen.len() > 1_000_000 {
                let cutoff = now_ms().saturating_sub(2 * MAX_SKEW_MS);
                seen.retain(|_, t| *t >= cutoff);
            }
        }
        self.take(id)?;
        Ok((id, pk, sig))
    }

    fn take(&self, id: AccountId) -> Result<(), AuthError> {
        let mut b = self.buckets.lock();
        let now = Instant::now();
        let e = b.entry(id).or_insert((2.0 * self.rate, now));
        e.0 = (e.0 + now.duration_since(e.1).as_secs_f64() * self.rate).min(2.0 * self.rate);
        e.1 = now;
        if e.0 < 1.0 {
            return Err(AuthError::RateLimited);
        }
        e.0 -= 1.0;
        Ok(())
    }

    /// Drop replay entries older than twice the skew window.
    pub fn purge(&self) {
        let cutoff = now_ms().saturating_sub(2 * MAX_SKEW_MS);
        self.seen.lock().retain(|_, t| *t >= cutoff);
    }
}
