use super::*;

pub(super) struct DiagnosticsState {
    pub(super) revision: u64,
    pub(super) pacing: VecDeque<PacingPoint>,
    pub(super) last_transition: Option<Instant>,
    pub(super) last_emission: Option<Instant>,
}

#[derive(Clone, Copy)]
pub(super) struct PacingPoint {
    pub(super) progress: WorldProgress,
    pub(super) host: Instant,
}

pub(super) fn record_pacing(
    diagnostics: &mut DiagnosticsState,
    progress: WorldProgress,
    running: bool,
    now: Instant,
) -> Result<bool, String> {
    if !running {
        diagnostics.pacing.clear();
    } else {
        if diagnostics.pacing.len() == PACING_WINDOW_TRANSITIONS {
            diagnostics.pacing.pop_front();
        }
        diagnostics.pacing.push_back(PacingPoint {
            progress,
            host: now,
        });
    }
    diagnostics.last_transition = Some(now);
    let emit = diagnostics
        .last_emission
        .is_none_or(|last| now.duration_since(last) >= DIAGNOSTICS_EMISSION_INTERVAL);
    if emit {
        diagnostics.last_emission = Some(now);
        diagnostics.revision = next_revision(diagnostics.revision)?;
    }
    Ok(emit)
}

pub(super) fn clear_pacing_state(
    diagnostics: &mut DiagnosticsState,
    now: Instant,
) -> Result<(), String> {
    diagnostics.pacing.clear();
    diagnostics.last_emission = Some(now);
    diagnostics.revision = next_revision(diagnostics.revision)?;
    Ok(())
}

pub(super) fn project_diagnostics(state: &DiagnosticsState) -> WorldSessionDiagnostics {
    let pacing = match (state.pacing.front(), state.pacing.back()) {
        (Some(first), Some(last)) if state.pacing.len() >= 2 => {
            let world_elapsed_ns = last
                .progress
                .elapsed_ns()
                .saturating_sub(first.progress.elapsed_ns());
            let host_elapsed_ns =
                u64::try_from(last.host.duration_since(first.host).as_nanos()).unwrap_or(u64::MAX);
            let completed_transitions = last
                .progress
                .completed_step()
                .saturating_sub(first.progress.completed_step());
            let observed = ObservedWorldPacing {
                world_elapsed_ns,
                host_elapsed_ns,
                completed_transitions,
            };
            observed.is_valid().then_some(observed)
        }
        _ => None,
    };
    WorldSessionDiagnostics {
        revision: state.revision,
        pacing,
        last_transition_age_ns: state
            .last_transition
            .map(|instant| u64::try_from(instant.elapsed().as_nanos()).unwrap_or(u64::MAX)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacing_samples_every_transition_but_publishes_at_most_once_per_second() {
        let start = Instant::now();
        let mut diagnostics = DiagnosticsState {
            revision: 0,
            pacing: VecDeque::new(),
            last_transition: None,
            last_emission: None,
        };
        assert!(
            record_pacing(
                &mut diagnostics,
                WorldProgress::at(1, 10).expect("progress"),
                true,
                start,
            )
            .expect("first pacing sample")
        );
        assert_eq!(diagnostics.revision, 1);
        assert!(
            !record_pacing(
                &mut diagnostics,
                WorldProgress::at(2, 10).expect("progress"),
                true,
                start + Duration::from_millis(500),
            )
            .expect("second pacing sample")
        );
        assert_eq!(diagnostics.pacing.len(), 2);
        assert_eq!(diagnostics.revision, 1);
        assert!(
            record_pacing(
                &mut diagnostics,
                WorldProgress::at(3, 10).expect("progress"),
                true,
                start + DIAGNOSTICS_EMISSION_INTERVAL,
            )
            .expect("third pacing sample")
        );
        assert_eq!(diagnostics.pacing.len(), 3);
        assert_eq!(diagnostics.revision, 2);
    }

    #[test]
    fn pause_clears_only_the_window_and_publishes_a_revision() {
        let transition = Instant::now();
        let mut diagnostics = DiagnosticsState {
            revision: 7,
            pacing: VecDeque::from([PacingPoint {
                progress: WorldProgress::at(4, 10).expect("progress"),
                host: transition,
            }]),
            last_transition: Some(transition),
            last_emission: Some(transition),
        };
        clear_pacing_state(&mut diagnostics, transition + Duration::from_millis(1))
            .expect("pause pacing clear");
        assert!(diagnostics.pacing.is_empty());
        assert_eq!(diagnostics.last_transition, Some(transition));
        assert_eq!(diagnostics.revision, 8);
        let projection = project_diagnostics(&diagnostics);
        assert_eq!(projection.revision, 8);
        assert!(projection.pacing.is_none());
        assert!(projection.last_transition_age_ns.is_some());
    }

    #[test]
    fn below_one_pacing_is_retained_as_observation_without_becoming_a_target() {
        let start = Instant::now();
        let mut diagnostics = DiagnosticsState {
            revision: 0,
            pacing: VecDeque::new(),
            last_transition: None,
            last_emission: None,
        };
        record_pacing(
            &mut diagnostics,
            WorldProgress::at(1, 10).expect("first progress"),
            true,
            start,
        )
        .expect("first pacing sample");
        record_pacing(
            &mut diagnostics,
            WorldProgress::at(2, 10).expect("second progress"),
            true,
            start + Duration::from_nanos(40),
        )
        .expect("second pacing sample");

        let pacing = project_diagnostics(&diagnostics)
            .pacing
            .expect("two transitions form one observation");
        assert_eq!(pacing.world_elapsed_ns, 10);
        assert_eq!(pacing.host_elapsed_ns, 40);
        assert_eq!(pacing.completed_transitions, 1);
        assert!(pacing.world_elapsed_ns < pacing.host_elapsed_ns);
    }
}
