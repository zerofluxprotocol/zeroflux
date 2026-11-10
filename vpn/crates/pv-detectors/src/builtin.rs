//! Deterministic built-in recognizers: regex, checksums and context cues.
//!
//! Recognizer IDs carry a version suffix; changing behavior bumps the suffix so
//! audit events record exactly which logic produced a finding.

use pv_core::category::*;
use pv_core::{ClassifierType, Finding};
use regex::Regex;
use std::sync::LazyLock;

pub const VERSION: &str = "builtin-v1";

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid builtin regex")
}

macro_rules! lazy_re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| re($pat));
    };
}

lazy_re!(EMAIL_RE, r"(?i)\b[a-z0-9._%+\-]+@[a-z0-9](?:[a-z0-9\-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9\-]*[a-z0-9])?)*\.[a-z]{2,24}\b");
lazy_re!(SSN_RE, r"\b(\d{3})[- ](\d{2})[- ](\d{4})\b");
lazy_re!(NINE_DIGITS_RE, r"\b\d{9}\b");
lazy_re!(EIN_RE, r"\b\d{2}-\d{7}\b");
lazy_re!(NINO_RE, r"\b[A-CEGHJ-PR-TW-Z][A-CEGHJ-NPR-TW-Z] ?\d{2} ?\d{2} ?\d{2} ?[A-D]\b");
lazy_re!(CARD_RE, r"\b\d(?:[ \-]?\d){12,18}\b");
lazy_re!(NANP_RE, r"(?:\+1[\s.\-]?)?(?:\(\d{3}\)\s?|\b\d{3}[\s.\-])\d{3}[\s.\-]\d{4}\b");
lazy_re!(INTL_PHONE_RE, r"\+[1-9]\d{0,2}(?:[\s.\-]?\(?\d{1,4}\)?){2,5}\b");
lazy_re!(OPENAI_RE, r"\bsk-(?:proj-|svcacct-|admin-|ant-(?:api\d{2}-|admin\d{2}-)?)?[A-Za-z0-9_\-]{20,}");
lazy_re!(AWS_ID_RE, r"\b(?:AKIA|ASIA|ABIA|ACCA|AROA|AIDA)[0-9A-Z]{16}\b");
lazy_re!(AWS_SECRET_RE, r#"(?i)aws.{0,20}(?:secret|private).{0,20}?[:=]\s*["']?([A-Za-z0-9/+=]{40})\b"#);
lazy_re!(GITHUB_RE, r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{50,})\b");
lazy_re!(SLACK_RE, r"\bxox[abposr]-[A-Za-z0-9\-]{10,}");
lazy_re!(GOOGLE_RE, r"\bAIza[0-9A-Za-z_\-]{35}\b");
lazy_re!(STRIPE_RE, r"\b(?:sk|rk|pk|whsec)_(?:live|test)_[A-Za-z0-9]{16,}\b");
lazy_re!(GITLAB_RE, r"\bglpat-[A-Za-z0-9_\-]{20,}\b");
lazy_re!(HF_RE, r"\bhf_[A-Za-z0-9]{30,}\b");
lazy_re!(NPM_RE, r"\bnpm_[A-Za-z0-9]{36}\b");
lazy_re!(SENDGRID_RE, r"\bSG\.[A-Za-z0-9_\-]{16,}\.[A-Za-z0-9_\-]{16,}");
lazy_re!(TWILIO_RE, r"\bSK[0-9a-f]{32}\b");
lazy_re!(NEAR_KEY_RE, r"\bed25519:[1-9A-HJ-NP-Za-km-z]{60,90}\b");
lazy_re!(GENERIC_KEY_RE, r#"(?i)\b[a-z0-9_.\-]*(?:api[_\-]?key|apikey|access[_\-]?token|auth[_\-]?token|private[_\-]?token|x-api-key|_key|_token|secret|credentials?)\b["']?\s*[:=]\s*["']?([A-Za-z0-9_\-./+=]{16,})"#);
lazy_re!(SSN_DOT_RE, r"\b(\d{3})\.(\d{2})\.(\d{4})\b");
lazy_re!(PASSWORD_LATE_RE, r#"(?i)\bpass(?:word|phrase)\b[^\n:=]{1,40}[:=]\s*["']?([^\s"',;]{6,64})"#);
lazy_re!(OBFUSCATED_EMAIL_RE, r"(?i)\b[a-z0-9._\-]+(?:\s+dot\s+[a-z0-9_\-]+)*\s+(?:at|\[at\]|\(at\))\s+[a-z0-9\-]+(?:\s+dot\s+[a-z0-9\-]+)+\b");
lazy_re!(PEM_RE, r"-----BEGIN ((?:[A-Z0-9]+ )*)PRIVATE KEY(?: BLOCK)?-----(?s:.*?)(?:-----END (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----|\z)");
lazy_re!(HEX_KEY_RE, r"(?i)\b(?:private[_ \-]?key|priv[_ \-]?key|secret[_ \-]?key|signing[_ \-]?key)\b\W{0,6}((?:0x)?[0-9a-f]{64})\b");
lazy_re!(PASSWORD_RE, r#"(?i)\b(?:pass(?:word|wd|phrase)?|pwd)\b["']?\s*(?:[:=]|\s+is\s+)\s*["']?([^\s"',;]{4,128})"#);
lazy_re!(BEARER_RE, r"(?i)\bbearer\s+([A-Za-z0-9\-._~+/]{16,}=*)");
lazy_re!(BASIC_RE, r"(?i)\bauthorization:\s*basic\s+([A-Za-z0-9+/]{8,}={0,2})");
lazy_re!(JWT_RE, r"\beyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}");
lazy_re!(URL_CRED_RE, r"\b[a-zA-Z][a-zA-Z0-9+.\-]*://[^\s:/@]+:([^\s@/]+)@");
lazy_re!(IBAN_RE, r"\b[A-Z]{2}\d{2}(?: ?[A-Z0-9]{4}){2,7}(?: ?[A-Z0-9]{1,4})?\b");
lazy_re!(ADDRESS_RE, r"\b\d{1,6}\s+(?:[NSEW]\.?\s+)?(?:[A-Z][a-z]+\.?\s+){1,4}(?:Street|St|Avenue|Ave|Road|Rd|Boulevard|Blvd|Lane|Ln|Drive|Dr|Court|Ct|Way|Place|Pl|Terrace|Ter|Circle|Cir|Parkway|Pkwy|Highway|Hwy|Square|Sq)\b\.?(?:,?\s+(?:Apt|Apartment|Suite|Ste|Unit|#)\.?\s*[A-Za-z0-9\-]+)?(?:,\s*[A-Z][a-z]+(?:\s[A-Z][a-z]+){0,2})?(?:,\s*[A-Z]{2})?(?:\s+\d{5}(?:-\d{4})?)?");
lazy_re!(PERSON_CUE_RE, r"(?:(?i:my name is|name\s*:|full name\s*:|dear|patient\s*:?|customer\s*:?|employee\s*:?|contact\s*:|signed,|regards,)|\b(?:Mr|Mrs|Ms|Dr|Prof)\.?)\s+([A-Z][a-z]+(?:[ \-][A-Z][a-z'\-]+){0,2})");
lazy_re!(DATE_RE, r"(?i)\b(?:\d{4}-\d{2}-\d{2}|\d{1,2}[/.\-]\d{1,2}[/.\-]\d{2,4}|(?:jan|feb|mar|apr|may|jun|jul|aug|sep|sept|oct|nov|dec)[a-z]*\.?\s+\d{1,2}(?:st|nd|rd|th)?,?\s+\d{4}|\d{1,2}(?:st|nd|rd|th)?\s+(?:jan|feb|mar|apr|may|jun|jul|aug|sep|sept|oct|nov|dec)[a-z]*\.?,?\s+\d{4})\b");
lazy_re!(DOB_CUE_RE, r"(?i)\b(?:dob|d\.o\.b\.?|date of birth|birth ?date|birthday|born(?: on)?)\b");
lazy_re!(PASSPORT_CUE_RE, r"(?i)\b(?:passport|travel\s+doc(?:ument)?)(?:\s*(?:no|number|num|#))?\.?");
lazy_re!(PASSPORT_VAL_RE, r"\b[A-Z0-9]{6,9}\b");
lazy_re!(DL_CUE_RE, r"(?i)\b(?:driver'?s?\s+licen[cs]e(?:\s*(?:no|number|#))?|DL\s*(?:#|no\.?|number)|licen[cs]e\s+(?:no|number|#))\.?");
lazy_re!(DL_VAL_RE, r"\b[A-Z]{0,3}[\- ]?\d{4,12}\b");
lazy_re!(ACCT_CUE_RE, r"(?i)\b(?:account\s*(?:no|number|num|#)|acct\.?\s*(?:no|#)?|bank account|routing(?:\s*(?:no|number|#))?|aba)\b\.?");
lazy_re!(ACCT_VAL_RE, r"\b\d{6,17}\b");
lazy_re!(SSN_CUE_RE, r"(?i)\b(?:ssn|social security|soc\.? sec|tax id|tin|itin|ein|social insurance|sin)\b");
lazy_re!(CARD_CUE_RE, r"(?i)\b(?:card|visa|mastercard|amex|discover|cc|credit|debit|cvv|pan)\b");
lazy_re!(PHONE_CUE_RE, r"(?i)\b(?:phone|tel|mobile|cell|call|fax|whatsapp|sms|text me)\b");
lazy_re!(HEALTH_RE, r"(?i)\b(?:diagnosed with|diagnosis\s*:|diagnosis of|prescribed|suffers from|tested positive for|positive for|treatment for|medical history of|history of|on medication)\s+([^.\n;]{3,80})");
lazy_re!(HEALTH_TERM_RE, r"(?i)\b(?:HIV|AIDS|hepatitis [ABC]|chemo(?:therapy)?|schizophrenia|bipolar disorder|major depressive disorder|type [12] diabetes|cancer|tuberculosis|pregnan(?:t|cy)|insulin|antidepressants?|opioid use disorder)\b");
lazy_re!(MRN_RE, r"(?i)\b(?:MRN|medical record (?:no|number|#))\s*[:#]?\s*([A-Z0-9\-]{6,14})\b");
lazy_re!(HEALTH_PERSON_CUE_RE, r"(?i)\b(?:patient|my|his|her|their|he|she|i)\b");

/// Window of `n` bytes before `pos`, on a char boundary.
fn before(text: &str, pos: usize, n: usize) -> &str {
    let mut s = pos.saturating_sub(n);
    while !text.is_char_boundary(s) {
        s += 1;
    }
    &text[s..pos]
}

fn after(text: &str, pos: usize, n: usize) -> (usize, &str) {
    let mut e = (pos + n).min(text.len());
    while !text.is_char_boundary(e) {
        e -= 1;
    }
    (pos, &text[pos..e])
}

pub fn luhn_valid(digits: &str) -> bool {
    let mut sum = 0u32;
    let mut alt = false;
    let mut n = 0;
    for c in digits.chars().rev() {
        let Some(mut d) = c.to_digit(10) else { return false };
        n += 1;
        if alt {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
        alt = !alt;
    }
    n >= 2 && sum.is_multiple_of(10)
}

pub fn ssn_valid(area: &str, group: &str, serial: &str) -> bool {
    let a: u32 = area.parse().unwrap_or(0);
    a != 0 && a != 666 && a < 900 && group != "00" && serial != "0000"
}

pub fn iban_valid(iban: &str) -> bool {
    let s: String = iban.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() < 15 || s.len() > 34 {
        return false;
    }
    let rearranged = format!("{}{}", &s[4..], &s[..4]);
    let mut rem: u64 = 0;
    for c in rearranged.chars() {
        let v = if c.is_ascii_digit() {
            c.to_digit(10).unwrap() as u64
        } else if c.is_ascii_uppercase() {
            (c as u64) - ('A' as u64) + 10
        } else {
            return false;
        };
        rem = if v >= 10 { (rem * 100 + v) % 97 } else { (rem * 10 + v) % 97 };
    }
    rem == 1
}

pub fn aba_valid(d: &str) -> bool {
    if d.len() != 9 {
        return false;
    }
    let v: Vec<u32> = d.chars().filter_map(|c| c.to_digit(10)).collect();
    if v.len() != 9 {
        return false;
    }
    let s = 3 * (v[0] + v[3] + v[6]) + 7 * (v[1] + v[4] + v[7]) + (v[2] + v[5] + v[8]);
    s.is_multiple_of(10) && s != 0
}

fn card_prefix_known(d: &str) -> bool {
    let p2: u32 = d.get(..2).and_then(|x| x.parse().ok()).unwrap_or(0);
    let p4: u32 = d.get(..4).and_then(|x| x.parse().ok()).unwrap_or(0);
    d.starts_with('4')
        || (51..=55).contains(&p2)
        || (2221..=2720).contains(&p4)
        || p2 == 34
        || p2 == 37
        || d.starts_with("6011")
        || p2 == 65
        || (3528..=3589).contains(&p4)
        || p2 == 36
        || p2 == 38
        || d.starts_with("30")
        || p2 == 62
}

/// Shannon entropy in bits per char.
pub fn entropy(s: &str) -> f64 {
    let mut counts = std::collections::HashMap::new();
    let n = s.chars().count() as f64;
    if n == 0.0 {
        return 0.0;
    }
    for c in s.chars() {
        *counts.entry(c).or_insert(0usize) += 1;
    }
    counts.values().map(|&k| {
        let p = k as f64 / n;
        -p * p.log2()
    }).sum()
}

fn looks_placeholder(v: &str) -> bool {
    let l = v.to_ascii_lowercase();
    ["your", "example", "xxxx", "<", "placeholder", "changeme", "redacted", "dummy", "process.env", "os.environ", "${", "{{"]
        .iter()
        .any(|p| l.contains(p))
        || v.chars().all(|c| c == v.chars().next().unwrap_or('x'))
}

struct Out<'a> {
    text: &'a str,
    findings: Vec<Finding>,
}

impl Out<'_> {
    fn push(&mut self, cat: &str, s: usize, e: usize, conf: f32, rid: &str, evidence: &str) {
        if s >= e || e > self.text.len() {
            return;
        }
        let mut f = Finding::new(ClassifierType::Builtin, cat, s, e, conf.clamp(0.0, 1.0), rid, evidence, sensitivity_of(cat));
        f.classifier_version = VERSION.into();
        self.findings.push(f);
    }
}

/// Run every built-in recognizer over `text`.
pub fn detect(text: &str) -> Vec<Finding> {
    let mut o = Out { text, findings: Vec::new() };

    for m in EMAIL_RE.find_iter(text) {
        o.push(EMAIL, m.start(), m.end(), 0.97, "email-v1", "pattern");
    }

    // Government IDs
    for c in SSN_RE.captures_iter(text) {
        let m = c.get(0).unwrap();
        let ctx = SSN_CUE_RE.is_match(before(text, m.start(), 40));
        if ssn_valid(&c[1], &c[2], &c[3]) {
            o.push(GOVERNMENT_ID, m.start(), m.end(), if ctx { 0.99 } else { 0.93 }, "ssn-us-v1", if ctx { "pattern+context" } else { "pattern+valid-ranges" });
        } else {
            // Invalid ranges are still treated as sensitive, just with lower confidence.
            o.push(GOVERNMENT_ID, m.start(), m.end(), if ctx { 0.9 } else { 0.72 }, "ssn-us-v1", "pattern-invalid-ranges");
        }
    }
    for c in SSN_DOT_RE.captures_iter(text) {
        let m = c.get(0).unwrap();
        if SSN_CUE_RE.is_match(before(text, m.start(), 40)) {
            o.push(GOVERNMENT_ID, m.start(), m.end(), if ssn_valid(&c[1], &c[2], &c[3]) { 0.95 } else { 0.85 }, "ssn-us-dotted-v1", "pattern+context");
        }
    }
    for m in NINE_DIGITS_RE.find_iter(text) {
        let pre = before(text, m.start(), 40);
        if SSN_CUE_RE.is_match(pre) {
            let d = m.as_str();
            let ok = ssn_valid(&d[..3], &d[3..5], &d[5..]);
            o.push(GOVERNMENT_ID, m.start(), m.end(), if ok { 0.95 } else { 0.85 }, "ssn-us-undelimited-v1", "pattern+context");
        }
    }
    for m in EIN_RE.find_iter(text) {
        if SSN_CUE_RE.is_match(before(text, m.start(), 40)) {
            o.push(GOVERNMENT_ID, m.start(), m.end(), 0.9, "ein-us-v1", "pattern+context");
        }
    }
    for m in NINO_RE.find_iter(text) {
        o.push(GOVERNMENT_ID, m.start(), m.end(), 0.8, "nino-uk-v1", "pattern");
    }

    // Payment cards
    for m in CARD_RE.find_iter(text) {
        let digits: String = m.as_str().chars().filter(|c| c.is_ascii_digit()).collect();
        if !(13..=19).contains(&digits.len()) {
            continue;
        }
        let ctx = CARD_CUE_RE.is_match(before(text, m.start(), 40));
        let luhn = luhn_valid(&digits);
        let conf = match (luhn, card_prefix_known(&digits), ctx) {
            (true, true, _) => 0.97,
            (true, false, true) => 0.85,
            (true, false, false) => 0.55,
            (false, _, true) => 0.6,
            (false, _, false) => continue,
        };
        o.push(PAYMENT_CARD, m.start(), m.end(), conf, "payment-card-v1", if luhn { "pattern+luhn" } else { "pattern+context-invalid-luhn" });
    }

    // Phones
    for m in NANP_RE.find_iter(text) {
        let ctx = PHONE_CUE_RE.is_match(before(text, m.start(), 30));
        o.push(PHONE, m.start(), m.end(), if ctx { 0.95 } else { 0.82 }, "phone-nanp-v1", "pattern");
    }
    for m in INTL_PHONE_RE.find_iter(text) {
        let digits = m.as_str().chars().filter(|c| c.is_ascii_digit()).count();
        if (8..=15).contains(&digits) {
            o.push(PHONE, m.start(), m.end(), 0.8, "phone-e164-v1", "pattern");
        }
    }

    // API keys and credentials
    let keyed: [(&LazyLock<Regex>, &str); 13] = [
        (&OPENAI_RE, "openai-anthropic-key-v1"),
        (&AWS_ID_RE, "aws-access-key-id-v1"),
        (&GITHUB_RE, "github-token-v1"),
        (&SLACK_RE, "slack-token-v1"),
        (&GOOGLE_RE, "google-api-key-v1"),
        (&STRIPE_RE, "stripe-key-v1"),
        (&GITLAB_RE, "gitlab-token-v1"),
        (&HF_RE, "huggingface-token-v1"),
        (&NPM_RE, "npm-token-v1"),
        (&SENDGRID_RE, "sendgrid-key-v1"),
        (&TWILIO_RE, "twilio-key-v1"),
        (&NEAR_KEY_RE, "near-ed25519-key-v1"),
        (&JWT_RE, "jwt-v1"),
    ];
    for (r, id) in keyed {
        for m in r.find_iter(text) {
            let cat = if id == "jwt-v1" { AUTH_SECRET } else if id == "near-ed25519-key-v1" { PRIVATE_KEY } else { API_KEY };
            o.push(cat, m.start(), m.end(), 0.97, id, "pattern");
        }
    }
    for c in AWS_SECRET_RE.captures_iter(text) {
        let g = c.get(1).unwrap();
        o.push(API_KEY, g.start(), g.end(), 0.93, "aws-secret-key-v1", "pattern+context");
    }
    for c in GENERIC_KEY_RE.captures_iter(text) {
        let g = c.get(1).unwrap();
        let v = g.as_str();
        if looks_placeholder(v) || entropy(v) < 3.3 || !v.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        let name = c.get(0).unwrap().as_str().to_ascii_lowercase();
        let cat = if name.contains("secret") || name.contains("credential") { AUTH_SECRET } else { API_KEY };
        o.push(cat, g.start(), g.end(), 0.85, "generic-keyed-secret-v2", "keyword+entropy");
    }
    for m in OBFUSCATED_EMAIL_RE.find_iter(text) {
        o.push(EMAIL, m.start(), m.end(), 0.75, "email-obfuscated-v1", "pattern");
    }
    for c in PEM_RE.captures_iter(text) {
        let m = c.get(0).unwrap();
        let terminated = m.as_str().contains("-----END");
        o.push(PRIVATE_KEY, m.start(), m.end(), if terminated { 0.99 } else { 0.9 }, "pem-private-key-v1", if terminated { "pattern" } else { "pattern-unterminated" });
    }
    for c in HEX_KEY_RE.captures_iter(text) {
        let g = c.get(1).unwrap();
        o.push(PRIVATE_KEY, g.start(), g.end(), 0.9, "hex-private-key-v1", "pattern+context");
    }
    for c in PASSWORD_RE.captures_iter(text) {
        let g = c.get(1).unwrap();
        if looks_placeholder(g.as_str()) {
            continue;
        }
        o.push(AUTH_SECRET, g.start(), g.end(), 0.88, "password-assignment-v1", "keyword");
    }
    for c in PASSWORD_LATE_RE.captures_iter(text) {
        let g = c.get(1).unwrap();
        let v = g.as_str();
        let mixed = v.chars().any(|c| c.is_ascii_digit()) && v.chars().any(|c| c.is_alphabetic());
        if looks_placeholder(v) || !mixed {
            continue;
        }
        o.push(AUTH_SECRET, g.start(), g.end(), 0.8, "password-late-v1", "keyword");
    }
    for c in BEARER_RE.captures_iter(text) {
        let g = c.get(1).unwrap();
        if looks_placeholder(g.as_str()) {
            continue;
        }
        o.push(AUTH_SECRET, g.start(), g.end(), 0.9, "bearer-token-v1", "keyword");
    }
    for c in BASIC_RE.captures_iter(text) {
        let g = c.get(1).unwrap();
        o.push(AUTH_SECRET, g.start(), g.end(), 0.9, "basic-auth-v1", "keyword");
    }
    for c in URL_CRED_RE.captures_iter(text) {
        let g = c.get(1).unwrap();
        if looks_placeholder(g.as_str()) {
            continue;
        }
        o.push(AUTH_SECRET, g.start(), g.end(), 0.95, "url-credentials-v1", "pattern");
    }

    // Context-anchored identifiers
    for m in PASSPORT_CUE_RE.find_iter(text) {
        let (base, win) = after(text, m.end(), 30);
        if let Some(v) = PASSPORT_VAL_RE.find(win) {
            if v.as_str().chars().any(|c| c.is_ascii_digit()) {
                o.push(PASSPORT, base + v.start(), base + v.end(), 0.88, "passport-v1", "pattern+context");
            }
        }
    }
    for m in DL_CUE_RE.find_iter(text) {
        let (base, win) = after(text, m.end(), 30);
        if let Some(v) = DL_VAL_RE.find(win) {
            let s = v.as_str().trim();
            let off = v.as_str().len() - v.as_str().trim_start().len();
            o.push(DRIVER_LICENSE, base + v.start() + off, base + v.start() + off + s.len(), 0.85, "driver-license-v1", "pattern+context");
        }
    }
    for m in IBAN_RE.find_iter(text) {
        if iban_valid(m.as_str()) {
            o.push(FINANCIAL_ACCOUNT, m.start(), m.end(), 0.95, "iban-v1", "pattern+mod97");
        }
    }
    for m in ACCT_CUE_RE.find_iter(text) {
        let (base, win) = after(text, m.end(), 30);
        if let Some(v) = ACCT_VAL_RE.find(win) {
            let routing = m.as_str().to_ascii_lowercase();
            let is_routing = routing.contains("routing") || routing.contains("aba");
            let conf = if is_routing && aba_valid(v.as_str()) { 0.93 } else { 0.82 };
            o.push(FINANCIAL_ACCOUNT, base + v.start(), base + v.end(), conf, if is_routing { "aba-routing-v1" } else { "bank-account-v1" }, "pattern+context");
        }
    }
    for m in DATE_RE.find_iter(text) {
        if DOB_CUE_RE.is_match(before(text, m.start(), 30)) {
            o.push(DOB, m.start(), m.end(), 0.9, "dob-v1", "date+context");
        }
    }
    for m in ADDRESS_RE.find_iter(text) {
        o.push(ADDRESS, m.start(), m.end(), 0.8, "street-address-us-v1", "pattern");
    }
    for c in PERSON_CUE_RE.captures_iter(text) {
        let g = c.get(1).unwrap();
        o.push(PERSON, g.start(), g.end(), 0.7, "person-cue-v1", "context");
    }
    for c in HEALTH_RE.captures_iter(text) {
        let g = c.get(1).unwrap();
        let s = g.as_str().trim_end();
        o.push(HEALTH_INFORMATION, g.start(), g.start() + s.len(), 0.75, "health-cue-v1", "context");
    }
    for m in HEALTH_TERM_RE.find_iter(text) {
        // Condition terms only count when tied to a person.
        if HEALTH_PERSON_CUE_RE.is_match(before(text, m.start(), 60)) {
            o.push(HEALTH_INFORMATION, m.start(), m.end(), 0.6, "health-term-v1", "lexicon+person-context");
        }
    }
    for c in MRN_RE.captures_iter(text) {
        let g = c.get(1).unwrap();
        o.push(HEALTH_INFORMATION, g.start(), g.end(), 0.9, "mrn-v1", "pattern+context");
    }

    dedupe(o.findings)
}

/// Remove exact duplicates (same span and category), keeping the most confident.
pub fn dedupe(mut v: Vec<Finding>) -> Vec<Finding> {
    v.sort_by(|a, b| {
        (a.field_index, a.start_byte, a.end_byte, &a.category)
            .cmp(&(b.field_index, b.start_byte, b.end_byte, &b.category))
            .then(b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal))
    });
    v.dedup_by(|b, a| a.field_index == b.field_index && a.start_byte == b.start_byte && a.end_byte == b.end_byte && a.category == b.category);
    // Drop a phone finding fully inside a payment card / gov ID / key finding; keep everything else.
    let strong: Vec<(usize, usize, usize)> = v
        .iter()
        .filter(|f| matches!(f.category.as_str(), PAYMENT_CARD | GOVERNMENT_ID | API_KEY | PRIVATE_KEY | AUTH_SECRET | FINANCIAL_ACCOUNT))
        .map(|f| (f.field_index, f.start_byte, f.end_byte))
        .collect();
    v.retain(|f| {
        f.category != PHONE || !strong.iter().any(|&(fi, s, e)| fi == f.field_index && s <= f.start_byte && f.end_byte <= e)
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cats(t: &str) -> Vec<String> {
        detect(t).into_iter().map(|f| f.category).collect()
    }
    fn has(t: &str, c: &str) -> bool {
        cats(t).iter().any(|x| x == c)
    }
    fn span<'a>(t: &'a str, c: &str) -> &'a str {
        let f = detect(t).into_iter().find(|f| f.category == c).unwrap();
        &t[f.start_byte..f.end_byte]
    }

    #[test]
    fn checksums() {
        assert!(luhn_valid("4111111111111111"));
        assert!(!luhn_valid("4111111111111112"));
        assert!(iban_valid("GB82WEST12345698765432"));
        assert!(iban_valid("GB82 WEST 1234 5698 7654 32"));
        assert!(!iban_valid("GB82WEST12345698765433"));
        assert!(aba_valid("011000015"));
        assert!(ssn_valid("123", "45", "6789"));
        assert!(!ssn_valid("666", "45", "6789"));
        assert!(!ssn_valid("900", "45", "6789"));
    }

    #[test]
    fn email_phone() {
        assert_eq!(span("mail jane.doe+x@example.co.uk now", EMAIL), "jane.doe+x@example.co.uk");
        assert_eq!(span("call (555) 010-1234", PHONE), "(555) 010-1234");
        assert!(has("reach me at +44 20 7946 0958", PHONE));
        assert!(!has("order 5550101234 shipped", PHONE));
    }

    #[test]
    fn ssn_variants() {
        assert_eq!(span("SSN: 123-45-6789", GOVERNMENT_ID), "123-45-6789");
        // invalid area still flagged
        let f = detect("id 000-12-3456").into_iter().find(|f| f.category == GOVERNMENT_ID).unwrap();
        assert!(f.confidence < 0.8);
        assert!(has("my social security number is 123456789", GOVERNMENT_ID));
        assert!(!has("tracking 123456789", GOVERNMENT_ID));
    }

    #[test]
    fn cards() {
        assert_eq!(span("card 4111 1111 1111 1111 exp", PAYMENT_CARD), "4111 1111 1111 1111");
        assert!(!has("value 4111111111111112 here", PAYMENT_CARD));
        assert!(has("visa 4111111111111112", PAYMENT_CARD));
    }

    #[test]
    fn keys() {
        assert!(has("OPENAI_API_KEY=sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz012345", API_KEY));
        assert!(has("AKIAIOSFODNN7EXAMPLE", API_KEY));
        assert!(has("token ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789", API_KEY));
        assert!(has("api_key: \"q8Zr3LmPx9Tv2Wn7Ks4Yb6Hd\"", API_KEY));
        assert!(!has("api_key: \"your_api_key_here_xxxx\"", API_KEY));
        assert!(has("-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\n-----END RSA PRIVATE KEY-----", PRIVATE_KEY));
        assert!(has("-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk", PRIVATE_KEY));
        assert_eq!(span("password=hunter2!", AUTH_SECRET), "hunter2!");
        assert_eq!(span("postgres://admin:s3cretP@db:5432/x", AUTH_SECRET), "s3cretP");
        assert!(has("Authorization: Bearer abcdefghijklmnop1234", AUTH_SECRET));
        assert!(has("eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U", AUTH_SECRET));
    }

    #[test]
    fn context_ids() {
        assert_eq!(span("Passport No. X1234567 issued", PASSPORT), "X1234567");
        assert!(has("driver's license number D1234567", DRIVER_LICENSE));
        assert!(has("DOB: 1984-03-12", DOB));
        assert!(!has("released 1984-03-12", DOB));
        assert!(has("IBAN GB82 WEST 1234 5698 7654 32", FINANCIAL_ACCOUNT));
        assert!(has("account number 12345678", FINANCIAL_ACCOUNT));
    }

    #[test]
    fn address_person_health() {
        assert!(has("ship to 221 Baker Street, London", ADDRESS));
        assert!(has("1600 Pennsylvania Avenue, Washington, DC 20500", ADDRESS));
        assert_eq!(span("Hi, my name is Jane Doe and", PERSON), "Jane Doe");
        assert!(has("The patient was diagnosed with type 2 diabetes last year", HEALTH_INFORMATION));
        assert!(!has("Diabetes research funding rose 10%", HEALTH_INFORMATION));
    }

    #[test]
    fn broadened_patterns() {
        assert!(has("Her SSN is 219.09.9999 per the form.", GOVERNMENT_ID));
        assert!(!has("version 219.09.9999 released", GOVERNMENT_ID));
        assert_eq!(span("AZURE_OPENAI_KEY=4f9c2b7e1a8d4c3f9e6b5a7d2c1e8f90", API_KEY), "4f9c2b7e1a8d4c3f9e6b5a7d2c1e8f90");
        assert_eq!(span("{\"secret\": \"Zq8vT3xLp0Wm9Rk2Ys7N\"}", AUTH_SECRET), "Zq8vT3xLp0Wm9Rk2Ys7N");
        assert!(!has("\"max_tokens\": 4096, \"page_token\": \"next\"", API_KEY));
        assert_eq!(span("my password is the same as before: Tr0ub4dor&3", AUTH_SECRET), "Tr0ub4dor&3");
        assert!(has("Travel doc 548927615 (USA)", PASSPORT));
        assert!(has("write to jane dot roe at example dot com", EMAIL));
        assert!(!has("look at the dog", EMAIL));
    }

    #[test]
    fn hard_negatives() {
        for t in [
            "commit 3f2a9c1d8e7b6a5f4e3d2c1b0a9f8e7d6c5b4a39 merged",
            "uuid 550e8400-e29b-41d4-a716-446655440000",
            "Apple Inc. reported revenue on 2024-01-25",
            "port 8080, version 1.2.3, timestamp 1700000000",
            "use process.env.OPENAI_API_KEY to configure",
        ] {
            let c = cats(t);
            assert!(c.iter().all(|x| !is_critical(x)), "{t}: {c:?}");
        }
    }
}
