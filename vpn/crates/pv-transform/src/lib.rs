//! Transformations (tokenize, redact, generalize) and placeholder restoration.

use chrono::Duration;
use pv_core::category::*;
use pv_core::{overlap_groups, Action, Decision, Finding};
use pv_vault::{token_label, Vault};
use regex::Regex;
use std::sync::LazyLock;

pub static TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\[PV_[A-Z0-9_]{1,40}_[0-9a-f]{8}\]\]").unwrap());
/// Longest possible placeholder, used to size streaming hold-back.
pub const MAX_TOKEN_LEN: usize = 4 + 1 + 40 + 1 + 8 + 2 + 2;

#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    #[error("vault: {0}")]
    Vault(#[from] pv_vault::VaultError),
    #[error("finding {0} cannot be isolated to a span; refusing partial transformation")]
    NotIsolatable(String),
    #[error("finding offsets invalid")]
    BadOffsets,
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
pub fn apply(fields: &mut [String], findings: &[Finding], decision: &Decision, vault: &Vault, scope: &str, ttl: Duration) -> Result<TransformReport, TransformError> {
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
pub fn restore(text: &str, vault: &Vault, scope: &str) -> Result<(String, RestoreStats), TransformError> {
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

/// Whether `s` could be the beginning of a placeholder token.
pub fn is_token_prefix(s: &str) -> bool {
    const HEAD: &str = "[[PV_";
    if s.len() <= HEAD.len() {
        return HEAD.starts_with(s);
    }
    if !s.starts_with(HEAD) {
        return false;
    }
    let rest = &s[HEAD.len()..];
    // body: [A-Z0-9_]{1,40} then '_' + up to 8 hex + "]]"
    let mut chars = rest.char_indices().peekable();
    let mut n = 0;
    while let Some(&(_, c)) = chars.peek() {
        if c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' {
            n += 1;
            chars.next();
        } else {
            break;
        }
    }
    if n > 50 {
        return false;
    }
    let tail: String = chars.map(|(_, c)| c).collect();
    if tail.is_empty() {
        return true;
    }
    // tail may be hex digits (if the label's last '_' was consumed) then ']]'
    let t = tail.trim_start_matches(|c: char| c.is_ascii_hexdigit() && c.is_ascii_lowercase() || c.is_ascii_digit());
    matches!(t, "" | "]" | "]]") && t.len() < tail.len() + 1
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
