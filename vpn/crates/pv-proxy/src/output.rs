//! Outbound inspection: model output is classified with outbound selectors,
//! transformed (redacted), then approved placeholders are restored for the
//! authorized caller. Streaming uses a bounded hold-back so no byte is
//! released before the deterministic detectors have seen its full context.

use pv_core::{evaluate, Action, Decision, DestinationRef, Direction, Disposition, EvalContext, FallbackSettings, Finding, PolicySet};
use pv_detectors::exact::ExactMatcher;
use pv_detectors::{detect_deterministic, Detector};
use pv_transform::{restore, safe_release_len};
use pv_vault::Vault;
use std::sync::Arc;

/// Bytes held back from release while streaming (longer than any built-in pattern
/// except PEM blocks, which are handled explicitly).
pub const HOLDBACK: usize = 256;
/// Bytes of already-released text kept as detection context (for cue words).
pub const CONTEXT: usize = 64;

#[derive(Debug, Clone, thiserror::Error)]
#[error("model output blocked by local policy ({categories:?})")]
pub struct OutputBlocked {
    pub categories: Vec<String>,
}

#[derive(Clone)]
pub struct OutCtx {
    pub policy: Arc<PolicySet>,
    pub fallback: FallbackSettings,
    pub user_id: String,
    pub agent_id: Option<String>,
    pub app_id: Option<String>,
    pub destination: DestinationRef,
    pub vault: Arc<Vault>,
    pub detector: Arc<Detector>,
    pub exact: Arc<ExactMatcher>,
    pub scope: String,
    /// Restore placeholders (only for the authorized caller that owns the scope).
    pub restore: bool,
    /// Run the span model on output (forces full buffering).
    pub span_model: bool,
}

#[derive(Debug, Default, Clone)]
pub struct OutStats {
    pub findings: u32,
    pub redacted: u32,
    pub restored: u32,
    pub categories: Vec<String>,
}

impl OutCtx {
    fn decide(&self, findings: &[Finding]) -> Decision {
        let ctx = EvalContext {
            user_id: &self.user_id,
            agent_id: self.agent_id.clone(),
            app_id: self.app_id.clone(),
            request_id: None,
            destination: self.destination.clone(),
            direction: Direction::Outbound,
            fallback: self.fallback.clone(),
            approval: None,
        };
        let mut d = evaluate(&self.policy, findings, &ctx);
        // Outbound tokens could never be restored; treat TOKENIZE/GENERALIZE as REDACT.
        for f in &mut d.findings {
            if matches!(f.action, Action::Tokenize | Action::Generalize) {
                f.action = Action::Redact;
            }
            // Routing actions are meaningless after generation; fail safe to redaction.
            if matches!(f.action, Action::PrivateRoute | Action::RequireApproval | Action::Fallback) {
                f.action = Action::Redact;
            }
        }
        d
    }

    /// Apply outbound policy to one complete text (findings relative to `text`).
    pub fn apply(&self, text: &str, findings: Vec<Finding>, stats: &mut OutStats) -> Result<String, OutputBlocked> {
        let mut out = text.to_string();
        if !findings.is_empty() {
            let d = self.decide(&findings);
            if d.disposition == Disposition::Block || d.findings.iter().any(|f| f.action == Action::Block) {
                return Err(OutputBlocked { categories: d.categories() });
            }
            stats.findings += findings.len() as u32;
            let mut fields = vec![out];
            let fs: Vec<Finding> = findings.into_iter().map(|mut f| {
                f.field_index = 0;
                f
            }).collect();
            // Whole-field outbound findings cannot be isolated: block.
            if fs.iter().any(|f| f.whole_field && d.action_for(&f.finding_id).is_some_and(|a| a != Action::Allow)) {
                return Err(OutputBlocked { categories: d.categories() });
            }
            let r = pv_transform::apply(&mut fields, &fs, &d, &self.vault, &self.scope, chrono::Duration::minutes(1)).map_err(|_| OutputBlocked { categories: d.categories() })?;
            stats.redacted += r.redacted;
            for c in d.findings.iter().filter(|f| f.action != Action::Allow).map(|f| f.category.clone()) {
                if !stats.categories.contains(&c) {
                    stats.categories.push(c);
                }
            }
            out = fields.pop().unwrap();
        }
        if self.restore {
            match restore(&out, &self.vault, &self.scope) {
                Ok((s, st)) => {
                    stats.restored += st.restored;
                    out = s;
                }
                Err(_) => return Err(OutputBlocked { categories: vec!["RESTORE_FAILURE".into()] }),
            }
        }
        Ok(out)
    }

    /// Full (non-incremental) processing, optionally with model classifiers.
    pub async fn process_full(&self, texts: &[String], stats: &mut OutStats) -> Result<Vec<String>, OutputBlocked> {
        let findings: Vec<Finding> = if self.span_model {
            self.detector.detect(texts, true).await.findings
        } else {
            let mut all = Vec::new();
            for (i, t) in texts.iter().enumerate() {
                for mut f in detect_deterministic(t, &self.exact) {
                    f.field_index = i;
                    all.push(f);
                }
            }
            all
        };
        // A classifier failure on output fails closed.
        if findings.iter().any(|f| f.category == pv_core::category::CLASSIFIER_FAILURE) {
            return Err(OutputBlocked { categories: vec![pv_core::category::CLASSIFIER_FAILURE.into()] });
        }
        let mut out = Vec::with_capacity(texts.len());
        for (i, t) in texts.iter().enumerate() {
            let fs: Vec<Finding> = findings.iter().filter(|f| f.field_index == i).cloned().collect();
            out.push(self.apply(t, fs, stats)?);
        }
        Ok(out)
    }
}

/// Incremental guard for one text stream (one choice's content).
pub struct TextGuard {
    ctx: Arc<OutCtx>,
    pending: String,
    context: String,
    pub stats: OutStats,
    full_buffer: bool,
    max_bytes: usize,
    total: usize,
}

impl TextGuard {
    pub fn new(ctx: Arc<OutCtx>, max_bytes: usize) -> Self {
        let full_buffer = ctx.span_model;
        TextGuard { ctx, pending: String::new(), context: String::new(), stats: OutStats::default(), full_buffer, max_bytes, total: 0 }
    }

    /// Append model text; returns inspected text safe to release now.
    pub fn push(&mut self, s: &str) -> Result<String, OutputBlocked> {
        self.total += s.len();
        if self.total > self.max_bytes {
            return Err(OutputBlocked { categories: vec!["OUTPUT_TOO_LARGE".into()] });
        }
        self.pending.push_str(s);
        if self.full_buffer {
            return Ok(String::new());
        }
        self.release(false)
    }

    pub async fn finish(&mut self) -> Result<String, OutputBlocked> {
        if self.full_buffer {
            let text = std::mem::take(&mut self.pending);
            let mut st = std::mem::take(&mut self.stats);
            let r = self.ctx.process_full(&[text], &mut st).await;
            self.stats = st;
            return r.map(|mut v| v.pop().unwrap_or_default());
        }
        self.release(true)
    }

    fn release(&mut self, final_: bool) -> Result<String, OutputBlocked> {
        if self.pending.is_empty() {
            return Ok(String::new());
        }
        let window = format!("{}{}", self.context, self.pending);
        let cl = self.context.len();
        let findings: Vec<Finding> = detect_deterministic(&window, &self.ctx.exact).into_iter().filter(|f| f.end_byte > cl).collect();

        let mut rp = if final_ {
            self.pending.len()
        } else {
            let mut r = self.pending.len().saturating_sub(HOLDBACK).min(safe_release_len(&self.pending));
            if let Some(b) = self.pending.rfind("-----BEGIN") {
                if !self.pending[b..].contains("-----END") {
                    r = r.min(b);
                }
            }
            r
        };
        // Never split a finding; findings reaching into context hold everything.
        loop {
            let mut moved = false;
            for f in &findings {
                let s = f.start_byte.saturating_sub(cl);
                let e = f.end_byte - cl;
                if s < rp && rp < e {
                    rp = s;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        while rp > 0 && !self.pending.is_char_boundary(rp) {
            rp -= 1;
        }
        if rp == 0 {
            return Ok(String::new());
        }
        let segment: String = self.pending[..rp].to_string();
        let seg_findings: Vec<Finding> = findings
            .into_iter()
            .filter(|f| f.end_byte - cl <= rp)
            .map(|mut f| {
                // A finding that began in released context still has its tail redacted.
                f.start_byte = f.start_byte.max(cl) - cl;
                f.end_byte -= cl;
                f
            })
            .collect();
        let out = self.ctx.apply(&segment, seg_findings, &mut self.stats)?;
        // Keep raw context for cue words that precede later matches.
        let mut ctx = format!("{}{}", self.context, segment);
        if ctx.len() > CONTEXT {
            let mut cut = ctx.len() - CONTEXT;
            while !ctx.is_char_boundary(cut) {
                cut += 1;
            }
            ctx = ctx[cut..].to_string();
        }
        self.context = ctx;
        self.pending = self.pending[rp..].to_string();
        Ok(out)
    }
}
