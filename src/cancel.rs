//! Cooperative cancellation token.
//!
//! `execute` checks this between top-level steps (creating a directory,
//! starting a move, removing an emptied source folder) and stops *starting*
//! new work once it is set. A rename that has already begun is a single
//! syscall and is always allowed to finish; nothing already renamed is ever
//! rolled back.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A cheaply cloneable flag shared between whoever requests cancellation
/// (e.g. a Ctrl+C handler) and the code performing the run.
#[derive(Debug, Clone)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        CancelToken(Arc::new(AtomicBool::new(false)))
    }

    /// Request cancellation. Safe to call from a signal handler.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_uncancelled_and_can_be_cancelled() {
        let token = CancelToken::new();
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn clones_share_state() {
        let token = CancelToken::new();
        let clone = token.clone();
        clone.cancel();
        assert!(token.is_cancelled());
    }
}
