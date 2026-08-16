//! The contract surface this crate owns: the participant launch contract.
//!
//! A launched participant's whole process boundary on the way in is its argv.
//! The record below is read out of the clap definition itself rather than
//! restated beside it, so a renamed flag, a newly optional argument, or an
//! option that started accepting repetition changes the surface by
//! construction - there is no second list to forget to update.

use clap::CommandFactory;
use phoxal_runtime_contract::contract_surface::{
    ContractRecord, ContractSurface, LaunchArgument, LaunchValueShape,
};

use crate::participant::launch::SupervisedLaunch;

/// The canonical rendering of this crate's contract surface.
#[must_use]
pub fn contract_surface() -> String {
    ContractSurface::new([ContractRecord::launch(launch_arguments())]).canonical_json()
}

/// Read the declared arguments off the parser, in its own declaration order.
fn launch_arguments() -> Vec<LaunchArgument> {
    SupervisedLaunch::command()
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
    use phoxal_runtime_contract::contract_surface::{
        ContractRecord, ContractSurface, LaunchArgument, LaunchValueShape,
    };

    /// The launch record is derived from the live clap definition, so this
    /// states the record that definition is expected to produce. A change to
    /// the process boundary - a flag renamed, an argument made optional, an
    /// option made repeatable - shows up here as a diff a reviewer has to
    /// accept on purpose.
    #[test]
    fn the_launch_record_is_the_supervisor_owned_argv_contract() {
        let expected = ContractSurface::new([ContractRecord::launch([
            LaunchArgument::new("participant-id", true, false, LaunchValueShape::Text),
            LaunchArgument::new("bundle-root", true, false, LaunchValueShape::Text),
            LaunchArgument::new("connect", true, true, LaunchValueShape::Text),
            LaunchArgument::new("simulation", false, false, LaunchValueShape::Flag),
        ])]);
        assert_eq!(contract_surface(), expected.canonical_json());
    }

    /// Two calls produce the same bytes, which is what lets a checker compare
    /// the surface with a stored baseline by string equality.
    #[test]
    fn the_surface_is_deterministic_json() {
        let rendered = contract_surface();
        serde_json::from_str::<serde_json::Value>(&rendered).expect("the surface is JSON");
        assert_eq!(contract_surface(), rendered);
        assert!(rendered.contains(r#""record":"launch""#), "{rendered}");
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
