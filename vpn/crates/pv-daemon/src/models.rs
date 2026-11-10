//! Span-model lifecycle: pinned GLiNER downloads with sha256 verification,
//! size selection, loading into the detector. No generative models.

use crate::state::Paths;
use pv_detectors::Detector;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const MODELS_LOCK: &str = include_str!("../../../models.lock.json");

pub fn lock() -> Value {
    serde_json::from_str(MODELS_LOCK).expect("models.lock.json is valid")
}

pub fn default_model() -> String {
    lock()["default_span_model"].as_str().unwrap_or("gliner-pii-edge").to_string()
}

pub fn known_models() -> Vec<String> {
    lock()["span"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default()
}

/// Accepts `edge`, `small`, `base` or the full name.
pub fn resolve_name(s: &str) -> Option<String> {
    let full = if s.starts_with("gliner-pii-") { s.to_string() } else { format!("gliner-pii-{s}") };
    known_models().contains(&full).then_some(full)
}

/// Owner's span-model choice (persisted as setting `span_model`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanSelection {
    pub model: String,
}

impl Default for SpanSelection {
    fn default() -> Self {
        SpanSelection { model: default_model() }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SpanStatus {
    pub compiled: bool,
    pub model: String,
    pub revision: String,
    pub provisioned: bool,
    pub loaded: bool,
    pub size_bytes: u64,
    pub installing: bool,
    pub error: Option<String>,
}

pub struct ModelManager {
    paths: Paths,
    detector: Arc<Detector>,
    pub span: Mutex<SpanStatus>,
}

pub fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    }
}

fn files(model: &str) -> Vec<(String, String, String, u64)> {
    lock()["span"][model]["files"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|(local, f)| (local, f["path"].as_str().unwrap_or_default().to_string(), f["sha256"].as_str().unwrap_or_default().to_string(), f["size"].as_u64().unwrap_or(0)))
        .collect()
}

impl ModelManager {
    pub fn new(paths: Paths, detector: Arc<Detector>) -> Self {
        ModelManager { paths, detector, span: Mutex::new(SpanStatus { compiled: cfg!(feature = "gliner"), ..Default::default() }) }
    }

    pub fn dir(&self, model: &str) -> std::path::PathBuf {
        self.paths.models().join(model)
    }

    pub fn provisioned(&self, model: &str) -> Result<(), String> {
        let fs = files(model);
        if fs.is_empty() {
            return Err(format!("unknown model {model}"));
        }
        for (local, _, sha, _) in fs {
            let b = std::fs::read(self.dir(model).join(&local)).map_err(|_| format!("missing {local}"))?;
            if hex::encode(Sha256::digest(&b)) != sha {
                return Err(format!("{local} hash mismatch"));
            }
        }
        Ok(())
    }

    /// Download pinned files from Hugging Face and verify sha256 before use.
    pub async fn provision(&self, model: &str) -> Result<(), String> {
        let l = lock();
        let g = &l["span"][model];
        if g.is_null() {
            return Err(format!("unknown model {model}"));
        }
        self.span.lock().unwrap().installing = true;
        let r: Result<(), String> = async {
            let repo = g["source"].as_str().unwrap_or_default().trim_start_matches("https://huggingface.co/").to_string();
            let rev = g["revision"].as_str().unwrap_or_default().to_string();
            let http = reqwest::Client::builder().timeout(Duration::from_secs(1200)).build().map_err(|e| e.to_string())?;
            std::fs::create_dir_all(self.dir(model)).map_err(|e| e.to_string())?;
            for (local, remote, sha, _) in files(model) {
                let dest = self.dir(model).join(&local);
                if let Ok(b) = std::fs::read(&dest) {
                    if hex::encode(Sha256::digest(&b)) == sha {
                        continue;
                    }
                }
                let url = format!("https://huggingface.co/{repo}/resolve/{rev}/{remote}");
                let resp = http.get(&url).send().await.map_err(|e| format!("download {local}: {e}"))?;
                if !resp.status().is_success() {
                    return Err(format!("download {local}: HTTP {}", resp.status()));
                }
                let b = resp.bytes().await.map_err(|e| e.to_string())?;
                if hex::encode(Sha256::digest(&b)) != sha {
                    return Err(format!("{local}: sha256 mismatch; refusing to install"));
                }
                let tmp = dest.with_extension("part");
                std::fs::write(&tmp, &b).map_err(|e| e.to_string())?;
                std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
        .await;
        self.span.lock().unwrap().installing = false;
        r
    }

    /// Load the selected model if compiled in and provisioned. Returns whether it loaded.
    pub fn load(&self, model: &str) -> bool {
        let l = lock();
        let g = &l["span"][model];
        let mut st = self.span.lock().unwrap();
        st.model = model.to_string();
        st.revision = g["revision"].as_str().unwrap_or_default().to_string();
        st.size_bytes = files(model).iter().map(|f| f.3).sum();
        st.provisioned = self.provisioned(model).is_ok();
        st.loaded = false;
        if !st.compiled {
            st.error = Some("built without the gliner feature".into());
            self.detector.set_span(None);
            return false;
        }
        if let Err(e) = self.provisioned(model) {
            st.error = Some(format!("not installed ({e}); run `promptvpn model install`"));
            self.detector.set_span(None);
            return false;
        }
        #[cfg(feature = "gliner")]
        {
            let dir = self.dir(model);
            let thr = g["threshold"].as_f64().unwrap_or(0.5) as f32;
            let mode = g["span_mode"].as_str().unwrap_or("token_level");
            let version = format!("{model}@{}", &st.revision[..7.min(st.revision.len())]);
            match pv_detectors::span::gliner_impl::GlinerModel::load(&dir.join("tokenizer.json").to_string_lossy(), &dir.join("model.onnx").to_string_lossy(), thr, &version, mode) {
                Ok(m) => {
                    self.detector.set_span(Some(Arc::new(m)));
                    st.loaded = true;
                    st.error = None;
                }
                Err(e) => {
                    self.detector.set_span(None);
                    st.error = Some(e);
                }
            }
        }
        st.loaded
    }

    pub fn status(&self) -> Value {
        let l = lock();
        let installed: Vec<String> = known_models().into_iter().filter(|m| self.provisioned(m).is_ok()).collect();
        let options: Vec<Value> = known_models()
            .into_iter()
            .map(|m| serde_json::json!({"model": m, "size_bytes": files(&m).iter().map(|f| f.3).sum::<u64>(), "revision": l["span"][&m]["revision"], "installed": installed.contains(&m), "default": m == default_model()}))
            .collect();
        serde_json::json!({"span": *self.span.lock().unwrap(), "options": options, "lock": l})
    }
}
