//! Public [`Scenario`] trait (P1 shape, P2/P3 fill in body semantics).
//!
//! Per the plan:
//!
//! ```text
//! pub trait Scenario: Default + 'static {
//!     fn plan(&self) -> phoxal::Result<ScenarioPlan>;
//!     fn verify(&self, run: &ScenarioRun) -> phoxal::Result<()>;
//! }
//! ```
//!
//! The trait itself is not `dyn` compatible (it carries a `Default`
//! supertrait, which is `Sized`). That is by design: the harness
//! dispatches through monomorphized entry functions, not `dyn Scenario`.

use crate::scenario::plan::ScenarioPlan;
use crate::scenario::results::ScenarioRun;

/// A scenario author implements this trait on a `pub struct` with a
/// hand-written or derived [`Default`]. The `#[phoxal::scenario]`
/// attribute registers the impl into the static [`ScenarioRegistry`].
///
/// `plan()` declares one finite simulated experiment. P2 replaces the
/// placeholder `ScenarioPlan` with the typed action schedule and capture
/// declarations.
///
/// `verify()` returns `Ok(())` on success or `Err(...)` explaining the
/// failure. P3 supplies the immutable `ScenarioRun` evidence.
pub trait Scenario: Default + 'static {
    fn plan(&self) -> crate::Result<ScenarioPlan>;
    fn verify(&self, run: &ScenarioRun) -> crate::Result<()>;
}
