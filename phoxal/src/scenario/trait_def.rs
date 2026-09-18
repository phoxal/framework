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
//! [`Scenario`] itself is not `dyn` compatible because the
//! `Default` supertrait is `Sized`. The case host retains the
//! same scenario instance through planning, execution, and
//! verification through the dyn-compatible [`ScenarioBox`]
//! companion trait; the [`#[phoxal::scenario]`](../macro@phoxal)
//! attribute generates a `Box<dyn ScenarioBox>` per registered
//! type so `plan()` and `verify()` observe the same instance's
//! state.

use crate::scenario::plan::ScenarioPlan;
use crate::scenario::results::ScenarioRun;

/// A scenario author implements this trait on a `pub struct` with a
/// hand-written or derived [`Default`]. The `#[phoxal::scenario]`
/// attribute registers the impl into the static
/// [`ScenarioRegistry`](crate::scenario::ScenarioRegistry).
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

/// Dyn-compatible companion trait for [`Scenario`]. The case host
/// retains a `Box<dyn ScenarioBox>` so `plan_box()` and
/// `verify_box()` run on the same instance the user authored;
/// state set by the user on the scenario struct survives across
/// the planning -> execution -> verification boundary. The
/// blanket impl forwards to the user's `plan` and `verify`
/// methods without re-constructing a fresh `Default` instance.
///
/// [`#[phoxal::scenario]`](../macro@phoxal) wraps each
/// registered type in a `Box<dyn ScenarioBox>` at registration
/// time; the case host moves that box through the lifecycle.
pub trait ScenarioBox: 'static {
    /// Identical contract to [`Scenario::plan`]. Required because
    /// `Scenario` is not `dyn`-compatible.
    fn plan_box(&self) -> crate::Result<ScenarioPlan>;
    /// Identical contract to [`Scenario::verify`]. Required
    /// because `Scenario` is not `dyn`-compatible.
    fn verify_box(&self, run: &ScenarioRun) -> crate::Result<()>;
}

impl<T: Scenario> ScenarioBox for T {
    fn plan_box(&self) -> crate::Result<ScenarioPlan> {
        self.plan()
    }
    fn verify_box(&self, run: &ScenarioRun) -> crate::Result<()> {
        self.verify(run)
    }
}
