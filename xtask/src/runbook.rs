//! Where a refused release sends the agent that has to answer for it.
//!
//! A gate that only says "no" makes the reader guess what it is allowed to do
//! about it, and the wrong guess is expensive: blessing a real break with a
//! bigger version, or fixing forward over a frozen fact that must never move.
//! So every refusal ends with one line naming the decision tree and the rule
//! inside it that applies. The rules themselves live in `xtask/README.md`,
//! because they are read by a human as often as by an agent; the numbering is
//! restated here so a diagnostic and the document cannot drift apart.

use std::fmt;

/// The path a refusal points at, spelled from the repository root so it reads
/// the same in a CI log as in a checkout.
const RUNBOOK: &str = "xtask/README.md";

/// The section inside it.
const SECTION: &str = "When a gate fails";

/// One rule of the decision tree.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum RunbookRule {
    /// Rules 1 and 2: a breaking contract change, intended or not. Which of
    /// the two applies is the one thing the checker cannot know, so the pointer
    /// names both and the reader decides.
    BreakingContractChange,
    /// Rule 3: a frozen bootstrap fact moved. There is no autonomous remedy.
    FrozenBootstrapDrift,
    /// Rule 4: the toolchain floor was raised.
    ToolchainFloorRaised,
    /// Rule 7: the candidate version does not clear an unchanged or additive
    /// change.
    UnderSizedCandidate,
    /// Rule 8: a document the published reader accepted is now rejected, or
    /// compiles to something else.
    AuthoredSourceBreak,
}

impl RunbookRule {
    /// How this rule is numbered in the document.
    fn numbering(self) -> &'static str {
        match self {
            Self::BreakingContractChange => "rules 1 and 2",
            Self::FrozenBootstrapDrift => "rule 3",
            Self::ToolchainFloorRaised => "rule 4",
            Self::UnderSizedCandidate => "rule 7",
            Self::AuthoredSourceBreak => "rule 8",
        }
    }

    /// What the reader is about to be told, in one clause, so the pointer is
    /// useful even to someone who does not open the document.
    fn headline(self) -> &'static str {
        match self {
            Self::BreakingContractChange => {
                "a breaking contract change - carry the marker if it is intended, revert it if it \
                 is not"
            }
            Self::FrozenBootstrapDrift => {
                "a frozen bootstrap fact moved - stop and escalate, there is no autonomous remedy"
            }
            Self::ToolchainFloorRaised => {
                "the toolchain floor rose - revert the raise or acknowledge it and release a minor"
            }
            Self::UnderSizedCandidate => "raise the candidate version to the stated minimum",
            Self::AuthoredSourceBreak => {
                "an authored document stopped being read the way it was - restore it in the \
                 normalizer, or take the break to the next line with the breaking marker"
            }
        }
    }
}

/// The one line a refusal ends with.
#[derive(Clone, Debug)]
pub(crate) struct RunbookPointer {
    rules: Vec<RunbookRule>,
}

impl RunbookPointer {
    /// Point at the rules that answer this refusal, in the order the document
    /// numbers them.
    pub(crate) fn to(rules: impl IntoIterator<Item = RunbookRule>) -> Self {
        let mut rules = rules.into_iter().collect::<Vec<_>>();
        rules.sort();
        rules.dedup();
        Self { rules }
    }
}

impl fmt::Display for RunbookPointer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "remedy: {RUNBOOK} \"{SECTION}\"")?;
        for (index, rule) in self.rules.iter().enumerate() {
            let separator = if index == 0 { ',' } else { ';' };
            write!(
                formatter,
                "{separator} {} ({})",
                rule.numbering(),
                rule.headline()
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    /// The pointer is one line: a CI log reader lands on the tree by reading
    /// the last line of the failure, not by scrolling.
    #[test]
    fn a_pointer_is_one_line_naming_the_document_and_the_rule() {
        let pointer = RunbookPointer::to([RunbookRule::FrozenBootstrapDrift]).to_string();
        assert!(!pointer.contains('\n'), "{pointer}");
        assert!(pointer.contains("xtask/README.md"), "{pointer}");
        assert!(pointer.contains("When a gate fails"), "{pointer}");
        assert!(pointer.contains("rule 3"), "{pointer}");
        assert!(pointer.contains("no autonomous remedy"), "{pointer}");
    }

    /// Two reasons can bind one refusal, and the reader is owed both rules.
    #[test]
    fn several_rules_stay_on_one_line_in_a_stable_order() {
        let pointer = RunbookPointer::to([
            RunbookRule::ToolchainFloorRaised,
            RunbookRule::BreakingContractChange,
            RunbookRule::ToolchainFloorRaised,
        ])
        .to_string();
        assert!(!pointer.contains('\n'), "{pointer}");
        let breaking = pointer
            .find("rules 1 and 2")
            .expect("the contract rule is named");
        let floor = pointer.find("rule 4").expect("the floor rule is named");
        assert!(breaking < floor, "{pointer}");
        assert_eq!(pointer.matches("rule 4").count(), 1, "{pointer}");
    }

    /// The numbering a diagnostic prints is the numbering the document uses.
    /// The two are written in different files, so this is the only thing
    /// holding them together.
    #[test]
    fn every_rule_number_exists_in_the_runbook() {
        let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
            .expect("the runner's README reads");
        assert!(
            readme.contains(&format!("## {SECTION}")),
            "{SECTION} is missing"
        );
        for rule in [
            RunbookRule::BreakingContractChange,
            RunbookRule::FrozenBootstrapDrift,
            RunbookRule::ToolchainFloorRaised,
            RunbookRule::UnderSizedCandidate,
            RunbookRule::AuthoredSourceBreak,
        ] {
            for number in rule.numbering().trim_start_matches("rules ").split(" and ") {
                let heading = format!("### {}.", number.trim_start_matches("rule "));
                assert!(readme.contains(&heading), "the runbook has no {heading}");
            }
        }
    }
}
