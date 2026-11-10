//! Desktop notifications. Bodies name categories and actions only; never values.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Notification {
    pub kind: String,
    pub title: String,
    pub body: String,
    pub categories: Vec<String>,
    /// RFC 3339 time the notification was raised.
    #[serde(default)]
    pub at: String,
}

pub trait Notifier: Send + Sync {
    fn notify(&self, n: Notification);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationSettings {
    pub enabled: bool,
    /// Notify only for these kinds: block, review, redaction, private_route, error.
    pub kinds: Vec<String>,
    pub min_interval_secs: u64,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        NotificationSettings { enabled: true, kinds: vec!["block".into(), "review".into(), "redaction".into(), "private_route".into(), "error".into()], min_interval_secs: 20 }
    }
}

/// Rate-limited notifier wrapper: at most one banner per kind per interval;
/// suppressed notifications are counted into the next banner.
pub struct RateLimited<N: Notifier> {
    inner: N,
    pub settings: Mutex<NotificationSettings>,
    last: Mutex<HashMap<String, (Instant, u32)>>,
}

impl<N: Notifier> RateLimited<N> {
    pub fn new(inner: N, settings: NotificationSettings) -> Self {
        RateLimited { inner, settings: Mutex::new(settings), last: Mutex::new(HashMap::new()) }
    }
}

impl<N: Notifier> Notifier for RateLimited<N> {
    fn notify(&self, mut n: Notification) {
        let s = self.settings.lock().unwrap().clone();
        if !s.enabled || !s.kinds.contains(&n.kind) {
            return;
        }
        let mut last = self.last.lock().unwrap();
        let now = Instant::now();
        let e = last.entry(n.kind.clone()).or_insert((now - Duration::from_secs(s.min_interval_secs + 1), 0));
        if now.duration_since(e.0) < Duration::from_secs(s.min_interval_secs) {
            e.1 += 1;
            return;
        }
        if e.1 > 0 {
            n.body = format!("{} (+{} similar)", n.body, e.1);
        }
        *e = (now, 0);
        drop(last);
        self.inner.notify(n);
    }
}

/// Native desktop banners via osascript (macOS) or notify-send (Linux).
pub struct Desktop;

fn sanitize(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).map(|c| if c == '"' || c == '\\' { '\'' } else { c }).take(200).collect()
}

impl Notifier for Desktop {
    fn notify(&self, n: Notification) {
        let title = sanitize(&n.title);
        let body = sanitize(&n.body);
        std::thread::spawn(move || {
            let _ = if cfg!(target_os = "macos") {
                std::process::Command::new("osascript").arg("-e").arg(format!("display notification \"{body}\" with title \"Prompt VPN\" subtitle \"{title}\"")).output()
            } else {
                std::process::Command::new("notify-send").arg("--app-name=Prompt VPN").arg(format!("Prompt VPN: {title}")).arg(body).output()
            };
        });
    }
}

/// Collects notifications (tests and the dashboard's recent list).
#[derive(Default)]
pub struct Recorder {
    pub sent: Mutex<Vec<Notification>>,
}

impl Notifier for Recorder {
    fn notify(&self, mut n: Notification) {
        if n.at.is_empty() {
            n.at = chrono::Utc::now().to_rfc3339();
        }
        let mut v = self.sent.lock().unwrap();
        v.push(n);
        if v.len() > 500 {
            v.drain(..100);
        }
    }
}

impl<T: Notifier + ?Sized> Notifier for std::sync::Arc<T> {
    fn notify(&self, n: Notification) {
        (**self).notify(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn rate_limit_and_filter() {
        let rec = Arc::new(Recorder::default());
        let rl = RateLimited::new(rec.clone(), NotificationSettings { enabled: true, kinds: vec!["block".into()], min_interval_secs: 60 });
        let n = |k: &str| Notification { kind: k.into(), title: "t".into(), body: "b".into(), categories: vec![], at: String::new() };
        rl.notify(n("block"));
        rl.notify(n("block"));
        rl.notify(n("redaction"));
        assert_eq!(rec.sent.lock().unwrap().len(), 1);
    }

    #[test]
    fn sanitize_strips_quotes() {
        assert_eq!(sanitize("a\"b\\c\n"), "a'b'c");
    }
}
