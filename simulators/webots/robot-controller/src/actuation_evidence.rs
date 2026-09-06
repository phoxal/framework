use super::*;

pub(super) struct PendingActuationEvidence {
    pub(super) capability: phoxal::model::identity::CapabilityRef,
    pub(super) revision: u64,
    pub(super) selected_at: phoxal::bus::RobotInstant,
    pub(super) selected_from: WorldProgress,
    pub(super) offered: Vec<OfferedActuation>,
    pub(super) selected: Option<api::component::motor::Command>,
    pub(super) selection: ActuationSelection,
    pub(super) applied: AppliedActuation,
}

impl PendingActuationEvidence {
    pub(super) fn complete(self, transition: &LiveTransitionStamp) -> ActuationEvidence {
        ActuationEvidence {
            capability: self.capability,
            revision: self.revision,
            selected_at: self.selected_at,
            selected_from: self.selected_from,
            progress: transition.progress(),
            instant: transition.instant(),
            offered: self.offered,
            selected: self.selected,
            selection: self.selection,
            applied: self.applied,
        }
    }
}

pub(super) fn evidence_decision(decision: LeaseDecision) -> ActuationDecision {
    match decision {
        LeaseDecision::Acquired => ActuationDecision::Acquired,
        LeaseDecision::Renewed => ActuationDecision::Renewed,
        LeaseDecision::Rejected(rejection) => match rejection {
            LeaseRejection::WrongParticipant => ActuationDecision::WrongParticipant,
            LeaseRejection::ParticipantSource => ActuationDecision::ParticipantSource,
            LeaseRejection::SourceAbsent => ActuationDecision::SourceAbsent,
            LeaseRejection::SourceConflict => ActuationDecision::SourceConflict,
            LeaseRejection::StaleSequence { accepted, observed } => {
                ActuationDecision::StaleSequence { accepted, observed }
            }
            LeaseRejection::AuthorityHeld { owner } => ActuationDecision::AuthorityHeld { owner },
            LeaseRejection::NotOwner { owner, requested } => {
                ActuationDecision::NotOwner { owner, requested }
            }
            LeaseRejection::ReadyStateOverflow => ActuationDecision::ReadyStateOverflow,
        },
    }
}
