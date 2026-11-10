//! Span-level PII recognition through an embedded GLiNER runtime (not Ollama).
//! Besides the built-in labels, owner-defined entity types are passed to the
//! model as extra zero-shot labels and come back as `CUSTOM:<id>` spans.

use pv_core::category::*;
use pv_core::{ClassifierType, Finding};
use serde::{Deserialize, Serialize};

/// Owner-defined entity type, e.g. label "policy number".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomEntity {
    pub id: String,
    /// Natural-language label given to the span model (lowercase, 2..60 chars).
    pub label: String,
    #[serde(default)]
    pub description: String,
}

pub fn normalize_label(l: &str) -> Option<String> {
    let l: String = l.trim().to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-').collect();
    let l = l.split_whitespace().collect::<Vec<_>>().join(" ");
    (2..=60).contains(&l.chars().count()).then_some(l)
}

pub trait SpanModel: Send + Sync {
    fn id(&self) -> String;
    /// Synchronous; callers run it on a blocking thread.
    fn detect(&self, text: &str, custom: &[CustomEntity]) -> Result<Vec<Finding>, String>;
}

/// GLiNER labels requested from the model, with the built-in category each maps to.
pub const GLINER_LABELS: &[(&str, &str)] = &[
    ("name", PERSON),
    ("username", PERSON),
    ("email address", EMAIL),
    ("phone number", PHONE),
    ("location address", ADDRESS),
    ("location street", ADDRESS),
    ("dob", DOB),
    ("ssn", GOVERNMENT_ID),
    ("passport number", PASSPORT),
    ("driver license", DRIVER_LICENSE),
    ("credit card", PAYMENT_CARD),
    ("cvv", PAYMENT_CARD),
    ("bank account", FINANCIAL_ACCOUNT),
    ("account number", FINANCIAL_ACCOUNT),
    ("routing number", FINANCIAL_ACCOUNT),
    ("condition", HEALTH_INFORMATION),
    ("drug", HEALTH_INFORMATION),
    ("medical code", HEALTH_INFORMATION),
    ("healthcare number", HEALTH_INFORMATION),
    ("password", AUTH_SECRET),
];

pub fn label_to_category(label: &str) -> Option<&'static str> {
    GLINER_LABELS.iter().find(|(l, _)| *l == label).map(|(_, c)| *c)
}

/// Map a character-offset span (as some runtimes report) to byte offsets.
pub fn char_to_byte(text: &str, start_char: usize, end_char: usize) -> Option<(usize, usize)> {
    let mut idx: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    idx.push(text.len());
    Some((*idx.get(start_char)?, *idx.get(end_char)?))
}

/// Locate a model-reported entity text near its reported offset, returning byte offsets.
pub fn locate_entity(text: &str, entity: &str, hint_byte: usize) -> Option<(usize, usize)> {
    if entity.is_empty() {
        return None;
    }
    let mut best: Option<(usize, usize)> = None;
    let mut from = 0;
    while let Some(p) = text[from..].find(entity) {
        let s = from + p;
        let d = s.abs_diff(hint_byte);
        if best.is_none_or(|(bs, _)| s.abs_diff(hint_byte) < bs.abs_diff(hint_byte)) {
            best = Some((s, s + entity.len()));
        }
        if d == 0 {
            break;
        }
        from = s + entity.len().max(1);
        if from >= text.len() {
            break;
        }
    }
    best
}

/// Shape checks applied to span-model output. GLiNER is good at context but
/// labels product names and countries as IDs/addresses; critical categories
/// block requests, so their spans must look like the thing they claim to be.
pub fn plausible(category: &str, text: &str) -> bool {
    let digits = text.chars().filter(|c| c.is_ascii_digit()).count();
    let letters = text.chars().filter(|c| c.is_alphabetic()).count();
    match category {
        ADDRESS => digits >= 1 && text.split_whitespace().count() >= 2,
        DRIVER_LICENSE | PASSPORT | GOVERNMENT_ID | FINANCIAL_ACCOUNT | PAYMENT_CARD => digits >= 5,
        PHONE => digits >= 7,
        EMAIL => text.contains('@'),
        DOB => digits >= 2,
        AUTH_SECRET => text.chars().count() >= 8 && !text.contains(char::is_whitespace) && digits >= 1 && letters >= 1,
        HEALTH_INFORMATION => letters >= 4 && !text.contains('%') && !(text.len() <= 5 && text.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())),
        PERSON => letters >= 3,
        _ => true,
    }
}

/// Model-only claims of critical data below this confidence are reported as
/// UNCERTAIN (reviewed or privately routed) instead of hard-blocking.
pub const CRITICAL_MIN_CONFIDENCE: f32 = 0.8;

pub fn make_finding(category: &str, s: usize, e: usize, prob: f32, version: &str) -> Finding {
    let (cat, evidence) = if is_critical(category) && prob < CRITICAL_MIN_CONFIDENCE {
        (UNCERTAIN, format!("span-model:{category}"))
    } else {
        (category, "span-model".to_string())
    };
    let mut f = Finding::new(ClassifierType::Builtin, cat, s, e, prob.clamp(0.0, 1.0), "gliner-span", evidence, sensitivity_of(cat));
    f.classifier_version = version.into();
    f
}

pub fn custom_finding(e: &CustomEntity, s: usize, end: usize, prob: f32, version: &str) -> Finding {
    let mut f = Finding::new(ClassifierType::Custom, format!("{CUSTOM_PREFIX}{}", e.id), s, end, prob.clamp(0.0, 1.0), "gliner-custom-label", "span-model", pv_core::Sensitivity::High);
    f.entity_id = Some(e.id.clone());
    f.classifier_version = version.into();
    f
}

/// Split text into overlapping chunks on whitespace so entities near a
/// boundary are seen whole in at least one chunk. Returns (byte offset, chunk).
pub fn chunks(text: &str, size: usize, overlap: usize) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + size).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end < text.len() {
            if let Some(ws) = text[start..end].rfind(char::is_whitespace) {
                if ws > size / 2 {
                    end = start + ws;
                }
            }
        }
        out.push((start, &text[start..end]));
        if end >= text.len() {
            break;
        }
        let mut next = end.saturating_sub(overlap).max(start + 1);
        while !text.is_char_boundary(next) {
            next += 1;
        }
        // Start the next chunk at a word boundary.
        if let Some(ws) = text[next..end].find(char::is_whitespace) {
            next += ws + 1;
        }
        start = next;
    }
    out
}

/// Deterministic test double: finds configured literal strings for built-in
/// categories and custom labels (case-insensitive).
pub struct KeywordSpans {
    pub builtin: Vec<(String, &'static str)>,
    /// Matches when the custom entity's label appears as `<label>: <value>`; the value is the span.
    pub fail: bool,
}

impl SpanModel for KeywordSpans {
    fn id(&self) -> String {
        "keyword-spans-test".into()
    }
    fn detect(&self, text: &str, custom: &[CustomEntity]) -> Result<Vec<Finding>, String> {
        if self.fail {
            return Err("test failure".into());
        }
        let lower = text.to_lowercase();
        let mut out = Vec::new();
        for (kw, cat) in &self.builtin {
            if let Some(p) = lower.find(&kw.to_lowercase()) {
                out.push(make_finding(cat, p, p + kw.len(), 0.9, "keyword"));
            }
        }
        for e in custom {
            let cue = format!("{}: ", e.label);
            if let Some(p) = lower.find(&cue) {
                let s = p + cue.len();
                let end = text[s..].find(|c: char| c.is_whitespace() || c == ',' || c == '.').map(|x| s + x).unwrap_or(text.len());
                if end > s {
                    out.push(custom_finding(e, s, end, 0.9, "keyword"));
                }
            }
        }
        Ok(out)
    }
}

#[cfg(feature = "gliner")]
pub mod gliner_impl {
    use super::*;
    use gliner::model::input::text::TextInput;
    use gliner::model::params::Parameters;
    use gliner::model::pipeline::span::SpanMode;
    use gliner::model::pipeline::token::TokenMode;
    use gliner::model::GLiNER;
    use orp::params::RuntimeParameters;
    use std::sync::Mutex;

    enum Inner {
        Token(GLiNER<TokenMode>),
        Span(GLiNER<SpanMode>),
    }

    pub struct GlinerModel {
        model: Mutex<Inner>,
        version: String,
    }

    impl GlinerModel {
        /// `span_mode`: the model's `gliner_config.json` value (`token_level` or a span mode like `markerV0`).
        pub fn load(tokenizer: &str, onnx: &str, threshold: f32, version: &str, span_mode: &str) -> Result<Self, String> {
            let threads = std::thread::available_parallelism().map(|n| n.get().min(4)).unwrap_or(2);
            let params = Parameters::default().with_threshold(threshold).with_max_length(Some(512));
            let rt = RuntimeParameters::default().with_threads(threads);
            let inner = if span_mode == "token_level" {
                Inner::Token(GLiNER::<TokenMode>::new(params, rt, tokenizer, onnx).map_err(|e| format!("gliner load: {e}"))?)
            } else {
                Inner::Span(GLiNER::<SpanMode>::new(params, rt, tokenizer, onnx).map_err(|e| format!("gliner load: {e}"))?)
            };
            Ok(GlinerModel { model: Mutex::new(inner), version: version.into() })
        }
    }

    impl SpanModel for GlinerModel {
        fn id(&self) -> String {
            self.version.clone()
        }

        fn detect(&self, text: &str, custom: &[CustomEntity]) -> Result<Vec<Finding>, String> {
            let mut labels: Vec<String> = GLINER_LABELS.iter().map(|(l, _)| l.to_string()).collect();
            for e in custom {
                if !labels.contains(&e.label) {
                    labels.push(e.label.clone());
                }
            }
            let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
            let mut out: Vec<Finding> = Vec::new();
            let model = self.model.lock().map_err(|_| "gliner lock poisoned".to_string())?;
            for (base, chunk) in chunks(text, 1500, 200) {
                let input = TextInput::from_str(&[chunk], &label_refs).map_err(|e| e.to_string())?;
                let output = match &*model {
                    Inner::Token(m) => m.inference(input),
                    Inner::Span(m) => m.inference(input),
                }
                .map_err(|e| e.to_string())?;
                for spans in output.spans {
                    for s in spans {
                        let (cs, _) = s.offsets();
                        let hint = char_to_byte(chunk, cs, cs).map(|(b, _)| b).unwrap_or(0);
                        let Some((a, b)) = locate_entity(chunk, s.text(), hint) else { continue };
                        let (a, b) = (base + a, base + b);
                        if out.iter().any(|f| f.start_byte == a && f.end_byte == b) {
                            continue; // seen in an overlapping chunk
                        }
                        if let Some(cat) = label_to_category(s.class()) {
                            if plausible(cat, s.text()) {
                                out.push(make_finding(cat, a, b, s.probability(), &self.version));
                            }
                        } else if let Some(e) = custom.iter().find(|e| e.label == s.class()) {
                            out.push(custom_finding(e, a, b, s.probability(), &self.version));
                        }
                    }
                }
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plausibility_filters_known_false_positives() {
        assert!(!plausible(ADDRESS, "France"));
        assert!(!plausible(ADDRESS, "Springfield"));
        assert!(plausible(ADDRESS, "42 Oak Street"));
        assert!(!plausible(DRIVER_LICENSE, "Orion-7"));
        assert!(plausible(DRIVER_LICENSE, "D1234567"));
        assert!(!plausible(AUTH_SECRET, "HBM4"));
        assert!(!plausible(AUTH_SECRET, "OPENAI_API_KEY"));
        assert!(plausible(AUTH_SECRET, "Tr0ub4dor&3"));
        assert!(!plausible(HEALTH_INFORMATION, "FP8"));
        assert!(!plausible(HEALTH_INFORMATION, "12%"));
        assert!(plausible(HEALTH_INFORMATION, "type 2 diabetes"));
        assert!(plausible(PERSON, "Maria Gonzalez"));
        assert!(plausible(PERSON, "maria gonzalez"));
    }

    #[test]
    fn low_confidence_critical_becomes_uncertain() {
        assert_eq!(make_finding(DRIVER_LICENSE, 0, 5, 0.52, "v").category, UNCERTAIN);
        assert_eq!(make_finding(DRIVER_LICENSE, 0, 5, 0.9, "v").category, DRIVER_LICENSE);
        assert_eq!(make_finding(PERSON, 0, 5, 0.52, "v").category, PERSON);
    }

    #[test]
    fn chunks_overlap_and_cover() {
        let t = "word ".repeat(1000);
        let c = chunks(&t, 300, 60);
        assert!(c.len() > 1);
        assert_eq!(c[0].0, 0);
        for w in c.windows(2) {
            assert!(w[1].0 < w[0].0 + w[0].1.len(), "chunks overlap");
        }
        let last = c.last().unwrap();
        assert_eq!(last.0 + last.1.len(), t.len());
        assert_eq!(chunks("short", 300, 60), vec![(0, "short")]);
    }

    #[test]
    fn labels_normalized() {
        assert_eq!(normalize_label("  Policy   Number! "), Some("policy number".into()));
        assert_eq!(normalize_label("x"), None);
    }

    #[test]
    fn mapping_and_locate() {
        assert_eq!(label_to_category("ssn"), Some(GOVERNMENT_ID));
        assert_eq!(label_to_category("money"), None);
        let t = "é Jane met Jane";
        assert_eq!(char_to_byte(t, 2, 6), Some((3, 7)));
        assert_eq!(locate_entity(t, "Jane", 12), Some((12, 16)));
        assert_eq!(locate_entity(t, "Jane", 0), Some((3, 7)));
    }
}
