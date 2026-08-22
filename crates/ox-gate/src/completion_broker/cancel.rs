//! Block-run cancellation.
//!
//! A Block's blocking substrate reads can park for the whole inter-token
//! gap of an upstream stream. When the handle the Block serves is GC'd
//! (client disconnect, teardown), nothing would ever unpark it — the run
//! would hold its downstream handles until the upstream happened to emit.
//! The host cancels instead: GC triggers this handle, the Block backing
//! fails the parked read, and the Block unwinds through its normal
//! error path, GC'ing its own downstream handles on the way out.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

#[derive(Clone, Default)]
pub struct CancelHandle {
    inner: Arc<CancelInner>,
}

#[derive(Default)]
struct CancelInner {
    flag: AtomicBool,
    notify: Notify,
}

impl CancelHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.inner.flag.store(true, Ordering::Release);
        self.inner.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.flag.load(Ordering::Acquire)
    }

    /// Resolves when cancelled. Enable-before-check: notify_waiters()
    /// stores no permit, so the Notified future must be enabled before
    /// the flag test or a cancel landing between them is lost forever.
    pub async fn cancelled(&self) {
        let notified = self.inner.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}
