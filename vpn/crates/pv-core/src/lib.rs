//! Prompt VPN core contracts: typed IRs, findings, policy evaluation, errors.

pub mod category;
pub mod errors;
pub mod policy;
pub mod schema;
pub mod types;

pub use errors::{ErrorCode, PvError};
pub use policy::*;
pub use types::*;

/// Hex SHA-256 helper with `sha256:` prefix.
pub fn sha256_tagged(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// Merge overlapping findings for the transformer. Returns groups of finding
/// indices whose spans overlap (transitively) within the same field. Findings
/// are never dropped: a group keeps every member so the strictest action and
/// highest sensitivity win.
pub fn overlap_groups(findings: &[Finding]) -> Vec<Vec<usize>> {
    let mut idx: Vec<usize> = (0..findings.len()).filter(|&i| !findings[i].whole_field).collect();
    idx.sort_by_key(|&i| (findings[i].field_index, findings[i].start_byte, std::cmp::Reverse(findings[i].end_byte)));
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut cur_end = 0usize;
    let mut cur_field = usize::MAX;
    for i in idx {
        let f = &findings[i];
        if let Some(g) = groups.last_mut() {
            if f.field_index == cur_field && f.start_byte < cur_end {
                g.push(i);
                cur_end = cur_end.max(f.end_byte);
                continue;
            }
        }
        groups.push(vec![i]);
        cur_end = f.end_byte;
        cur_field = f.field_index;
    }
    groups
}
