//! In-memory operation log with a fixed-size ring buffer.
//!
//! Every tool invocation appends one entry; oldest entries are evicted
//! once the buffer is full. The assistant queries this via
//! `get_recent_tool_calls` to self-diagnose during a session — when a
//! call hangs or returns unexpectedly, the previous N calls give the
//! shape of the workload that produced the symptom.
//!
//! The log lives in process memory only; persistence is intentionally
//! out of scope until a real need surfaces (and would belong behind an
//! env-var gate to keep the default surface boring).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use sha2::{Digest, Sha256};

/// Default ring-buffer capacity. Picked so a long session of dense tool
/// use still has hours of history without blowing memory — each entry
/// is a few hundred bytes.
pub const DEFAULT_CAPACITY: usize = 1024;

/// One recorded tool invocation. Times are unix-millis since epoch so
/// they survive serialisation across process boundaries unambiguously.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OpLogEntry {
    pub tool: String,
    /// Hex-encoded SHA-256 of the canonical JSON params. Stable for
    /// identical inputs across runs, so a caller can detect that two
    /// invocations were "the same call".
    pub params_hash: String,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    pub duration_ms: u64,
    pub status: OpStatus,
    /// Truncated error text, when status is Err. We bound the length to
    /// keep the log compact even if a backend returns a large error
    /// payload.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OpStatus {
    Ok,
    Err,
}

const MAX_ERROR_LEN: usize = 512;

#[derive(Debug, Default, Clone)]
pub struct OpLog {
    inner: Arc<OpLogInner>,
}

#[derive(Debug)]
struct OpLogInner {
    entries: RwLock<VecDeque<OpLogEntry>>,
    capacity: usize,
    seq: AtomicU64,
}

impl Default for OpLogInner {
    fn default() -> Self {
        Self {
            entries: RwLock::new(VecDeque::with_capacity(DEFAULT_CAPACITY)),
            capacity: DEFAULT_CAPACITY,
            seq: AtomicU64::new(0),
        }
    }
}

impl OpLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(OpLogInner {
                entries: RwLock::new(VecDeque::with_capacity(capacity.max(1))),
                capacity: capacity.max(1),
                seq: AtomicU64::new(0),
            }),
        }
    }

    /// Open a recording handle for a tool invocation. The returned
    /// [`OpLogRecorder`] captures the start time and computes the
    /// params hash; calling [`OpLogRecorder::finish_ok`] or
    /// [`OpLogRecorder::finish_err`] appends the entry and evicts the
    /// oldest if the buffer is full.
    pub fn record(&self, tool: impl Into<String>, params: &serde_json::Value) -> OpLogRecorder {
        let started = SystemTime::now();
        let started_unix_ms = unix_ms(started);
        let canonical = canonical_json(params);
        let params_hash = hex_sha256(canonical.as_bytes());
        OpLogRecorder {
            log: self.clone(),
            tool: tool.into(),
            params_hash,
            started,
            started_unix_ms,
        }
    }

    /// Bypass the recorder pattern — useful for callers that already
    /// have their own timing and just need to append. Most callers
    /// should use [`OpLog::record`] instead.
    pub fn append(&self, entry: OpLogEntry) {
        let mut guard = self.inner.entries.write();
        if guard.len() == self.inner.capacity {
            guard.pop_front();
        }
        guard.push_back(entry);
        self.inner.seq.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot the most recent `limit` entries, optionally filtered to
    /// those finished at or after `since_unix_ms`. Newest first.
    pub fn recent(&self, limit: usize, since_unix_ms: Option<u64>) -> Vec<OpLogEntry> {
        let guard = self.inner.entries.read();
        guard
            .iter()
            .rev()
            .filter(|e| since_unix_ms.is_none_or(|since| e.finished_at_unix_ms >= since))
            .take(limit)
            .cloned()
            .collect()
    }

    /// Most recent failure across all tools, if any. `workflowy_status`
    /// surfaces this so callers can see which mutation last broke without
    /// scrolling through the full op log. None when no `Err` entries have
    /// been recorded since startup.
    pub fn last_failure(&self) -> Option<OpLogEntry> {
        let guard = self.inner.entries.read();
        guard
            .iter()
            .rev()
            .find(|e| matches!(e.status, OpStatus::Err))
            .cloned()
    }

    pub fn len(&self) -> usize {
        self.inner.entries.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.entries.read().is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }

    /// Total number of entries ever appended (including evicted ones).
    /// Used by tests and by status responses to confirm the log is alive.
    pub fn total_recorded(&self) -> u64 {
        self.inner.seq.load(Ordering::Relaxed)
    }
}

/// Open recording for one in-flight call. Drops without recording if
/// the handler panics — that is intentional, we want the log to reflect
/// returns, not entries.
pub struct OpLogRecorder {
    log: OpLog,
    tool: String,
    params_hash: String,
    started: SystemTime,
    started_unix_ms: u64,
}

impl OpLogRecorder {
    pub fn finish_ok(self) {
        self.finish_inner(OpStatus::Ok, None);
    }

    pub fn finish_err(self, error: impl AsRef<str>) {
        let err = error.as_ref();
        let truncated = if err.len() > MAX_ERROR_LEN {
            format!("{}…[truncated]", &err[..MAX_ERROR_LEN])
        } else {
            err.to_string()
        };
        self.finish_inner(OpStatus::Err, Some(truncated));
    }

    fn finish_inner(self, status: OpStatus, error: Option<String>) {
        let finished = SystemTime::now();
        let finished_unix_ms = unix_ms(finished);
        let duration_ms = finished
            .duration_since(self.started)
            .unwrap_or(Duration::ZERO)
            .as_millis() as u64;
        let entry = OpLogEntry {
            tool: self.tool,
            params_hash: self.params_hash,
            started_at_unix_ms: self.started_unix_ms,
            finished_at_unix_ms: finished_unix_ms,
            duration_ms,
            status,
            error,
        };
        self.log.append(entry);
    }
}

fn unix_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        use std::fmt::Write;
        let _ = write!(hex, "{:02x}", byte);
    }
    hex
}

/// Canonical JSON: keys sorted, no whitespace. Two semantically equal
/// payloads produce the same hash regardless of producer formatting.
fn canonical_json(value: &serde_json::Value) -> String {
    let canonical = sort_keys(value);
    serde_json::to_string(&canonical).unwrap_or_else(|_| String::from("null"))
}

fn sort_keys(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .iter()
                .map(|(k, v)| (k.clone(), sort_keys(v)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(sort_keys).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ring_buffer_evicts_oldest_when_full() {
        let log = OpLog::with_capacity(3);
        for i in 0..5 {
            let r = log.record("noop", &json!({ "i": i }));
            r.finish_ok();
        }
        assert_eq!(log.len(), 3);
        assert_eq!(log.total_recorded(), 5);
        let recent = log.recent(10, None);
        // Most recent first: i=4, i=3, i=2.
        assert_eq!(recent[0].tool, "noop");
        // params_hash must differ between distinct payloads.
        assert_ne!(recent[0].params_hash, recent[1].params_hash);
    }

    #[test]
    fn params_hash_is_stable_across_key_orderings() {
        let log = OpLog::new();
        let r1 = log.record("t", &json!({ "a": 1, "b": 2 }));
        let r2 = log.record("t", &json!({ "b": 2, "a": 1 }));
        r1.finish_ok();
        r2.finish_ok();
        let entries = log.recent(2, None);
        assert_eq!(entries[0].params_hash, entries[1].params_hash);
    }

    #[test]
    fn finish_err_records_error_status_and_truncates() {
        let log = OpLog::new();
        let long = "x".repeat(MAX_ERROR_LEN + 100);
        let r = log.record("t", &json!({}));
        r.finish_err(&long);
        let entries = log.recent(1, None);
        assert_eq!(entries[0].status, OpStatus::Err);
        let err = entries[0].error.as_deref().unwrap();
        assert!(err.len() <= MAX_ERROR_LEN + 32, "error truncation failed: len={}", err.len());
        assert!(err.ends_with("[truncated]"));
    }

    #[test]
    fn since_filter_returns_only_newer_entries() {
        let log = OpLog::new();
        for _ in 0..3 {
            let r = log.record("t", &json!({}));
            r.finish_ok();
        }
        let cutoff = log.recent(10, None)[1].finished_at_unix_ms;
        // Strictly: include the cutoff entry and anything newer.
        let after = log.recent(10, Some(cutoff));
        assert!(after.len() >= 2);
        for entry in &after {
            assert!(entry.finished_at_unix_ms >= cutoff);
        }
    }

    #[test]
    fn ok_recorder_finishes_with_ok_status() {
        let log = OpLog::new();
        let r = log.record("t", &json!({"k": "v"}));
        r.finish_ok();
        let entries = log.recent(1, None);
        assert_eq!(entries[0].status, OpStatus::Ok);
        assert!(entries[0].error.is_none());
        assert!(entries[0].duration_ms < 5_000); // sanity
    }
}
