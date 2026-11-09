//! Persistent bridge job store: idempotent, leased, with bounded
//! exponential backoff. Written atomically (tmp + fsync + rename).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JobState {
    Observed,
    AwaitingFinality,
    Attesting,
    ReadyToSubmit,
    Submitted,
    Finalized,
    Reorged,
    Paused,
    FailedNeedsReview,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Mint,
    Release,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub kind: JobKind,
    pub state: JobState,
    pub data: serde_json::Value,
    pub approvals: Vec<serde_json::Value>,
    pub attempts: u32,
    pub next_attempt_ms: u64,
    pub lease_until_ms: u64,
    pub submission: Option<serde_json::Value>,
    pub history: Vec<(u64, String)>,
    pub created_ms: u64,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

impl Job {
    pub fn new(id: String, kind: JobKind, data: serde_json::Value) -> Job {
        let t = now_ms();
        Job {
            id,
            kind,
            state: JobState::Observed,
            data,
            approvals: vec![],
            attempts: 0,
            next_attempt_ms: 0,
            lease_until_ms: 0,
            submission: None,
            history: vec![(t, "OBSERVED".into())],
            created_ms: t,
        }
    }

    pub fn to(&mut self, s: JobState, note: &str) {
        if self.state != s || !note.is_empty() {
            self.history.push((now_ms(), format!("{:?} {}", s, note)));
            if self.history.len() > 50 {
                self.history.remove(1);
            }
        }
        self.state = s;
    }

    /// Bounded exponential backoff with jitter: 0.5 s .. 30 s.
    pub fn backoff(&mut self, why: &str) {
        self.attempts += 1;
        let base = 500u64.saturating_mul(1u64 << self.attempts.min(6));
        let jitter = rand::random::<u64>() % 250;
        self.next_attempt_ms = now_ms() + base.min(30_000) + jitter;
        self.history.push((now_ms(), format!("retry #{}: {}", self.attempts, why)));
        if self.history.len() > 50 {
            self.history.remove(1);
        }
    }

    pub fn terminal(&self) -> bool {
        // REORGED is not terminal: a reorged deposit is usually mined again.
        matches!(self.state, JobState::Finalized | JobState::FailedNeedsReview)
    }
}

#[derive(Default, Serialize, Deserialize)]
pub struct JobStore {
    pub jobs: BTreeMap<String, Job>,
    pub native_cursor: u64,
    pub near_burn_cursor: u64,
    pub halted: Option<String>,
    #[serde(skip)]
    path: String,
}

impl JobStore {
    pub fn open(path: &str) -> Result<JobStore, String> {
        let mut s: JobStore = match std::fs::read_to_string(path) {
            Ok(t) => serde_json::from_str(&t).map_err(|e| format!("job store {} corrupt: {}", path, e))?,
            Err(_) => JobStore::default(),
        };
        s.path = path.to_string();
        Ok(s)
    }

    pub fn save(&self) -> Result<(), String> {
        let tmp = format!("{}.tmp", self.path);
        let data = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
            f.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
            f.sync_all().map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    /// Insert if new (idempotent on duplicate event delivery).
    pub fn observe(&mut self, id: &str, kind: JobKind, data: serde_json::Value) -> bool {
        if self.jobs.contains_key(id) {
            return false;
        }
        self.jobs.insert(id.to_string(), Job::new(id.to_string(), kind, data));
        true
    }

    pub fn counts(&self) -> BTreeMap<String, usize> {
        let mut m = BTreeMap::new();
        for j in self.jobs.values() {
            *m.entry(format!("{:?}", j.state)).or_insert(0) += 1;
        }
        m
    }
}
