//! The participant engine: the authoring traits, the role-gated capability
//! surface, the setup/step/reset contexts, the clock and step scheduler, the
//! launch contract, and the runner that drives them.
//!
//! Authoring uses a role attribute on a unit marker, an optional
//! `#[derive(phoxal::Config)]`, and an ordinary `Participant` implementation.
//!
//! [`metadata`] is the one public path here: the compatibility record every
//! participant artifact embeds, and its strict reader, which build-time tooling
//! reads back out of a linked binary. Nothing else in this module is a public
//! path. Whatever the framework publishes to participant authors goes out
//! through the crate-root facade in `lib.rs`, and whatever macro-generated code
//! needs goes out through `crate::__private`; modules inside the engine import
//! from the module that owns the item.

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// The participant-artifact metadata contract: the embedded document, its
/// writer, and its strict reader.
pub mod metadata;

pub(crate) mod api;
pub(crate) mod bus_log;
pub(crate) mod clock;
pub(crate) mod config;
pub(crate) mod context;
pub(crate) mod launch;
pub(crate) mod managed;
pub(crate) mod query;
pub(crate) mod runner;
pub(crate) mod runtime_performance;
pub(crate) mod scheduler;
pub(crate) mod spec;
// `surface` is the one engine module a participant crate reaches by name: the
// role attributes emit `impl $crate::__private::surface::…` into the
// participant's own crate, so the module itself is re-exported there. It has to
// stay `pub` for that re-export to be legal; `#[doc(hidden)]` keeps it out of
// the documented surface, the same as the `__private` path it is reached by.
#[doc(hidden)]
pub mod surface;

/// A duration as whole nanoseconds, saturating at [`u64::MAX`].
///
/// Every nanosecond count the engine carries is a `u64` - `RobotInstant` ticks
/// and the `*_ns` fields of a runtime-performance rollup alike - while
/// [`Duration::as_nanos`] is a `u128`. Narrowing it in one place is what keeps
/// the saturation identical on the cadence path and the telemetry path instead
/// of letting a second copy wrap.
pub(crate) fn duration_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// Lock `mutex`, taking the guard even when another thread panicked holding it.
///
/// Poisoning reports only that some thread unwound somewhere; it says nothing
/// about the guarded value. Everything the engine locks is a plain counter,
/// instant or channel handle updated by code that cannot unwind mid-update, so
/// the state behind a poisoned lock is exactly as consistent as behind a healthy
/// one. These locks all sit on the logging, clock and step paths, where refusing
/// to proceed would turn one unrelated thread's panic into a participant that
/// silently stops logging, stops stepping, or stops being able to date its own
/// samples - strictly worse than carrying on with the state as it stands. So
/// every lock in the engine recovers the guard rather than propagating the
/// poison.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
