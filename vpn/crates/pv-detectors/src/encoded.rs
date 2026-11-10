//! Detection over decoded views: base64 blobs, percent-escapes, \u/\x escapes.

use base64::Engine;
use pv_core::Finding;
use regex::Regex;
use std::sync::LazyLock;

static B64_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/_\-]{24,}={0,2}").unwrap());

/// Decode a base64/base64url blob to printable UTF-8, if it plausibly is text.
pub fn decode_b64_text(s: &str) -> Option<String> {
    let engines = [
        &base64::engine::general_purpose::STANDARD,
        &base64::engine::general_purpose::STANDARD_NO_PAD,
        &base64::engine::general_purpose::URL_SAFE,
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ];
    for e in engines {
        if let Ok(bytes) = e.decode(s.as_bytes()) {
            if let Ok(t) = String::from_utf8(bytes) {
                let printable = t.chars().filter(|c| !c.is_control() || c.is_whitespace()).count();
                if !t.is_empty() && printable * 100 / t.chars().count() >= 90 {
                    return Some(t);
                }
            }
        }
    }
    None
}

/// Findings for base64 blobs whose decoded text contains sensitive content.
/// `inner` runs the full detector on decoded text; depth limits recursion.
pub fn base64_findings(text: &str, depth: u8, inner: &dyn Fn(&str, u8) -> Vec<Finding>) -> Vec<Finding> {
    if depth == 0 {
        return vec![];
    }
    let mut out = Vec::new();
    for m in B64_RE.find_iter(text).take(256) {
        if m.len() > 1 << 20 {
            continue;
        }
        let Some(decoded) = decode_b64_text(m.as_str()) else { continue };
        for f in inner(&decoded, depth - 1) {
            let mut g = f.clone();
            g.finding_id = uuid::Uuid::new_v4().to_string();
            g.start_byte = m.start();
            g.end_byte = m.end();
            g.confidence = (f.confidence * 0.95).clamp(0.0, 1.0);
            g.evidence = format!("base64-decoded:{}", f.evidence);
            g.recognizer_id = format!("b64({})", f.recognizer_id);
            out.push(g);
        }
    }
    out
}

/// A view with %XX, \uXXXX and \xXX escapes decoded, mapping back to source bytes.
pub fn unescaped_view(text: &str) -> Option<crate::normalize::View> {
    if !(text.contains('%') || text.contains("\\u") || text.contains("\\x")) {
        return None;
    }
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut starts = Vec::with_capacity(text.len());
    let mut ends = Vec::with_capacity(text.len());
    let mut i = 0;
    let mut changed = false;
    let hex = |s: &str| u32::from_str_radix(s, 16).ok();
    while i < b.len() {
        let (ch, len) = if b[i] == b'%' && i + 3 <= b.len() && text.is_char_boundary(i + 3) {
            match hex(&text[i + 1..i + 3]) {
                Some(v) if v < 0x80 => (Some(char::from_u32(v).unwrap()), 3),
                _ => (None, 0),
            }
        } else if b[i] == b'\\' && i + 6 <= b.len() && b[i + 1] == b'u' && text.is_char_boundary(i + 6) {
            match hex(&text[i + 2..i + 6]).and_then(char::from_u32) {
                Some(c) => (Some(c), 6),
                None => (None, 0),
            }
        } else if b[i] == b'\\' && i + 4 <= b.len() && b[i + 1] == b'x' && text.is_char_boundary(i + 4) {
            match hex(&text[i + 2..i + 4]) {
                Some(v) if v < 0x80 => (Some(char::from_u32(v).unwrap()), 4),
                _ => (None, 0),
            }
        } else {
            (None, 0)
        };
        if let Some(c) = ch {
            changed = true;
            out.push(c);
            for _ in 0..c.len_utf8() {
                starts.push(i);
                ends.push(i + len);
            }
            i += len;
        } else {
            let c = text[i..].chars().next().unwrap();
            out.push(c);
            for _ in 0..c.len_utf8() {
                starts.push(i);
                ends.push(i + c.len_utf8());
            }
            i += c.len_utf8();
        }
    }
    changed.then(|| crate::normalize::View::from_parts(out, starts, ends))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64_detects_inner_ssn() {
        let blob = base64::engine::general_purpose::STANDARD.encode("ssn 123-45-6789 here");
        let t = format!("payload={blob};");
        let f = base64_findings(&t, 2, &|s, _| crate::builtin::detect(s));
        assert!(f.iter().any(|f| f.category == "GOVERNMENT_ID"));
        assert_eq!(&t[f[0].start_byte..f[0].end_byte], blob);
    }

    #[test]
    fn unescape_view_maps() {
        let t = "mail jane%40example.com or \\u0040x";
        let v = unescaped_view(t).unwrap();
        assert!(v.text.contains("jane@example.com"));
        let p = v.text.find("jane@example.com").unwrap();
        let (a, b) = v.map_span(p, p + "jane@example.com".len()).unwrap();
        assert_eq!(&t[a..b], "jane%40example.com");
    }
}
