//! `promptvpn eval`: reproducible privacy/utility benchmark over a synthetic
//! labeled corpus, for each classifier configuration.

use anyhow::{Context, Result};
use pv_core::*;
use pv_detectors::exact::{ExactEntry, ExactKind, ExactMatcher};
use pv_detectors::normalize::NormOpts;
use pv_detectors::span::{CustomEntity, SpanModel};
use pv_detectors::{Detector, DetectorSettings};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;
use zeroize::Zeroizing;

#[derive(Debug, Deserialize)]
pub struct Case {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub labels: Vec<Label>,
    /// BLOCK | TRANSFORM | ALLOW | REVIEW (REVIEW = held or private-routed)
    pub expect: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Label {
    pub value: String,
    pub category: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct CorpusMeta {
    #[serde(default)]
    pub entities: Vec<CustomEntity>,
    #[serde(default)]
    pub exact_values: Vec<String>,
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct Counts {
    pub tp_labels: u32,
    pub fn_labels: u32,
    pub tp_findings: u32,
    pub fp_findings: u32,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub config: String,
    pub cases: usize,
    pub per_category: BTreeMap<String, Value>,
    pub critical_labels: u32,
    pub critical_escapes: u32,
    pub critical_escape_rate: f64,
    pub decision_accuracy: f64,
    pub false_block_rate: f64,
    pub utility_preserved: f64,
    pub latency_ms_p50: f64,
    pub latency_ms_p95: f64,
    pub peak_rss_mb: f64,
    pub classifier_failures: u32,
    pub mismatches: Vec<Value>,
}

fn locate(text: &str, value: &str) -> Vec<(usize, usize)> {
    let mut out = vec![];
    let mut from = 0;
    while let Some(p) = text[from..].find(value) {
        out.push((from + p, from + p + value.len()));
        from += p + value.len().max(1);
        if from >= text.len() {
            break;
        }
    }
    out
}

fn overlap(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

fn cat_match(label: &str, finding: &str) -> bool {
    label == finding
}

pub fn peak_rss_mb() -> f64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let v = ru.ru_maxrss as f64;
    if cfg!(target_os = "macos") {
        v / 1024.0 / 1024.0
    } else {
        v / 1024.0
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[i]
}

pub fn load(corpus: &str) -> Result<(Vec<Case>, CorpusMeta)> {
    let text = std::fs::read_to_string(corpus).with_context(|| format!("read {corpus} (generate it with: python3 -I scripts/gen_eval_corpus.py > {corpus})"))?;
    let mut cases = vec![];
    let mut meta = CorpusMeta::default();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line).with_context(|| format!("line {}", i + 1))?;
        if v.get("meta").is_some() {
            meta = serde_json::from_value(v["meta"].clone())?;
            continue;
        }
        cases.push(serde_json::from_value(v).with_context(|| format!("line {}", i + 1))?);
    }
    Ok((cases, meta))
}

pub async fn run(config: &str, cases: &[Case], meta: &CorpusMeta, span: Option<Arc<dyn SpanModel>>) -> Report {
    let det = Detector::default();
    det.set_exact(ExactMatcher::new(
        meta.exact_values
            .iter()
            .enumerate()
            .map(|(i, v)| ExactEntry { secret_id: format!("eval_{i}"), kind: ExactKind::Value, value: Zeroizing::new(v.clone()), norm: NormOpts { unicode: true, casefold: true, whitespace: true, separators: false }, allow: false })
            .collect(),
    ));
    // Custom entity types need the span model; without it they are not evaluated.
    if span.is_some() {
        det.set_entities(meta.entities.clone());
    }
    det.set_span(span.clone());
    det.set_settings(DetectorSettings { span_enabled: span.is_some(), span_required: false, span_timeout_ms: 60_000 });
    let policy = default_policy(&default_category_choices());
    let mut policy = policy;
    for c in &meta.entities {
        policy.rules.push(PolicyRule {
            schema_version: 1,
            id: format!("entity-{}", c.id),
            version: 1,
            scope: Scope { kind: ScopeKind::User, id: "local-owner".into() },
            selector: Selector { categories: vec![format!("CUSTOM:{}", c.id)], destinations: vec!["external".into()], direction: Direction::Inbound, min_confidence: None },
            action: Action::Tokenize,
            severity: Sensitivity::High,
            notify: vec![],
            override_mode: OverrideMode::PolicyOwnerOnly,
            retention_days: 30,
            enabled: true,
            description: String::new(),
            origin: "eval".into(),
            critical_exception: false,
        });
    }
    let ctx = EvalContext {
        user_id: "local-owner",
        agent_id: None,
        app_id: None,
        request_id: None,
        destination: DestinationRef { id: "eval".into(), tier: DestinationTier::ExternalUnverified },
        direction: Direction::Inbound,
        fallback: FallbackSettings { mode: FallbackMode::BlockAndReview, tee_consent: false, local_private_handling: false, uncertain_threshold: 0.5 },
        approval: None,
    };

    let mut per: BTreeMap<String, Counts> = BTreeMap::new();
    let mut lat = vec![];
    let (mut crit, mut escapes, mut correct, mut benign, mut false_blocks, mut fails) = (0u32, 0u32, 0u32, 0u32, 0u32, 0u32);
    let (mut kept, mut total_nonlabel) = (0usize, 0usize);
    let mut mismatches = vec![];
    for c in cases {
        let t = Instant::now();
        let rep = det.detect(std::slice::from_ref(&c.text), span.is_some()).await;
        lat.push(t.elapsed().as_secs_f64() * 1000.0);
        if !rep.failures.is_empty() {
            fails += 1;
        }
        let d = evaluate(&policy, &rep.findings, &ctx);
        let label_spans: Vec<((usize, usize), &str)> = c.labels.iter().flat_map(|l| locate(&c.text, &l.value).into_iter().map(move |s| (s, l.category.as_str()))).collect();
        // Recall per label.
        for (span, cat) in &label_spans {
            let e = per.entry(cat.to_string()).or_default();
            let hit = rep.findings.iter().any(|f| cat_match(cat, &f.category) && (f.whole_field || overlap(*span, (f.start_byte, f.end_byte))));
            if hit {
                e.tp_labels += 1;
            } else {
                e.fn_labels += 1;
            }
            if category::is_critical(cat) || *cat == category::EXACT_SECRET {
                crit += 1;
                // Escape: request would leave with this value intact.
                let protected = match d.disposition {
                    Disposition::Block | Disposition::RequireApproval | Disposition::PrivateRoute => true,
                    _ => rep.findings.iter().any(|f| (f.whole_field || overlap(*span, (f.start_byte, f.end_byte))) && d.action_for(&f.finding_id).is_some_and(|a| a != Action::Allow)),
                };
                if !protected {
                    escapes += 1;
                }
            }
        }
        // Precision per finding.
        for f in &rep.findings {
            if f.category == category::CLASSIFIER_FAILURE || f.category == category::UNCERTAIN {
                continue;
            }
            let e = per.entry(f.category.clone()).or_default();
            if label_spans.iter().any(|(s, cat)| cat_match(cat, &f.category) && (f.whole_field || overlap(*s, (f.start_byte, f.end_byte)))) {
                e.tp_findings += 1;
            } else {
                e.fp_findings += 1;
            }
        }
        let got = match d.disposition {
            Disposition::Block => "BLOCK",
            Disposition::RequireApproval | Disposition::PrivateRoute => "REVIEW",
            Disposition::Transform => "TRANSFORM",
            Disposition::Allow => "ALLOW",
        };
        if got == c.expect {
            correct += 1;
        } else if mismatches.len() < 200 {
            mismatches.push(json!({"id": c.id, "expect": c.expect, "got": got, "categories": d.categories(), "tags": c.tags}));
        }
        if c.expect == "ALLOW" || c.expect == "TRANSFORM" {
            benign += 1;
            if got == "BLOCK" || got == "REVIEW" {
                false_blocks += 1;
            }
        }
        if got == "TRANSFORM" {
            // Utility: fraction of non-label bytes left unchanged.
            let mut covered = vec![false; c.text.len()];
            for f in rep.findings.iter().filter(|f| d.action_for(&f.finding_id).is_some_and(|a| a.is_transform())) {
                for b in covered.iter_mut().take(f.end_byte.min(c.text.len())).skip(f.start_byte) {
                    *b = true;
                }
            }
            for (i, cov) in covered.iter().enumerate() {
                if !label_spans.iter().any(|(s, _)| s.0 <= i && i < s.1) {
                    total_nonlabel += 1;
                    if !cov {
                        kept += 1;
                    }
                }
            }
        }
    }
    let per_category = per
        .into_iter()
        .map(|(k, c)| {
            let p = if c.tp_findings + c.fp_findings == 0 { None } else { Some(c.tp_findings as f64 / (c.tp_findings + c.fp_findings) as f64) };
            let r = if c.tp_labels + c.fn_labels == 0 { None } else { Some(c.tp_labels as f64 / (c.tp_labels + c.fn_labels) as f64) };
            (k, json!({"precision": p, "recall": r, "fnr": r.map(|r| 1.0 - r), "counts": c}))
        })
        .collect();
    Report {
        config: config.into(),
        cases: cases.len(),
        per_category,
        critical_labels: crit,
        critical_escapes: escapes,
        critical_escape_rate: if crit == 0 { 0.0 } else { escapes as f64 / crit as f64 },
        decision_accuracy: correct as f64 / cases.len().max(1) as f64,
        false_block_rate: if benign == 0 { 0.0 } else { false_blocks as f64 / benign as f64 },
        utility_preserved: if total_nonlabel == 0 { 1.0 } else { kept as f64 / total_nonlabel as f64 },
        latency_ms_p50: pct(&mut lat.clone(), 0.5),
        latency_ms_p95: pct(&mut lat, 0.95),
        peak_rss_mb: peak_rss_mb(),
        classifier_failures: fails,
        mismatches,
    }
}

pub fn markdown(reports: &[Report]) -> String {
    let mut s = String::from("| config | cases | critical escapes | escape rate | decision acc. | false-block rate | utility kept | p50 ms | p95 ms | peak RSS MB | classifier failures |\n|---|---|---|---|---|---|---|---|---|---|---|\n");
    for r in reports {
        s += &format!(
            "| {} | {} | {}/{} | {:.3} | {:.3} | {:.3} | {:.3} | {:.1} | {:.1} | {:.0} | {} |\n",
            r.config, r.cases, r.critical_escapes, r.critical_labels, r.critical_escape_rate, r.decision_accuracy, r.false_block_rate, r.utility_preserved, r.latency_ms_p50, r.latency_ms_p95, r.peak_rss_mb, r.classifier_failures
        );
    }
    s += "\nPer-category precision / recall:\n\n| config | category | precision | recall | FNR |\n|---|---|---|---|---|\n";
    for r in reports {
        for (k, v) in &r.per_category {
            let f = |x: &Value| x.as_f64().map(|x| format!("{x:.3}")).unwrap_or_else(|| "-".into());
            s += &format!("| {} | {} | {} | {} | {} |\n", r.config, k, f(&v["precision"]), f(&v["recall"]), f(&v["fnr"]));
        }
    }
    s
}
