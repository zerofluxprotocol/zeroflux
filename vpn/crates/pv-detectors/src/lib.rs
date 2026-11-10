//! Detection pipeline: deterministic recognizers, owner-registered exact
//! values, decoded/Unicode-folded views, and the GLiNER span model (built-in
//! labels plus owner-defined entity types). Every finding is an exact span.

pub mod builtin;
pub mod encoded;
pub mod exact;
pub mod normalize;
pub mod ollama;
pub mod span;

use pv_core::category::{self, CLASSIFIER_FAILURE};
use pv_core::{ClassifierType, Finding, Sensitivity};
use serde::{Deserialize, Serialize};
use span::{CustomEntity, SpanModel};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectorSettings {
    /// Run the span model when one is loaded.
    pub span_enabled: bool,
    /// A missing or failing span model is a classifier failure (fail closed).
    /// Always true while custom entity types exist, since only the model finds them.
    pub span_required: bool,
    pub span_timeout_ms: u64,
}

impl Default for DetectorSettings {
    fn default() -> Self {
        DetectorSettings { span_enabled: true, span_required: false, span_timeout_ms: 10_000 }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DetectionReport {
    pub findings: Vec<Finding>,
    pub recognizer_versions: Vec<String>,
    pub failures: Vec<String>,
    pub span_latency_ms: Option<u64>,
}

/// Shared by all requests. Components are swapped atomically when the owner
/// edits secrets, custom entity types or the model.
pub struct Detector {
    pub exact: RwLock<Arc<exact::ExactMatcher>>,
    pub span: RwLock<Option<Arc<dyn SpanModel>>>,
    pub entities: RwLock<Arc<Vec<CustomEntity>>>,
    pub settings: RwLock<DetectorSettings>,
}

impl Default for Detector {
    fn default() -> Self {
        Detector {
            exact: RwLock::new(Arc::new(exact::ExactMatcher::default())),
            span: RwLock::new(None),
            entities: RwLock::new(Arc::new(vec![])),
            settings: RwLock::new(DetectorSettings::default()),
        }
    }
}

/// Deterministic detection (built-in + exact) over raw, confusable-folded and
/// unescaped views, plus base64-decoded blobs. Pure and synchronous.
pub fn detect_deterministic(text: &str, exact: &exact::ExactMatcher) -> Vec<Finding> {
    detect_det_depth(text, exact, 2)
}

fn detect_det_depth(text: &str, exact: &exact::ExactMatcher, depth: u8) -> Vec<Finding> {
    let mut out = builtin::detect(text);
    out.extend(exact.find(text));
    let conf = normalize::view(text, normalize::NormOpts::confusables());
    if conf.text != text {
        for mut f in builtin::detect(&conf.text) {
            if let Some((s, e)) = conf.map_span(f.start_byte, f.end_byte) {
                f.start_byte = s;
                f.end_byte = e;
                f.evidence = format!("{}+unicode-normalized", f.evidence);
                out.push(f);
            }
        }
    }
    if let Some(v) = encoded::unescaped_view(text) {
        let mut inner = builtin::detect(&v.text);
        inner.extend(exact.find(&v.text));
        for mut f in inner {
            if let Some((s, e)) = v.map_span(f.start_byte, f.end_byte) {
                f.start_byte = s;
                f.end_byte = e;
                f.evidence = format!("{}+unescaped", f.evidence);
                out.push(f);
            }
        }
    }
    out.extend(encoded::base64_findings(text, depth, &|s, d| detect_det_depth(s, exact, d)));
    let allowed = exact.allowed_spans(text);
    if !allowed.is_empty() {
        out.retain(|f| f.sensitivity == Sensitivity::Critical || !allowed.iter().any(|&(s, e)| s <= f.start_byte && f.end_byte <= e));
    }
    builtin::dedupe(out)
}

fn failure(field_index: usize, len: usize, what: &str) -> Finding {
    let mut f = Finding::new(ClassifierType::Builtin, CLASSIFIER_FAILURE, 0, len, 1.0, what, "classifier-failure", Sensitivity::High);
    f.field_index = field_index;
    f.whole_field = true;
    f
}

impl Detector {
    pub fn set_exact(&self, m: exact::ExactMatcher) {
        *self.exact.write().unwrap() = Arc::new(m);
    }
    pub fn set_entities(&self, e: Vec<CustomEntity>) {
        *self.entities.write().unwrap() = Arc::new(e);
    }
    pub fn entities(&self) -> Arc<Vec<CustomEntity>> {
        self.entities.read().unwrap().clone()
    }
    pub fn set_span(&self, s: Option<Arc<dyn SpanModel>>) {
        *self.span.write().unwrap() = s;
    }
    pub fn span_loaded(&self) -> bool {
        self.span.read().unwrap().is_some()
    }
    pub fn set_settings(&self, s: DetectorSettings) {
        *self.settings.write().unwrap() = s;
    }
    pub fn settings(&self) -> DetectorSettings {
        self.settings.read().unwrap().clone()
    }

    /// Detect over all fields. `use_model` runs the span model (inbound always;
    /// outbound only when the owner enables it).
    pub async fn detect(&self, fields: &[String], use_model: bool) -> DetectionReport {
        let exact = self.exact.read().unwrap().clone();
        let settings = self.settings();
        let entities = self.entities();
        let mut rep = DetectionReport { recognizer_versions: vec![builtin::VERSION.into()], ..Default::default() };
        if !exact.is_empty() {
            rep.recognizer_versions.push("exact-v1".into());
        }
        for (i, t) in fields.iter().enumerate() {
            for mut f in detect_deterministic(t, &exact) {
                f.field_index = i;
                rep.findings.push(f);
            }
        }
        let nonempty: Vec<usize> = (0..fields.len()).filter(|&i| !fields[i].trim().is_empty()).collect();
        if !use_model || nonempty.is_empty() {
            return rep;
        }
        let required = settings.span_required || !entities.is_empty();
        let last = *nonempty.last().unwrap();
        let model = if settings.span_enabled { self.span.read().unwrap().clone() } else { None };
        match model {
            Some(model) => {
                rep.recognizer_versions.push(model.id());
                let t = Instant::now();
                let texts: Vec<(usize, String)> = nonempty.iter().map(|&i| (i, fields[i].clone())).collect();
                let ents = entities.clone();
                let job = tokio::task::spawn_blocking(move || {
                    let mut out = Vec::new();
                    for (i, t) in texts {
                        for mut f in model.detect(&t, &ents)? {
                            if f.validate_against(&t).is_ok() {
                                f.field_index = i;
                                out.push(f);
                            }
                        }
                    }
                    Ok::<_, String>(out)
                });
                let res = tokio::time::timeout(Duration::from_millis(settings.span_timeout_ms), job).await;
                rep.span_latency_ms = Some(t.elapsed().as_millis() as u64);
                match res {
                    Ok(Ok(Ok(f))) => rep.findings.extend(f),
                    _ => {
                        rep.failures.push("span-model".into());
                        if required {
                            rep.findings.push(failure(last, fields[last].len(), "span-model-failure"));
                        }
                    }
                }
            }
            None if required => {
                rep.failures.push("span-model-missing".into());
                rep.findings.push(failure(last, fields[last].len(), "span-model-missing"));
            }
            None => {}
        }
        rep.findings = builtin::dedupe(std::mem::take(&mut rep.findings));
        rep
    }
}

/// True if any finding is critical by category.
pub fn has_critical(findings: &[Finding]) -> bool {
    findings.iter().any(|f| category::is_critical(&f.category) || f.category == category::EXACT_SECRET)
}

#[cfg(test)]
mod tests {
    use super::*;
    use exact::{ExactEntry, ExactKind, ExactMatcher};
    use span::KeywordSpans;
    use zeroize::Zeroizing;

    #[test]
    fn confusable_ssn_found() {
        let t = "ssn １２３-４５-６７８９";
        let f = detect_deterministic(t, &ExactMatcher::default());
        let g = f.iter().find(|f| f.category == "GOVERNMENT_ID").unwrap();
        assert_eq!(&t[g.start_byte..g.end_byte], "１２３-４５-６７８９");
    }

    #[test]
    fn allowlist_suppresses_noncritical_only() {
        let m = ExactMatcher::new(vec![
            ExactEntry { secret_id: "a".into(), kind: ExactKind::Value, value: Zeroizing::new("help@acme.test".into()), norm: Default::default(), allow: true },
            ExactEntry { secret_id: "b".into(), kind: ExactKind::Value, value: Zeroizing::new("123-45-6789".into()), norm: Default::default(), allow: true },
        ]);
        let f = detect_deterministic("help@acme.test 123-45-6789", &m);
        assert!(!f.iter().any(|f| f.category == "EMAIL"));
        assert!(f.iter().any(|f| f.category == "GOVERNMENT_ID"));
    }

    fn ent() -> CustomEntity {
        CustomEntity { id: "ent_1".into(), label: "policy number".into(), description: String::new() }
    }

    #[tokio::test]
    async fn optional_model_missing_is_fine() {
        let d = Detector::default();
        let r = d.detect(&["hello".into()], true).await;
        assert!(r.findings.is_empty());
    }

    #[tokio::test]
    async fn required_model_missing_fails_closed() {
        let d = Detector::default();
        d.set_settings(DetectorSettings { span_required: true, ..Default::default() });
        let r = d.detect(&["hello".into()], true).await;
        assert!(r.findings.iter().any(|f| f.category == CLASSIFIER_FAILURE));
    }

    #[tokio::test]
    async fn custom_entities_require_the_model() {
        let d = Detector::default();
        d.set_entities(vec![ent()]);
        let r = d.detect(&["policy number: P-77".into()], true).await;
        assert!(r.findings.iter().any(|f| f.category == CLASSIFIER_FAILURE));
    }

    #[tokio::test]
    async fn model_failure_fails_closed_when_required() {
        let d = Detector::default();
        d.set_span(Some(Arc::new(KeywordSpans { builtin: vec![], fail: true })));
        d.set_settings(DetectorSettings { span_required: true, ..Default::default() });
        let r = d.detect(&["hello".into()], true).await;
        assert!(r.findings.iter().any(|f| f.category == CLASSIFIER_FAILURE));
        assert_eq!(r.failures, vec!["span-model"]);
    }

    #[tokio::test]
    async fn custom_entity_spans() {
        let d = Detector::default();
        d.set_span(Some(Arc::new(KeywordSpans { builtin: vec![], fail: false })));
        d.set_entities(vec![ent()]);
        let t = "renew policy number: PX-4471 today".to_string();
        let r = d.detect(&["sys".into(), t.clone()], true).await;
        let f = r.findings.iter().find(|f| f.category == "CUSTOM:ent_1").unwrap();
        assert_eq!(f.field_index, 1);
        assert_eq!(&t[f.start_byte..f.end_byte], "PX-4471");
        assert_eq!(f.entity_id.as_deref(), Some("ent_1"));
    }

    #[tokio::test]
    async fn model_skipped_when_not_requested() {
        let d = Detector::default();
        d.set_span(Some(Arc::new(KeywordSpans { builtin: vec![("maria".into(), category::PERSON)], fail: false })));
        assert!(d.detect(&["maria".into()], false).await.findings.is_empty());
        assert_eq!(d.detect(&["maria".into()], true).await.findings.len(), 1);
    }
}
