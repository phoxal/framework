//! The compatibility machinery, and the one aggregate this crate states its
//! whole process/wire boundary in.
//!
//! [`surface`] is the record model each contract-owning module states its own
//! boundary in; [`wire`] is the deterministic model of the shapes those
//! contracts put on the wire, which compatibility CI checks against published
//! baselines.
//!
//! [`contract_surface`] is the crate aggregate. Each owner still states its own
//! records beside its own definitions - `bus::__compat`, `bundle::__compat`,
//! `participant::metadata::__compat`, and the three api families, whose records
//! the `nodes!`/`endpoints!` declarations emit from the same structure that
//! renders their concrete keys - and this module only collects them and renders
//! the one canonical document.
//!
//! One record is this module's own: a launched participant's whole process
//! boundary on the way in is its argv. It is read out of the clap definition
//! itself rather than restated beside it, so a renamed flag, a newly optional
//! argument, or an option that started accepting repetition changes the surface
//! by construction - there is no second list to forget to update.

pub mod surface;
pub mod wire;

use clap::CommandFactory;

use crate::__compat::surface::{ContractRecord, ContractSurface, LaunchArgument, LaunchValueShape};
use crate::participant::launch::Launch;

/// The canonical rendering of this crate's whole contract surface.
///
/// Every process/wire fact the framework owns, in one deterministic document:
/// the three api families' endpoints, the bus envelopes and key constants, the
/// bundle manifest document, the participant metadata document, and the launch
/// argv contract.
#[must_use]
pub fn contract_surface() -> String {
    ContractSurface::new(contract_records()).canonical_json()
}

/// Every record this crate declares, in no particular order:
/// [`ContractSurface::new`] is what puts them in the canonical one.
fn contract_records() -> Vec<ContractRecord> {
    let mut records = vec![ContractRecord::launch(launch_arguments())];
    crate::api::contract_records(&mut records);
    crate::runtime::api::contract_records(&mut records);
    crate::supervisor::api::contract_records(&mut records);
    crate::bus::__compat::contract_records(&mut records);
    crate::bundle::__compat::contract_records(&mut records);
    crate::participant::metadata::__compat::contract_records(&mut records);
    records
}

/// Read the declared arguments off the parser, in its own declaration order.
fn launch_arguments() -> Vec<LaunchArgument> {
    Launch::command()
        .get_arguments()
        .map(|argument| {
            LaunchArgument::new(
                // Every launch argument is long-only, which the parser's own
                // tests pin; an argument without a long spelling would show up
                // here as its clap id rather than being silently dropped.
                argument
                    .get_long()
                    .map_or_else(|| argument.get_id().to_string(), ToString::to_string),
                argument.is_required_set(),
                matches!(argument.get_action(), clap::ArgAction::Append),
                if argument.get_action().takes_values() {
                    LaunchValueShape::Text
                } else {
                    LaunchValueShape::Flag
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{contract_surface, launch_arguments};
    use crate::__compat::surface::{
        ContractRecord, ContractSurface, LaunchArgument, LaunchValueShape,
    };

    /// The launch record is derived from the live clap definition, so this
    /// states the record that definition is expected to produce, and that the
    /// record reaches the crate aggregate unchanged. A change to the process
    /// boundary - a flag renamed, an argument made optional, an option made
    /// repeatable - shows up here as a diff a reviewer has to accept on
    /// purpose.
    #[test]
    fn the_launch_record_is_the_supervisor_owned_argv_contract() {
        let expected = ContractRecord::launch([
            LaunchArgument::new("participant-id", true, false, LaunchValueShape::Text),
            LaunchArgument::new("bundle-root", true, false, LaunchValueShape::Text),
            LaunchArgument::new("connect", true, true, LaunchValueShape::Text),
            LaunchArgument::new("simulation", false, false, LaunchValueShape::Flag),
        ]);
        assert_eq!(ContractRecord::launch(launch_arguments()), expected);

        let rendered = ContractSurface::new([expected]).canonical_json();
        let record = rendered
            .strip_prefix(r#"{"records":["#)
            .and_then(|rest| rest.strip_suffix("]}"))
            .expect("one record renders inside its own surface");
        assert!(contract_surface().contains(record), "{record}");
    }

    /// Two calls produce the same bytes, which is what lets a checker compare
    /// the surface with a stored baseline by string equality. Every owner is in
    /// it, so a lost sub-provider cannot pass as an unchanged surface.
    #[test]
    fn the_surface_is_deterministic_json() {
        let rendered = contract_surface();
        serde_json::from_str::<serde_json::Value>(&rendered).expect("the surface is JSON");
        assert_eq!(contract_surface(), rendered);
        for expected in [
            r#""record":"launch""#,
            r#""record":"endpoint""#,
            r#""record":"envelope""#,
            r#""record":"document""#,
            r#""record":"identifier""#,
        ] {
            assert!(rendered.contains(expected), "{expected} missing");
        }
    }

    /// `--simulation` is the launch contract's only bare switch: it is a
    /// launcher decision with no value to carry. Everything else names a fact
    /// and therefore consumes an argv token.
    #[test]
    fn simulation_is_the_only_bare_switch() {
        let flags = launch_arguments()
            .into_iter()
            .filter(|argument| argument.value == LaunchValueShape::Flag)
            .map(|argument| argument.name)
            .collect::<Vec<_>>();
        assert_eq!(flags, ["simulation"]);
    }
}
