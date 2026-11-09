//! Native side of the ZeroFlux <-> NEAR bridge (consensus v2).
//!
//! Custody model (BRIDGE_TRUST_MODEL.md): native ZFX deposited to the
//! lockbox address can leave it only through a `Release` operation that
//! carries approvals from at least `threshold` *distinct* members of the
//! current committee, each an SLH-DSA-SHA2-128s signature over the exact
//! release digest. Nobody holds a key for the lockbox address itself: it is
//! a hash of a domain string, not a P2MR root, so no ordinary transaction
//! can spend from it. Consensus, not a relayer, enforces the quorum.

use crate::hash::{sha256, Hash32};
use crate::tx::{verify_slh, Address, SCHEME_SLH_DSA_SHA2_128S};
use serde::{Deserialize, Serialize};

pub const BRIDGE_SIG_CTX: &[u8] = b"ZFX-bridge-v1";
pub const COMMITTEE_SIZE: usize = 3;
pub const MAX_NEAR_REF_LEN: usize = 128;
pub const MAX_BRIDGE_OPS_PER_BLOCK: usize = 64;

/// Canonical native asset id, also used by the NEAR contract.
pub fn native_asset_id(chain_id: u32) -> String {
    format!("zfx:{:08x}:native", chain_id)
}

/// The lockbox address: hash of a domain string, so no key controls it.
pub fn lockbox_address(chain_id: u32) -> Address {
    sha256(&[b"ZFX-bridge-lockbox-v1", &chain_id.to_le_bytes()])
}

/// Consensus bridge parameters, committed in the genesis config.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeParams {
    /// Initial committee: SLH-DSA-SHA2-128s public keys (hex), exactly 3.
    pub committee: Vec<String>,
    /// Approvals needed for releases, unpause and rotation (2).
    pub threshold: u8,
    /// Minimum blocks between a rotation's inclusion and its activation.
    pub rotation_timelock_blocks: u64,
    /// Smallest accepted deposit (base units).
    pub min_lock: u64,
    /// Height from which deposits and bridge ops are valid.
    pub activation_height: u64,
    /// NEAR wZFX contract account this bridge pairs with (informational
    /// for relayers; bound into the asset id domain off-chain).
    pub near_contract: String,
}

impl BridgeParams {
    pub fn committee_keys(&self) -> Result<Vec<Vec<u8>>, String> {
        if self.committee.len() != COMMITTEE_SIZE {
            return Err(format!("committee must have exactly {} members", COMMITTEE_SIZE));
        }
        let keys: Vec<Vec<u8>> = self.committee.iter().map(|h| hex::decode(h).map_err(|e| e.to_string())).collect::<Result<_, _>>()?;
        check_member_set(&keys)?;
        if self.threshold < 2 || self.threshold as usize > COMMITTEE_SIZE {
            return Err("threshold must be 2..=3".into());
        }
        Ok(keys)
    }

    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut b = b"ZFX-bridge-params-v1".to_vec();
        for k in &self.committee {
            let raw = hex::decode(k).unwrap_or_default();
            b.extend_from_slice(&(raw.len() as u16).to_le_bytes());
            b.extend_from_slice(&raw);
        }
        b.push(self.threshold);
        b.extend_from_slice(&self.rotation_timelock_blocks.to_le_bytes());
        b.extend_from_slice(&self.min_lock.to_le_bytes());
        b.extend_from_slice(&self.activation_height.to_le_bytes());
        b.push(self.near_contract.len() as u8);
        b.extend_from_slice(self.near_contract.as_bytes());
        b
    }
}

pub fn check_member_set(keys: &[Vec<u8>]) -> Result<(), String> {
    if keys.len() != COMMITTEE_SIZE {
        return Err("committee must have exactly 3 members".into());
    }
    for k in keys {
        if k.len() != 32 {
            return Err("committee keys must be SLH-DSA-SHA2-128s (32-byte) public keys".into());
        }
    }
    if keys[0] == keys[1] || keys[0] == keys[2] || keys[1] == keys[2] {
        return Err("committee keys must be distinct".into());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    /// Committee index 0..3.
    pub member: u8,
    #[serde(with = "crate::tx::hexvec")]
    pub sig: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub epoch: u32,
    /// Single-use id derived from the finalized NEAR burn (see relayers).
    #[serde(with = "crate::header::hex32")]
    pub withdrawal_id: Hash32,
    /// NEAR burn evidence reference: "<contract>:<burn_nonce>:<tx_hash>".
    pub near_burn_ref: String,
    #[serde(with = "crate::tx::addr_str")]
    pub recipient: Address,
    pub amount: u64,
    /// Release invalid above this height (stale approvals expire).
    pub expiry_height: u64,
    pub approvals: Vec<Approval>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AdminAction {
    /// Halt deposits and releases. Any single member may pause.
    Pause,
    /// Resume. Needs `threshold` approvals.
    Unpause,
    /// Replace the committee at `activate_height` (timelocked); epoch + 1.
    Rotate { members: Vec<String>, activate_height: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdminOp {
    pub epoch: u32,
    /// Must equal the bridge state's admin nonce (replay protection).
    pub op_nonce: u64,
    #[serde(flatten)]
    pub action: AdminAction,
    pub approvals: Vec<Approval>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum BridgeOp {
    Release(Release),
    Admin(AdminOp),
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(&(s.len() as u16).to_le_bytes());
    b.extend_from_slice(s.as_bytes());
}

/// Exact bytes every committee member signs for a release.
pub fn release_digest(chain_id: u32, r: &Release) -> Hash32 {
    let mut b = Vec::with_capacity(160);
    b.extend_from_slice(&chain_id.to_le_bytes());
    put_str(&mut b, &native_asset_id(chain_id));
    b.extend_from_slice(&r.epoch.to_le_bytes());
    b.extend_from_slice(&r.withdrawal_id);
    b.extend_from_slice(&r.recipient);
    b.extend_from_slice(&r.amount.to_le_bytes());
    b.extend_from_slice(&r.expiry_height.to_le_bytes());
    b.extend_from_slice(&sha256(&[r.near_burn_ref.as_bytes()]));
    sha256(&[b"ZFX-bridge-release-v1", &b])
}

fn action_bytes(a: &AdminAction) -> Vec<u8> {
    let mut b = Vec::new();
    match a {
        AdminAction::Pause => b.push(1),
        AdminAction::Unpause => b.push(2),
        AdminAction::Rotate { members, activate_height } => {
            b.push(3);
            for m in members {
                put_str(&mut b, m);
            }
            b.extend_from_slice(&activate_height.to_le_bytes());
        }
    }
    b
}

pub fn admin_digest(chain_id: u32, op: &AdminOp) -> Hash32 {
    let mut b = chain_id.to_le_bytes().to_vec();
    b.extend_from_slice(&op.epoch.to_le_bytes());
    b.extend_from_slice(&op.op_nonce.to_le_bytes());
    b.extend_from_slice(&action_bytes(&op.action));
    sha256(&[b"ZFX-bridge-admin-v1", &b])
}

/// Canonical encoding (for the block tx_root leaf and size limits).
pub fn encode_op(op: &BridgeOp) -> Vec<u8> {
    let mut b = Vec::new();
    let approvals = match op {
        BridgeOp::Release(r) => {
            b.push(1);
            b.extend_from_slice(&r.epoch.to_le_bytes());
            b.extend_from_slice(&r.withdrawal_id);
            put_str(&mut b, &r.near_burn_ref);
            b.extend_from_slice(&r.recipient);
            b.extend_from_slice(&r.amount.to_le_bytes());
            b.extend_from_slice(&r.expiry_height.to_le_bytes());
            &r.approvals
        }
        BridgeOp::Admin(a) => {
            b.push(2);
            b.extend_from_slice(&a.epoch.to_le_bytes());
            b.extend_from_slice(&a.op_nonce.to_le_bytes());
            b.extend_from_slice(&action_bytes(&a.action));
            &a.approvals
        }
    };
    b.push(approvals.len() as u8);
    for ap in approvals {
        b.push(ap.member);
        b.extend_from_slice(&(ap.sig.len() as u16).to_le_bytes());
        b.extend_from_slice(&ap.sig);
    }
    b
}

pub fn op_id(op: &BridgeOp) -> Hash32 {
    sha256(&[b"ZFX-bridgeop-v1", &encode_op(op)])
}

/// Count distinct committee members whose signature over `digest` verifies.
/// Duplicate member indices, out-of-range indices and bad signatures are
/// errors (not silently ignored), so malformed bundles are rejected.
pub fn count_approvals(members: &[Vec<u8>], digest: &Hash32, approvals: &[Approval]) -> Result<usize, String> {
    if approvals.len() > COMMITTEE_SIZE {
        return Err("too many approvals".into());
    }
    let mut seen = [false; COMMITTEE_SIZE];
    for ap in approvals {
        let i = ap.member as usize;
        if i >= members.len() {
            return Err(format!("approval from unknown member {}", i));
        }
        if seen[i] {
            return Err(format!("duplicate approval from member {}", i));
        }
        if !verify_slh(SCHEME_SLH_DSA_SHA2_128S, &members[i], digest, &ap.sig, BRIDGE_SIG_CTX) {
            return Err(format!("invalid signature from member {}", i));
        }
        seen[i] = true;
    }
    Ok(seen.iter().filter(|s| **s).count())
}

/// Committee member signing key, derived from a 32-byte relayer seed.
pub fn member_key(seed: &[u8; 32]) -> crate::tx::LeafKey {
    let s = sha256(&[b"ZFX-bridge-member-key-v1", seed]);
    crate::tx::LeafKey::derive(&s, SCHEME_SLH_DSA_SHA2_128S, 0).expect("128s keygen")
}

pub fn sign_digest(key: &crate::tx::LeafKey, digest: &Hash32) -> Vec<u8> {
    key.sign_ctx(digest, BRIDGE_SIG_CTX).expect("SLH-DSA sign")
}
