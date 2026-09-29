//! One executable, named runtime roles.
//!
//! A robot executable can host its authored brain runtime plus generated
//! hosted roles — the conversion runtime emitted by the build helper — in
//! one process image. The supervisor still admits and launches one process
//! per graph instance: it starts the same executable again with
//! `--instance-id phoxal-adapter`, and [`dispatch_hosted`] routes that
//! launch to the registered role closure while the `brain` instance runs
//! the authored runtime. The command-line surface is exactly
//! [`crate::runtime::RuntimeLaunch`], so launch routing needs no new
//! supervisor-side wiring.
//!
//! Role selection is explicit: a hosted role exists only when generated
//! glue registers it, duplicate or reserved names are rejected up front,
//! and an instance id that matches nothing fails loudly instead of
//! accidentally running the brain.

/// One named secondary runtime role hosted inside a brain executable.
#[derive(Clone, Copy)]
pub struct HostedRole {
    /// Admitted supervisor instance identity for this role (for example
    /// `phoxal-adapter`).
    pub registration: RoleRegistration,
    /// Entry point that runs the hosted runtime through the ordinary
    /// transport runner.
    pub run: fn() -> crate::Result<()>,
}

/// One registered runtime role inside a binary that hosts multiple roles.
#[derive(Clone, Copy)]
pub struct RoleRegistration {
    /// Admitted supervisor instance identity for this role.
    pub instance_id: &'static str,
    /// Human-readable description used in dispatch diagnostics.
    pub description: &'static str,
}

/// Run `brain`, or the hosted role matching the launch `--instance-id`.
///
/// Every launch that does not match a hosted role runs `brain` — the
/// brain is the executable's primary runtime and owns its own instance
/// naming. Hosted roles may not claim the reserved `brain` instance or
/// duplicate each other.
pub fn dispatch_hosted<BRAIN>(brain: BRAIN, hosted: &[HostedRole]) -> crate::Result<()>
where
    BRAIN: crate::runtime::RegisteredRuntime,
    BRAIN::Inputs:
        crate::runtime::input::TransportInputSet + crate::runtime::input::TransportInputSink,
{
    let mut seen = std::collections::BTreeSet::new();
    for role in hosted {
        if role.registration.instance_id == "brain" {
            return Err(anyhow::anyhow!(
                "a hosted role may not claim the reserved `brain` instance"
            ));
        }
        if !seen.insert(role.registration.instance_id) {
            return Err(anyhow::anyhow!(
                "hosted role `{}` is registered more than once",
                role.registration.instance_id
            ));
        }
    }
    let parsed = super::runner::RuntimeLaunch::parse()?;
    if let Some(role) = hosted
        .iter()
        .find(|role| role.registration.instance_id == parsed.instance_id)
    {
        tracing::info!(
            instance_id = %parsed.instance_id,
            description = role.registration.description,
            "dispatching hosted runtime role",
        );
        return (role.run)();
    }
    super::runner::run_transport(brain)
}
