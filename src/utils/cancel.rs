//! Cooperative cancellation for long-running tree walks.
//!
//! `CancelRegistry` holds a monotonically-increasing generation counter.
//! Long-running operations call [`CancelRegistry::guard`] at the start of work;
//! the returned [`CancelGuard`] captures the generation at that moment. When
//! [`CancelRegistry::cancel_all`] runs it bumps the generation, so every
//! outstanding guard will observe [`CancelGuard::is_cancelled`] returning true
//! at its next checkpoint. Operations started after the bump get a fresh
//! guard and are unaffected.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Default)]
pub struct CancelRegistry {
    generation: Arc<AtomicU64>,
}

impl CancelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot the current generation; the returned guard signals cancellation
    /// when the registry's generation moves past this snapshot.
    pub fn guard(&self) -> CancelGuard {
        CancelGuard {
            snapshot: self.generation.load(Ordering::SeqCst),
            registry: self.clone(),
        }
    }

    /// Cancel every outstanding guard. Subsequent guards are unaffected.
    /// Returns the new generation value.
    pub fn cancel_all(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Current generation — exposed for diagnostic tooling.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone)]
pub struct CancelGuard {
    snapshot: u64,
    registry: CancelRegistry,
}

impl CancelGuard {
    /// True if [`CancelRegistry::cancel_all`] has been called since this guard
    /// was obtained.
    pub fn is_cancelled(&self) -> bool {
        self.registry.generation.load(Ordering::SeqCst) != self.snapshot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_not_cancelled_initially() {
        let registry = CancelRegistry::new();
        let guard = registry.guard();
        assert!(!guard.is_cancelled());
    }

    #[test]
    fn cancel_all_flips_existing_guards() {
        let registry = CancelRegistry::new();
        let a = registry.guard();
        let b = registry.guard();
        registry.cancel_all();
        assert!(a.is_cancelled());
        assert!(b.is_cancelled());
    }

    #[test]
    fn guards_taken_after_cancel_are_fresh() {
        let registry = CancelRegistry::new();
        let old = registry.guard();
        registry.cancel_all();
        assert!(old.is_cancelled());
        let new = registry.guard();
        assert!(!new.is_cancelled(), "new guards must start fresh");
    }

    #[test]
    fn multiple_cancels_compound_generations() {
        let registry = CancelRegistry::new();
        let g0 = registry.generation();
        registry.cancel_all();
        registry.cancel_all();
        registry.cancel_all();
        assert_eq!(registry.generation(), g0 + 3);
    }

    #[test]
    fn registry_is_clone_safe() {
        let registry = CancelRegistry::new();
        let registry_copy = registry.clone();
        let guard = registry.guard();
        registry_copy.cancel_all();
        assert!(guard.is_cancelled(), "clones must share state");
    }
}
