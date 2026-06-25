//! A small in-memory work queue for SSH-pull sync. Producers (the connect
//! trigger, the periodic loop, manual requests) enqueue the per-file work a scan
//! found; a pool of background workers drains it. This decouples the slow part
//! (pulling tens of MB over scp) from the request that asked for it, so the UI
//! never blocks — and lets the header show how much is queued.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, Notify};

/// One file to pull/reconcile for a segment.
#[derive(Clone)]
pub struct QueueItem {
    pub dongle: String,
    pub addr: String,
    pub ts: String,
    pub seg: i64,
    pub file: String,
    pub key: String,
}

#[derive(Default)]
struct Inner {
    pending: VecDeque<QueueItem>,
    pending_keys: HashSet<String>,
    /// key -> (route ts, file), for the items currently being processed.
    active: HashMap<String, (String, String)>,
}

/// Cloneable handle to the shared queue.
#[derive(Clone, Default)]
pub struct SyncQueue {
    inner: Arc<Mutex<Inner>>,
    notify: Arc<Notify>,
}

impl SyncQueue {
    /// Add items that aren't already queued or in flight (deduped by blob key).
    /// Returns how many were newly enqueued.
    pub async fn enqueue(&self, items: Vec<QueueItem>) -> usize {
        let mut added = 0;
        {
            let mut s = self.inner.lock().await;
            for it in items {
                if s.pending_keys.contains(&it.key) || s.active.contains_key(&it.key) {
                    continue;
                }
                s.pending_keys.insert(it.key.clone());
                s.pending.push_back(it);
                added += 1;
            }
        }
        if added > 0 {
            self.notify.notify_waiters();
        }
        added
    }

    /// Take the next item, awaiting until one is available. The 1s timeout makes
    /// the wait self-healing against any missed notification.
    pub async fn next(&self) -> QueueItem {
        loop {
            {
                let mut s = self.inner.lock().await;
                if let Some(it) = s.pending.pop_front() {
                    s.pending_keys.remove(&it.key);
                    s.active.insert(it.key.clone(), (it.ts.clone(), it.file.clone()));
                    return it;
                }
            }
            let _ = tokio::time::timeout(Duration::from_secs(1), self.notify.notified()).await;
        }
    }

    /// Mark an item done (success or failure).
    pub async fn done(&self, key: &str) {
        self.inner.lock().await.active.remove(key);
    }

    /// `(drives, files)` currently queued or in flight — for the UI counter.
    pub async fn stats(&self) -> (usize, usize) {
        let s = self.inner.lock().await;
        let mut drives: HashSet<&str> = HashSet::new();
        for it in &s.pending {
            drives.insert(it.ts.as_str());
        }
        for (ts, _) in s.active.values() {
            drives.insert(ts.as_str());
        }
        (drives.len(), s.pending.len() + s.active.len())
    }

    /// The queue contents for the detail page: `(ts, file, in_flight)`, in-flight
    /// items first.
    pub async fn detail(&self) -> Vec<(String, String, bool)> {
        let s = self.inner.lock().await;
        let mut out: Vec<(String, String, bool)> = Vec::new();
        for (ts, file) in s.active.values() {
            out.push((ts.clone(), file.clone(), true));
        }
        for it in &s.pending {
            out.push((it.ts.clone(), it.file.clone(), false));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(ts: &str, file: &str) -> QueueItem {
        QueueItem {
            dongle: "d".into(),
            addr: "a".into(),
            ts: ts.into(),
            seg: 0,
            file: file.into(),
            key: format!("{ts}-{file}"),
        }
    }

    #[tokio::test]
    async fn enqueue_dedups_and_counts() {
        let q = SyncQueue::default();
        let added = q
            .enqueue(vec![item("r1", "qcamera.ts"), item("r1", "fcamera.hevc"), item("r2", "qcamera.ts")])
            .await;
        assert_eq!(added, 3);
        assert_eq!(q.enqueue(vec![item("r1", "qcamera.ts")]).await, 0, "dup key ignored");
        assert_eq!(q.stats().await, (2, 3), "2 drives, 3 files");

        // next() moves an item to in-flight; it's still counted until done().
        let it = q.next().await;
        assert_eq!(q.stats().await.1, 3);
        q.done(&it.key).await;
        assert_eq!(q.stats().await.1, 2);
    }
}
