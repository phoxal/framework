//! The plan #00 Layer 2 owner capability.

/// The plan #00 Layer 2 owner capability: a runner-minted token required to
/// construct the OWNER side of a topic on the documented authoring surface.
///
/// # What it is for
///
/// Layer 1 (side branding) makes taking the *wrong side* of a topic a compile
/// error, but on its own it left the owner side reachable through the owner
/// builder with no proof of authority. Layer 2 closes the "any participant can
/// mint itself the owner side" hole: the owner builder entry
/// `api::topic::internal::new(cap)` now *requires* an `OwnerCap`, and the ONLY
/// documented way to obtain one is
/// [`phoxal::SetupContext::owner_capability()`]. The runner mints the single
/// `OwnerCap` it hands to `SetupContext` before `#[setup]` runs, so a runtime opts
/// into owning its own topics deliberately and visibly:
///
/// ```ignore
/// let cap = ctx.owner_capability();
/// let pub_ = ctx.publisher(api::topic::internal::new(cap).drive().state()).await?;
/// ```
///
/// # Honest residual (not a hard guarantee)
///
/// This is a "not-by-accident / not-on-the-documented-surface" gate, not a hard
/// guarantee (plan #00 lines 129-131). The raw `#[doc(hidden)]` [`Topic`]
/// constructors (`Topic::new_static` / `Topic::new_owned`) are still `pub`, so a
/// determined caller can forge an owner-branded topic by hand without ever
/// obtaining an `OwnerCap`. Those constructors must stay `pub` because the
/// proc-macro-generated `phoxal_api_tree!` code in the separate `phoxal-api` crate
/// has to call them across the crate boundary, and a `pub(crate)` constructor
/// cannot cross it. Layer 2 therefore raises the bar on the documented surface (no
/// owner topic is built by accident) without claiming to seal the escape hatch.
///
/// [`Topic`]: crate::Topic
/// [`phoxal::SetupContext::owner_capability()`]: https://docs.rs/phoxal
#[derive(Clone, Copy, Debug)]
pub struct OwnerCap(());

impl OwnerCap {
    /// Mint an `OwnerCap`. Reserved for the phoxal runner.
    ///
    /// `#[doc(hidden)]` and `__`-prefixed: this is the privileged construction
    /// point the runner uses when it builds `SetupContext`. It is not part of the
    /// documented authoring surface - runtimes obtain the cap through
    /// `phoxal::SetupContext::owner_capability()` instead.
    #[doc(hidden)]
    pub const fn __mint() -> Self {
        OwnerCap(())
    }
}
