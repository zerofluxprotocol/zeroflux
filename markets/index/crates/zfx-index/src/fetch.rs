//! Polite HTTP client. Every request:
//! - goes through `RateBudget` (minimum gap + random jitter + rolling 24 h cap,
//!   counted from the ledger so restarts cannot reset it),
//! - identifies itself with a generic, honest User-Agent,
//! - is recorded in the ledger with raw bytes, selected headers, timestamp and SHA-256,
//!   including failures.
//!
//! No cookies, no browser emulation, no attempt to bypass Cloudflare or any other
//! control. A 403/429/challenge response is recorded and the poller backs off.

use crate::ledger::{FetchRecord, Ledger, LedgerError};
use crate::source::Kind;
use crate::time::now_ms;
use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

pub const USER_AGENT: &str = "zfx-index/0.1 (+https://github.com/zerofluxprotocol/zeroflux/tree/main/markets/index; public-data index research)";
pub const CLIENT_NAME: &str = "zfx-index/0.1";
pub const BASE: &str = "https://openrouter.ai";
const MAX_BODY: u64 = 32 * 1024 * 1024;

/// Response headers kept in the ledger.
pub const KEPT_HEADERS: &[&str] = &[
    "content-type",
    "content-length",
    "date",
    "last-modified",
    "etag",
    "cache-control",
    "age",
    "cf-cache-status",
    "cf-ray",
    "retry-after",
    "x-ratelimit-limit",
    "x-ratelimit-remaining",
];

#[derive(Clone, Debug)]
pub struct RateBudget {
    /// Minimum milliseconds between any two requests.
    pub min_gap_ms: i64,
    /// Uniform random extra delay in [0, jitter_ms).
    pub jitter_ms: i64,
    /// Maximum requests in any rolling 24 hours.
    pub daily_cap: usize,
}

impl Default for RateBudget {
    fn default() -> Self {
        RateBudget { min_gap_ms: 6_000, jitter_ms: 4_000, daily_cap: 400 }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Admission {
    /// Wait this many ms, then send.
    WaitMs(i64),
    /// The rolling 24 h cap is used up until this unix ms.
    CapReachedUntil(i64),
}

impl RateBudget {
    /// Decides when the next request may go out, given past request times (unix ms).
    /// `jitter_draw` is in [0, 1_000_000) so tests can pass a fixed value.
    pub fn admit(&self, now: i64, history: &[i64], jitter_draw: u32) -> Admission {
        let day_ago = now - 86_400_000;
        let mut recent: Vec<i64> = history.iter().copied().filter(|t| *t > day_ago).collect();
        recent.sort_unstable();
        if recent.len() >= self.daily_cap {
            let idx = recent.len() - self.daily_cap;
            return Admission::CapReachedUntil(recent[idx] + 86_400_000 + 1);
        }
        let jitter = self.jitter_ms * jitter_draw as i64 / 1_000_000;
        let last = recent.last().copied().unwrap_or(i64::MIN / 2);
        let earliest = last + self.min_gap_ms + jitter;
        Admission::WaitMs((earliest - now).max(jitter))
    }
}

pub fn random_u32_below(n: u32) -> u32 {
    let mut b = [0u8; 4];
    getrandom::getrandom(&mut b).expect("os rng");
    u32::from_le_bytes(b) % n.max(1)
}

pub struct Fetcher<'a> {
    pub ledger: &'a Ledger,
    pub budget: RateBudget,
    agent: ureq::Agent,
    pub api_key: Option<String>,
    pub dry_log: bool,
}

#[derive(Debug)]
pub enum FetchError {
    Budget(i64),
    Ledger(LedgerError),
    /// Transport or HTTP error; still recorded in the ledger.
    Http(String, Option<FetchRecord>),
}
impl From<LedgerError> for FetchError {
    fn from(e: LedgerError) -> Self {
        FetchError::Ledger(e)
    }
}
impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Budget(t) => write!(f, "rate budget exhausted until {}", crate::time::fmt_ms(*t)),
            FetchError::Ledger(e) => write!(f, "{e}"),
            FetchError::Http(e, _) => write!(f, "http: {e}"),
        }
    }
}

impl<'a> Fetcher<'a> {
    pub fn new(ledger: &'a Ledger, budget: RateBudget) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .user_agent(USER_AGENT)
            .redirects(2)
            .build();
        Fetcher { ledger, budget, agent, api_key: std::env::var("OPENROUTER_API_KEY").ok().filter(|s| !s.is_empty()), dry_log: false }
    }

    fn history(&self) -> Result<Vec<i64>, LedgerError> {
        Ok(self.ledger.fetches()?.iter().map(|f| f.fetched_at_ms).collect())
    }

    /// Fetches `path` (relative to openrouter.ai), waits for the rate budget,
    /// records everything. Returns the ledger record and body for 2xx responses.
    pub fn get(&self, kind: Kind, subject: &str, path: &str) -> Result<(FetchRecord, Vec<u8>), FetchError> {
        match self.budget.admit(now_ms(), &self.history()?, random_u32_below(1_000_000)) {
            Admission::CapReachedUntil(t) => return Err(FetchError::Budget(t)),
            Admission::WaitMs(w) if w > 0 => std::thread::sleep(Duration::from_millis(w as u64)),
            Admission::WaitMs(_) => {}
        }
        let url = format!("{BASE}{path}");
        let mut req = self.agent.get(&url).set("Accept", "application/json");
        if kind == Kind::RankingsDailyApi {
            if let Some(k) = &self.api_key {
                req = req.set("Authorization", &format!("Bearer {k}"));
            }
        }
        let t0 = now_ms();
        let result = req.call();
        let (status, headers, body, error) = match result {
            Ok(resp) => {
                let (s, h) = (resp.status(), keep_headers(&resp));
                let mut b = Vec::new();
                let e = resp.into_reader().take(MAX_BODY).read_to_end(&mut b).err().map(|e| e.to_string());
                (s, h, b, e)
            }
            Err(ureq::Error::Status(s, resp)) => {
                let h = keep_headers(&resp);
                let mut b = Vec::new();
                let _ = resp.into_reader().take(MAX_BODY).read_to_end(&mut b);
                (s, h, b, Some(format!("HTTP {s}")))
            }
            Err(e) => (0, BTreeMap::new(), Vec::new(), Some(e.to_string())),
        };
        let sha = if body.is_empty() { String::new() } else { self.ledger.put_blob(&body)? };
        let mut rec = FetchRecord {
            seq: self.ledger.next_fetch_seq()?,
            kind: kind.name().into(),
            subject: subject.into(),
            documented: kind.documented(),
            method: "GET".into(),
            url, // never contains secrets: the API key travels in a header
            status,
            fetched_at_ms: t0,
            headers,
            len: body.len() as u64,
            sha256: sha,
            client: CLIENT_NAME.into(),
            error: error.clone(),
            chain: String::new(),
        };
        self.ledger.append_fetch(&mut rec)?;
        if !self.dry_log {
            eprintln!("[fetch] #{} {} {} {} bytes {}", rec.seq, rec.status, rec.url, rec.len, error.as_deref().unwrap_or(""));
        }
        match error {
            None if (200..300).contains(&status) => Ok((rec, body)),
            Some(e) => Err(FetchError::Http(e, Some(rec))),
            None => Err(FetchError::Http(format!("HTTP {status}"), Some(rec))),
        }
    }
}

fn keep_headers(resp: &ureq::Response) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for h in KEPT_HEADERS {
        if let Some(v) = resp.header(h) {
            m.insert(h.to_string(), v.to_string());
        }
    }
    m
}

pub fn enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

/// Effective pricing with an explicit chart range ("3d", "1w", "1m", "3m", "1y", "all").
/// The summary (weighted prices, provider token totals) does not depend on the range.
pub fn pricing_path(permaslug: &str, range: &str) -> String {
    format!("/api/frontend/v1/stats/effective-pricing?permaslug={}&variant=standard&shape=v7&range={range}", enc(permaslug))
}

pub fn path_for(kind: Kind, subject: &str) -> String {
    match kind {
        Kind::RankingsDay => "/api/frontend/v1/rankings/models?view=day".into(),
        Kind::RankingsDaySpend => "/api/frontend/v1/rankings/models?view=day&metric=spend".into(),
        Kind::ModelActivity => format!("/api/frontend/v1/stats/model-activity?permaslug={}&variant=standard", enc(subject)),
        Kind::EffectivePricing => pricing_path(subject, "all"),
        Kind::CatalogModels => "/api/v1/models".into(),
        // subject is the catalog model id, e.g. anthropic/claude-opus-5.5
        Kind::ModelEndpoints => format!("/api/v1/models/{subject}/endpoints"),
        Kind::RankingsDailyApi => format!("/api/v1/datasets/rankings-daily?period=day&{subject}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_gap_jitter_and_cap() {
        let b = RateBudget { min_gap_ms: 6_000, jitter_ms: 4_000, daily_cap: 3 };
        let now = 1_000_000_000;
        assert_eq!(b.admit(now, &[], 0), Admission::WaitMs(0));
        assert_eq!(b.admit(now, &[], 500_000), Admission::WaitMs(2_000));
        assert_eq!(b.admit(now, &[now - 1_000], 0), Admission::WaitMs(5_000));
        assert_eq!(b.admit(now, &[now - 1_000], 999_999), Admission::WaitMs(8_999));
        assert_eq!(b.admit(now, &[now - 60_000], 0), Admission::WaitMs(0));
        let h = [now - 3_000_000, now - 2_000_000, now - 1_000_000];
        assert_eq!(b.admit(now, &h, 0), Admission::CapReachedUntil(now - 3_000_000 + 86_400_000 + 1));
        // requests older than 24 h no longer count
        let old = [now - 90_000_000, now - 89_000_000, now - 88_000_000];
        assert_eq!(b.admit(now, &old, 0), Admission::WaitMs(0));
    }

    #[test]
    fn default_budget_is_conservative() {
        let b = RateBudget::default();
        // at most one request per 6 s, and fewer than the documented 30/min API limit
        assert!(b.min_gap_ms >= 2_000);
        assert!(b.daily_cap <= 500);
    }

    #[test]
    fn paths() {
        assert_eq!(
            path_for(Kind::ModelActivity, "anthropic/claude-opus-5.5-20260921"),
            "/api/frontend/v1/stats/model-activity?permaslug=anthropic%2Fclaude-opus-5.5-20260921&variant=standard"
        );
    }
}
