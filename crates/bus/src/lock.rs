//! This crate's policy for poisoned mutexes, stated once.
//!
//! Every `Mutex` in this crate guards a small piece of delivery state: a
//! subscriber ring, a keep-last slot, a metric row, the drain task handle.
//! A poisoned lock means some *other* thread panicked while holding it. None of
//! those critical sections leaves the guarded state torn - each is a push, a
//! pop, a swap, or a counter update over a plain data structure - so the state
//! behind a poisoned lock is exactly the state the panicking thread had reached.
//!
//! Refusing to touch it would mean a panic anywhere in the process stops sample
//! delivery, lifecycle, and metrics for everything, which is strictly worse than
//! carrying on: the panic is already reported through its own thread, and the
//! runner turns a failed participant into ordinary failure. So this crate takes
//! the inner guard and continues, everywhere, rather than propagating poison
//! into receive and teardown paths that have no better answer than continuing.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Lock `mutex`, taking the guard even if the lock is poisoned.
///
/// A free function because `Mutex` is foreign to this crate, so the policy
/// cannot be carried as an inherent method. See the module docs for why
/// ignoring poison is the correct default here.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
