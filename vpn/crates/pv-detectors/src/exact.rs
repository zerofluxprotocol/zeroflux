//! Matcher for owner-registered exact values, patterns and file fingerprints.
//!
//! Plaintext values exist only inside this trusted local process (decrypted from
//! the vault at load). Findings carry the opaque `secret_id`, never the value.

use crate::normalize::{normalize, view, NormOpts};
use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use pv_core::category::EXACT_SECRET;
use pv_core::{ClassifierType, Finding, Sensitivity};
use regex::Regex;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExactKind {
    /// A literal string, matched after the entry's normalizations.
    Value,
    /// An owner-supplied regular expression.
    Pattern,
    /// SHA-256 of a file's text (whitespace-normalized); matches whole fields or fenced blocks.
    FileFingerprint,
}

pub struct ExactEntry {
    pub secret_id: String,
    pub kind: ExactKind,
    pub value: Zeroizing<String>,
    pub norm: NormOpts,
    /// Allowlisted (whitelisted) values suppress noncritical findings instead of creating them.
    pub allow: bool,
}

pub const MIN_VALUE_LEN: usize = 4;
pub const MAX_PATTERN_LEN: usize = 512;

#[derive(Debug, thiserror::Error)]
pub enum ExactError {
    #[error("value must be at least {MIN_VALUE_LEN} characters after normalization")]
    TooShort,
    #[error("invalid pattern: {0}")]
    BadPattern(String),
    #[error("fingerprint must be 64 hex chars (sha256)")]
    BadFingerprint,
}

/// Validate an entry before it is stored.
pub fn validate_entry(kind: ExactKind, value: &str, norm: NormOpts) -> Result<(), ExactError> {
    match kind {
        ExactKind::Value => {
            if normalize(value, norm).chars().count() < MIN_VALUE_LEN {
                return Err(ExactError::TooShort);
            }
        }
        ExactKind::Pattern => {
            if value.len() > MAX_PATTERN_LEN {
                return Err(ExactError::BadPattern("too long".into()));
            }
            let r = regex::RegexBuilder::new(value).size_limit(1 << 20).build().map_err(|e| ExactError::BadPattern(e.to_string()))?;
            if r.is_match("") {
                return Err(ExactError::BadPattern("pattern matches empty string".into()));
            }
        }
        ExactKind::FileFingerprint => {
            if value.len() != 64 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(ExactError::BadFingerprint);
            }
        }
    }
    Ok(())
}

pub fn fingerprint_text(s: &str) -> String {
    let n = normalize(s, NormOpts { whitespace: true, ..Default::default() });
    hex::encode(Sha256::digest(n.as_bytes()))
}

struct Group {
    norm: NormOpts,
    ac: AhoCorasick,
    entry_idx: Vec<usize>,
}

#[derive(Default)]
pub struct ExactMatcher {
    entries: Vec<ExactEntry>,
    groups: Vec<Group>,
    patterns: Vec<(Regex, usize)>,
    fingerprints: Vec<(String, usize)>,
}

impl ExactMatcher {
    pub fn new(entries: Vec<ExactEntry>) -> Self {
        let mut m = ExactMatcher { entries, ..Default::default() };
        let mut by_norm: Vec<(NormOpts, Vec<usize>)> = Vec::new();
        for (i, e) in m.entries.iter().enumerate() {
            match e.kind {
                ExactKind::Value => match by_norm.iter_mut().find(|(n, _)| *n == e.norm) {
                    Some((_, v)) => v.push(i),
                    None => by_norm.push((e.norm, vec![i])),
                },
                ExactKind::Pattern => {
                    if let Ok(r) = regex::RegexBuilder::new(&e.value).size_limit(1 << 20).build() {
                        m.patterns.push((r, i));
                    }
                }
                ExactKind::FileFingerprint => m.fingerprints.push((e.value.to_ascii_lowercase(), i)),
            }
        }
        for (norm, idx) in by_norm {
            let pats: Vec<Zeroizing<String>> = idx.iter().map(|&i| Zeroizing::new(normalize(&m.entries[i].value, norm))).collect();
            let refs: Vec<&str> = pats.iter().map(|p| p.as_str()).collect();
            if let Ok(ac) = AhoCorasickBuilder::new().match_kind(MatchKind::LeftmostLongest).build(&refs) {
                m.groups.push(Group { norm, ac, entry_idx: idx });
            }
        }
        m
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Spans matched by entries; returns (start, end, entry index).
    fn raw_matches(&self, text: &str) -> Vec<(usize, usize, usize)> {
        let mut out = Vec::new();
        for g in &self.groups {
            let v = view(text, g.norm);
            for m in g.ac.find_iter(&v.text) {
                if let Some((s, e)) = v.map_span(m.start(), m.end()) {
                    out.push((s, e, g.entry_idx[m.pattern().as_usize()]));
                }
            }
        }
        for (r, i) in &self.patterns {
            for m in r.find_iter(text).take(1000) {
                if m.start() < m.end() {
                    out.push((m.start(), m.end(), *i));
                }
            }
        }
        if !self.fingerprints.is_empty() {
            let mut candidates = vec![(0usize, text.len())];
            candidates.extend(fenced_blocks(text));
            for (s, e) in candidates {
                let fp = fingerprint_text(&text[s..e]);
                for (want, i) in &self.fingerprints {
                    if &fp == want {
                        out.push((s, e, *i));
                    }
                }
            }
        }
        out
    }

    pub fn find(&self, text: &str) -> Vec<Finding> {
        self.raw_matches(text)
            .into_iter()
            .filter(|&(_, _, i)| !self.entries[i].allow)
            .map(|(s, e, i)| {
                let entry = &self.entries[i];
                let rid = match entry.kind {
                    ExactKind::Value => "exact-value-v1",
                    ExactKind::Pattern => "exact-pattern-v1",
                    ExactKind::FileFingerprint => "file-fingerprint-v1",
                };
                let mut f = Finding::new(ClassifierType::Exact, EXACT_SECRET, s, e, 1.0, rid, "registered value", Sensitivity::Critical);
                f.secret_id = Some(entry.secret_id.clone());
                f.classifier_version = "exact-v1".into();
                f
            })
            .collect()
    }

    /// Spans covered by allowlisted values.
    pub fn allowed_spans(&self, text: &str) -> Vec<(usize, usize)> {
        self.raw_matches(text).into_iter().filter(|&(_, _, i)| self.entries[i].allow).map(|(s, e, _)| (s, e)).collect()
    }
}

/// Byte ranges of the bodies of ``` fenced code blocks.
pub fn fenced_blocks(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut search = 0;
    while let Some(open) = text[search..].find("```") {
        let open = search + open;
        let Some(nl) = text[open..].find('\n') else { break };
        let body_start = open + nl + 1;
        let Some(close) = text[body_start..].find("```") else { break };
        out.push((body_start, body_start + close));
        search = body_start + close + 3;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: &str, v: &str, norm: NormOpts) -> ExactEntry {
        ExactEntry { secret_id: id.into(), kind: ExactKind::Value, value: Zeroizing::new(v.into()), norm, allow: false }
    }

    #[test]
    fn exact_and_normalized() {
        let m = ExactMatcher::new(vec![
            e("s1", "Project Nightjar", NormOpts { casefold: true, whitespace: true, ..Default::default() }),
            e("s2", "ACCT-99-1234", NormOpts { separators: true, casefold: true, ..Default::default() }),
            e("s3", "CaseSensitive", NormOpts::default()),
        ]);
        let t = "about project   NIGHTJAR and acct 99 1234 but not casesensitive";
        let f = m.find(t);
        let ids: Vec<_> = f.iter().map(|f| f.secret_id.clone().unwrap()).collect();
        assert!(ids.contains(&"s1".to_string()));
        assert!(ids.contains(&"s2".to_string()));
        assert!(!ids.contains(&"s3".to_string()));
        let s1 = f.iter().find(|f| f.secret_id.as_deref() == Some("s1")).unwrap();
        assert_eq!(&t[s1.start_byte..s1.end_byte], "project   NIGHTJAR");
        assert!(f.iter().all(|f| f.evidence == "registered value"));
    }

    #[test]
    fn unicode_evasion() {
        let m = ExactMatcher::new(vec![e("s1", "nightjar", NormOpts { unicode: true, casefold: true, ..Default::default() })]);
        assert_eq!(m.find("NІGHT\u{200B}JАR").len(), 1); // Cyrillic I and A, zero-width space
    }

    #[test]
    fn patterns_and_fingerprints() {
        let mut p = e("p1", r"PRJ-\d{4}", NormOpts::default());
        p.kind = ExactKind::Pattern;
        let body = "fn secret() {\n  42\n}";
        let mut fp = e("f1", &fingerprint_text(body), NormOpts::default());
        fp.kind = ExactKind::FileFingerprint;
        let m = ExactMatcher::new(vec![p, fp]);
        assert_eq!(m.find("ticket PRJ-1234").len(), 1);
        let t = format!("look:\n```rust\n{body}\n```\n");
        assert_eq!(m.find(&t).len(), 1);
    }

    #[test]
    fn validation() {
        assert!(validate_entry(ExactKind::Value, "abc", NormOpts::default()).is_err());
        assert!(validate_entry(ExactKind::Pattern, ".*", NormOpts::default()).is_err());
        assert!(validate_entry(ExactKind::Pattern, "(", NormOpts::default()).is_err());
        assert!(validate_entry(ExactKind::FileFingerprint, "zz", NormOpts::default()).is_err());
    }

    #[test]
    fn allowlist_not_reported() {
        let mut a = e("a1", "support@acme.test", NormOpts::default());
        a.allow = true;
        let m = ExactMatcher::new(vec![a]);
        assert!(m.find("mail support@acme.test").is_empty());
        assert_eq!(m.allowed_spans("mail support@acme.test").len(), 1);
    }
}
