//! Progress of a long read (an image on a network file system, a DICOM
//! series to decode), for the `load` request that answers while it runs.
//! The reading code reports through the thread's current `Progress` (none
//! outside such a load): bytes read, or frames decoded, out of a total;
//! a cancelled load makes the readers stop with an error.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Default)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    /// The units are not bytes (decoded frames): no transfer rate.
    frames: AtomicBool,
    cancelled: AtomicBool,
}

impl Progress {
    pub fn fraction(&self) -> f64 {
        let (d, t) = (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed));
        if t == 0 {
            0.0
        } else {
            (d as f64 / t as f64).min(1.0)
        }
    }

    /// Bytes read so far, None when counting frames.
    pub fn bytes(&self) -> Option<u64> {
        (!self.frames.load(Ordering::Relaxed)).then(|| self.done.load(Ordering::Relaxed))
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Arc<Progress>>> = const { RefCell::new(None) };
}

/// Report to `p` from this thread (None: stop reporting).
pub fn set(p: Option<Arc<Progress>>) {
    CURRENT.with(|c| *c.borrow_mut() = p);
}

pub fn current() -> Option<Arc<Progress>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// More work to do: `n` bytes (or frames).
pub fn expect(n: u64, frames: bool) {
    if let Some(p) = current() {
        p.total.fetch_add(n, Ordering::Relaxed);
        if frames {
            p.frames.store(true, Ordering::Relaxed);
        }
    }
}

/// `n` more done.
pub fn advance(n: u64) {
    if let Some(p) = current() {
        p.done.fetch_add(n, Ordering::Relaxed);
    }
}

/// Whether the load this thread works for was given up.
pub fn cancelled() -> bool {
    current().is_some_and(|p| p.is_cancelled())
}

/// The error a reader returns once its load was given up.
pub fn cancelled_error() -> crate::error::PreviewError {
    crate::error::PreviewError::new("cancelled", "Loading was cancelled.")
}
