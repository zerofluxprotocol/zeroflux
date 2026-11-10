//! Transformations (tokenize, redact, generalize) and placeholder restoration.
//!
//! Placeholders are minted by a [`TokenStore`]: the encrypted local vault
//! (local daemon) or an [`EphemeralStore`] that lives for one request or one
//! client session (hosted gateway, protected-transport SDK). Restoration only
//! ever resolves tokens of the caller's own scope.

use chrono::{DateTime, Duration, Utc};
use pv_core::category::*;
use pv_core::{overlap_groups, Action, Decision, Finding};
use pv_vault::{token_label, Vault};
use rand::RngCore;
use regex::Regex;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use zeroize::Zeroizing;

/// Current placeholder format: `[[ZFX_<LABEL>_<16 hex>]]` (64 random bits).
/// Legacy `[[PV_<LABEL>_<8 hex>]]` placeholders minted by Prompt VPN 0.1 are
/// still recognized so mappings created before an upgrade restore until they expire.
pub static TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\[(?:ZFX_[A-Z0-9_]{1,40}_[0-9a-f]{16}|PV_[A-Z0-9_]{1,40}_[0-9a-f]{8})\]\]").unwrap());
pub const TOKEN_HEAD: &str = "[[ZFX_";
pub const LEGACY_TOKEN_HEAD: &str = "[[PV_";
/// Longest possible placeholder, used to size streaming hold-back.
pub const MAX_TOKEN_LEN: usize = 6 + 40 + 1 + 16 + 2;

#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    #[error("vault: {0}")]
    Vault(#[from] pv_vault::VaultError),
    #[error("token store unavailable: {0}")]
    Store(String),
    #[error("finding {0} cannot be isolated to a span; refusing partial transformation")]
    NotIsolatable(String),
    #[error("finding offsets invalid")]
    BadOffsets,
}

/// Where reversible placeholder mappings live.
pub trait TokenStore: Send + Sync {
    /// Existing token for `value` in `scope`, or a newly minted one.
    fn token_for(&self, scope: &str, category: &str, value: &str, ttl: Duration) -> Result<String, TransformError>;
    /// Value for `token`, only within `scope` and before expiry.
    fn resolve_token(&self, scope: &str, token: &str) -> Result<Option<Zeroizing<String>>, TransformError>;
}

impl TokenStore for Vault {
    fn token_for(&self, scope: &str, category: &str, value: &str, ttl: Duration) -> Result<String, TransformError> {
        Ok(Vault::token_for(self, scope, category, value, ttl)?)
    }
    fn resolve_token(&self, scope: &str, token: &str) -> Result<Option<Zeroizing<String>>, TransformError> {
        Ok(Vault::resolve_token(self, scope, token)?)
    }
}

/// Mint a new placeholder for `category`.
pub fn mint_token(category: &str) -> String {
    let mut b = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut b);
    format!("{TOKEN_HEAD}{}_{}]]", token_label(category), hex_lower(&b))
}

fn hex_lower(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

struct EphemeralEntry {
    scope: String,
    value: Zeroizing<String>,
    expires_at: DateTime<Utc>,
}

/// In-memory mapping store. Nothing is written to disk; dropping the store
/// zeroizes every value. Used for per-request hosted state and for the
/// client-side protected transport, where the mapping never leaves the client.
#[derive(Default)]
pub struct EphemeralStore {
    inner: Mutex<HashMap<String, EphemeralEntry>>,
}

impl EphemeralStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Remove every mapping now (values are zeroized on drop).
    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }
    /// Values held, for leak tests in callers that must prove what they hold.
    pub fn contains_value(&self, v: &str) -> bool {
        self.inner.lock().unwrap().values().any(|e| e.value.as_str() == v)
    }
}

impl TokenStore for EphemeralStore {
    fn token_for(&self, scope: &str, category: &str, value: &str, ttl: Duration) -> Result<String, TransformError> {
        let mut m = self.inner.lock().map_err(|_| TransformError::Store("poisoned".into()))?;
        let now = Utc::now();
        if let Some((t, e)) = m.iter_mut().find(|(_, e)| e.scope == scope && e.value.as_str() == value && e.expires_at > now) {
            e.expires_at = now + ttl;
            return Ok(t.clone());
        }
        for _ in 0..8 {
            let t = mint_token(category);
            if !m.contains_key(&t) {
                m.insert(t.clone(), EphemeralEntry { scope: scope.to_string(), value: Zeroizing::new(value.to_string()), expires_at: now + ttl });
                return Ok(t);
            }
        }
        Err(TransformError::Store("could not allocate token".into()))
    }
    fn resolve_token(&self, scope: &str, token: &str) -> Result<Option<Zeroizing<String>>, TransformError> {
        let m = self.inner.lock().map_err(|_| TransformError::Store("poisoned".into()))?;
        Ok(m.get(token).filter(|e| e.scope == scope && e.expires_at > Utc::now()).map(|e| e.value.clone()))
    }
}

#[derive(Debug, Clone, Default)]
pub struct TransformReport {
    pub tokenized: u32,
    pub redacted: u32,
    pub generalized: u32,
    /// Fields touched by GENERALIZE, which must be reclassified before sending.
    pub generalized_fields: Vec<usize>,
}

fn redaction(category: &str) -> String {
    format!("[REDACTED_{}]", token_label(category))
}

/// Deterministic, specificity-reducing replacement. Never invents values.
pub fn generalize(category: &str, original: &str) -> String {
    match category {
        EMAIL => match original.rsplit_once('@') {
            Some((_, domain)) if domain.contains('.') => format!("[an email address at {}]", domain.to_ascii_lowercase()),
            _ => "[an email address]".into(),
        },
        PHONE => "[a phone number]".into(),
        ADDRESS => "[a street address]".into(),
        PERSON => "[a person]".into(),
        DOB => "[a date of birth]".into(),
        HEALTH_INFORMATION => "[a health condition]".into(),
        FINANCIAL_ACCOUNT => "[a bank account]".into(),
        c if c.starts_with(CUSTOM_PREFIX) => "[a confidential detail]".into(),
        _ => redaction(category),
    }
}

/// Apply the decision's transform actions to `fields` in place.
///
/// Overlapping findings are merged into one replacement covering their union;
/// the strictest action and the most sensitive category in the group win, so a
/// secret is never left exposed because a weaker finding overlapped it.
pub fn apply(fields: &mut [String], findings: &[Finding], decision: &Decision, vault: &dyn TokenStore, scope: &str, ttl: Duration) -> Result<TransformReport, TransformError> {
    let mut rep = TransformReport::default();
    let action_of = |f: &Finding| decision.action_for(&f.finding_id).unwrap_or(Action::Allow);

    for f in findings {
        if f.whole_field && action_of(f).is_transform() {
            return Err(TransformError::NotIsolatable(f.category.clone()));
        }
    }

    // Build replacement plans per field: (start, end, action, category).
    let mut plans: Vec<(usize, usize, usize, Action, String)> = Vec::new();
    for group in overlap_groups(findings) {
        let members: Vec<&Finding> = group.iter().map(|&i| &findings[i]).filter(|f| action_of(f).is_transform()).collect();
        if members.is_empty() {
            continue;
        }
        let field = members[0].field_index;
        // Union over ALL group members (not only transformed ones) so overlapping
        // sensitive bytes are covered too.
        let all: Vec<&Finding> = group.iter().map(|&i| &findings[i]).collect();
        let start = all.iter().map(|f| f.start_byte).min().unwrap();
        let end = all.iter().map(|f| f.end_byte).max().unwrap();
        let action = members.iter().map(|f| action_of(f)).max_by_key(|a| a.strictness()).unwrap();
        let category = all.iter().max_by_key(|f| f.sensitivity).map(|f| f.category.clone()).unwrap();
        plans.push((field, start, end, action, category));
    }
    plans.sort_by_key(|a| (a.0, std::cmp::Reverse(a.1)));

    for (field, start, end, action, category) in plans {
        let text = fields.get_mut(field).ok_or(TransformError::BadOffsets)?;
        if end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
            return Err(TransformError::BadOffsets);
        }
        let original = zeroize::Zeroizing::new(text[start..end].to_string());
        let replacement = match action {
            Action::Tokenize => {
                rep.tokenized += 1;
                vault.token_for(scope, &category, &original, ttl)?
            }
            Action::Redact => {
                rep.redacted += 1;
                redaction(&category)
            }
            Action::Generalize => {
                rep.generalized += 1;
                if !rep.generalized_fields.contains(&field) {
                    rep.generalized_fields.push(field);
                }
                generalize(&category, &original)
            }
            _ => continue,
        };
        text.replace_range(start..end, &replacement);
    }
    Ok(rep)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RestoreStats {
    pub restored: u32,
    pub unknown: u32,
}

/// Replace exact known tokens of `scope` with their values. Tokens from other
/// scopes, expired tokens and malformed look-alikes are left untouched.
pub fn restore(text: &str, vault: &dyn TokenStore, scope: &str) -> Result<(String, RestoreStats), TransformError> {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut st = RestoreStats::default();
    for m in TOKEN_RE.find_iter(text) {
        out.push_str(&text[last..m.start()]);
        match vault.resolve_token(scope, m.as_str())? {
            Some(v) => {
                out.push_str(&v);
                st.restored += 1;
            }
            None => {
                out.push_str(m.as_str());
                st.unknown += 1;
            }
        }
        last = m.end();
    }
    out.push_str(&text[last..]);
    Ok((out, st))
}

/// Whether `s` could be the beginning of a placeholder token (current or legacy).
pub fn is_token_prefix(s: &str) -> bool {
    prefix_of(s, TOKEN_HEAD, 16) || prefix_of(s, LEGACY_TOKEN_HEAD, 8)
}

fn prefix_of(s: &str, head: &str, hex_len: usize) -> bool {
    if s.len() <= head.len() {
        return head.starts_with(s);
    }
    if !s.starts_with(head) {
        return false;
    }
    // body: [A-Z0-9_]{1,40} '_' [0-9a-f]{hex_len} "]]". Labels may contain '_'
    // and digits, so accept any run of label/hex characters up to the maximum
    // length, then an optional partial terminator.
    let rest = &s[head.len()..];
    let body_len = rest.find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || matches!(c, 'a'..='f'))).unwrap_or(rest.len());
    if body_len > 40 + 1 + hex_len {
        return false;
    }
    matches!(&rest[body_len..], "" | "]" | "]]")
}

/// Largest prefix of `buf` that can be released without splitting a token.
pub fn safe_release_len(buf: &str) -> usize {
    let start = buf.len().saturating_sub(MAX_TOKEN_LEN);
    let mut i = start;
    while i < buf.len() {
        if buf.is_char_boundary(i) && buf.as_bytes()[i] == b'[' {
            let tail = &buf[i..];
            if is_token_prefix(tail) && !TOKEN_RE.is_match(tail) {
                return i;
            }
        }
        i += 1;
    }
    buf.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pv_core::*;
    use pv_vault::keys::MemoryBackend;
    use std::sync::Arc;

    fn vault() -> Vault {
        Vault::open(pv_audit::Db::open_memory().unwrap(), Arc::new(MemoryBackend::default())).unwrap()
    }

    fn f(cat: &str, s: usize, e: usize) -> Finding {
        Finding::new(ClassifierType::Builtin, cat, s, e, 0.9, "t", "p", category::sensitivity_of(cat))
    }

    fn decision(fs: &[(&Finding, Action)]) -> Decision {
        Decision {
            schema_version: 1,
            disposition: Disposition::Transform,
            findings: fs.iter().map(|(f, a)| FindingDecision { finding_id: f.finding_id.clone(), category: f.category.clone(), action: *a, policy_id: None, policy_version: None, override_mode: OverrideMode::Allowed, approved: false, explanation: String::new() }).collect(),
            effective_policy_hash: String::new(),
            destination: DestinationRef { id: "x".into(), tier: DestinationTier::ExternalUnverified },
            direction: Direction::Inbound,
            severity: Sensitivity::Moderate,
            notify: true,
            explanations: vec![],
            used_approval: false,
        }
    }

    #[test]
    fn tokenize_restore_roundtrip() {
        let v = vault();
        let mut fields = vec!["mail jane@example.com and phone 555-010-1234".to_string()];
        let a = f(EMAIL, 5, 21);
        let b = f(PHONE, 32, 44);
        let d = decision(&[(&a, Action::Tokenize), (&b, Action::Redact)]);
        let r = apply(&mut fields, &[a, b], &d, &v, "conv1", Duration::hours(1)).unwrap();
        assert_eq!(r.tokenized, 1);
        assert_eq!(r.redacted, 1);
        assert!(!fields[0].contains("jane@example.com"));
        assert!(fields[0].contains("[REDACTED_PHONE]"));
        let (back, st) = restore(&fields[0], &v, "conv1").unwrap();
        assert!(back.contains("jane@example.com"));
        assert_eq!(st.restored, 1);
        let (other, st2) = restore(&fields[0], &v, "conv2").unwrap();
        assert!(!other.contains("jane@example.com"));
        assert_eq!(st2.unknown, 1);
    }

    #[test]
    fn overlap_strictest_and_union() {
        let v = vault();
        let mut fields = vec!["Contact Jane Nightjar-42 now".to_string()];
        let person = f(PERSON, 8, 21); // "Jane Nightjar"
        let mut secret = f(EXACT_SECRET, 13, 24); // "Nightjar-42"
        secret.secret_id = Some("s".into());
        let d = decision(&[(&person, Action::Tokenize), (&secret, Action::Redact)]);
        apply(&mut fields, &[person, secret], &d, &v, "c", Duration::hours(1)).unwrap();
        assert_eq!(fields[0], "Contact [REDACTED_EXACT_SECRET] now");
    }

    #[test]
    fn whole_field_transform_refused() {
        let v = vault();
        let mut fields = vec!["secret plans".to_string()];
        let mut c = f("CUSTOM:x", 0, 12);
        c.whole_field = true;
        let d = decision(&[(&c, Action::Tokenize)]);
        assert!(apply(&mut fields, &[c], &d, &v, "c", Duration::hours(1)).is_err());
    }

    #[test]
    fn generalize_values() {
        assert_eq!(generalize(EMAIL, "Jane@Example.com"), "[an email address at example.com]");
        assert_eq!(generalize(GOVERNMENT_ID, "x"), "[REDACTED_GOVERNMENT_ID]");
    }

    #[test]
    fn malformed_tokens_ignored() {
        let v = vault();
        let (o, st) = restore("[[PV_EMAIL_zzzz]] [[PV_EMAIL_1234567]] [PV_EMAIL_12345678]", &v, "c").unwrap();
        assert_eq!(st, RestoreStats::default());
        assert_eq!(o, "[[PV_EMAIL_zzzz]] [[PV_EMAIL_1234567]] [PV_EMAIL_12345678]");
    }

    #[test]
    fn ephemeral_store_scoped_and_zeroized_on_clear() {
        let st = EphemeralStore::new();
        let mut fields = vec!["mail jane@example.com".to_string()];
        let a = f(EMAIL, 5, 21);
        let d = decision(&[(&a, Action::Tokenize)]);
        apply(&mut fields, &[a], &d, &st, "tenant-a:conv-1", Duration::hours(1)).unwrap();
        assert!(fields[0].starts_with("mail [[ZFX_EMAIL_"));
        assert!(TOKEN_RE.is_match(&fields[0]));
        assert_eq!(restore(&fields[0], &st, "tenant-a:conv-1").unwrap().0, "mail jane@example.com");
        assert_eq!(restore(&fields[0], &st, "tenant-b:conv-1").unwrap().0, fields[0]);
        assert_eq!(restore(&fields[0], &st, "tenant-a:conv-2").unwrap().0, fields[0]);
        st.clear();
        assert!(st.is_empty());
        assert_eq!(restore(&fields[0], &st, "tenant-a:conv-1").unwrap().0, fields[0]);
    }

    #[test]
    fn legacy_pv_tokens_still_recognized() {
        assert!(TOKEN_RE.is_match("[[PV_EMAIL_bb25f99e]]"));
        assert!(TOKEN_RE.is_match("[[ZFX_EMAIL_0123456789abcdef]]"));
        assert!(!TOKEN_RE.is_match("[[ZFX_EMAIL_bb25f99e]]"));
        assert!(!TOKEN_RE.is_match("[[PV_EMAIL_0123456789abcdef]]"));
        assert!(is_token_prefix("[[ZFX_EMAIL_0123"));
        assert!(is_token_prefix("[[Z"));
        assert!(!is_token_prefix("[[ZX"));
        assert_eq!(safe_release_len("hi [[ZFX_EXACT_SECRET_0123456789ab"), 3);
    }

    #[test]
    fn token_prefix_holdback() {
        assert!(is_token_prefix("["));
        assert!(is_token_prefix("[[PV"));
        assert!(is_token_prefix("[[PV_EMAIL_ab12"));
        assert!(is_token_prefix("[[PV_EMAIL_ab12cd34]"));
        assert!(!is_token_prefix("[[PX"));
        assert!(!is_token_prefix("[x"));
        assert_eq!(safe_release_len("hello [[PV_EMA"), 6);
        assert_eq!(safe_release_len("hello [link]"), 12);
        assert_eq!(safe_release_len("a [[PV_EMAIL_ab12cd34]]"), 23);
    }
}
