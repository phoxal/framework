use anyhow::Result;
use phoxal::api::v1::frame::FrameId;
use phoxal::api::v1::localize::{
    AffectedKeyframeSummary, Keyframe, KeyframeId, LocalizationMode, LocalizationRevisionCause,
    LocalizationSource, LocalizationStatus, LocalizationStatusReason, PoseEstimate,
};
use phoxal::api::v1::odometry::OdometryEstimate;
use phoxal::api::v1::simulation::pose::Pose as SimPose;
use phoxal::bus::typed::Received;
use phoxal::runtime::clock::Step;

use crate::runtime::{
    BackendUpdate, LocalizeBackend, NewRevision, current_revision,
    initial_sensor_integration_revision,
};

const MAP_FRAME_ID: &str = "map";
const BASE_FRAME_ID: &str = "base_footprint";
const LOOP_CLOSURE_DELAY_NS: u64 = 2_000_000_000;

pub(crate) struct SimulatorTruthBackend {
    latest_pose: Option<Received<SimPose>>,
    initial_revision_emitted: bool,
    loop_closure_emitted: bool,
    first_tracking_ns: Option<u64>,
}

impl SimulatorTruthBackend {
    pub(crate) const fn new() -> Self {
        Self {
            latest_pose: None,
            initial_revision_emitted: false,
            loop_closure_emitted: false,
            first_tracking_ns: None,
        }
    }
}

#[async_trait::async_trait]
impl LocalizeBackend for SimulatorTruthBackend {
    fn name(&self) -> LocalizationSource {
        LocalizationSource::SimulatorTruth
    }

    fn ingest_odometry(&mut self, _sample: Received<OdometryEstimate>) {}

    fn ingest_sim_pose(&mut self, sample: Received<SimPose>) {
        self.latest_pose = Some(sample);
    }

    fn step(&mut self, step: Step) -> Result<BackendUpdate> {
        let Some(pose) = &self.latest_pose else {
            return Ok(BackendUpdate {
                mode: LocalizationMode::Initializing,
                pose: None,
                velocity: None,
                covariance: None,
                imu_bias: None,
                status: LocalizationStatus {
                    healthy: false,
                    reasons: vec![
                        LocalizationStatusReason::SensorMissing,
                        LocalizationStatusReason::BackendInitializing,
                    ],
                },
                valid_at_ns: None,
                new_revision: None,
                keyframe: None,
            });
        };

        let pose_estimate = PoseEstimate {
            frame_id: FrameId::new(MAP_FRAME_ID),
            child_frame_id: FrameId::new(BASE_FRAME_ID),
            translation_m: pose.value.translation_m,
            rotation_xyzw: pose.value.rotation_xyzw,
        };
        let tracking_ns = step.tick.time_ns();
        let mut new_revision = initial_sensor_integration_revision(
            LocalizationMode::Tracking,
            self.initial_revision_emitted,
        );
        let keyframe = new_revision.as_ref().map(|_| Keyframe {
            keyframe_id: KeyframeId::new("simulator-truth-0"),
            revision: current_revision(),
            pose: pose_estimate.clone(),
            descriptors: Vec::new(),
        });
        if new_revision.is_some() {
            self.initial_revision_emitted = true;
            self.first_tracking_ns = Some(tracking_ns);
        } else if !self.loop_closure_emitted
            && self.first_tracking_ns.is_some_and(|first_tracking_ns| {
                tracking_ns.saturating_sub(first_tracking_ns) >= LOOP_CLOSURE_DELAY_NS
            })
        {
            new_revision = Some(NewRevision {
                cause: LocalizationRevisionCause::LoopClosure,
                affected_keyframes: AffectedKeyframeSummary {
                    keyframe_ids: Vec::new(),
                    region: None,
                },
            });
            self.loop_closure_emitted = true;
        }

        Ok(BackendUpdate {
            mode: LocalizationMode::Tracking,
            pose: Some(pose_estimate),
            keyframe,
            velocity: None,
            covariance: None,
            imu_bias: None,
            status: LocalizationStatus {
                healthy: true,
                reasons: Vec::new(),
            },
            valid_at_ns: Some(pose.at_ns.unwrap_or_else(|| step.tick.time_ns())),
            new_revision,
        })
    }
}

#[cfg(test)]
mod tests {
    use phoxal::api::v1::localize::{AffectedKeyframeSummary, LocalizationRevisionCause};
    use phoxal::api::v1::simulation::clock::Clock;

    use super::*;

    #[test]
    fn initializing_until_first_sim_pose() {
        let mut backend = SimulatorTruthBackend::new();

        let update = step_backend(&mut backend, step_at(20_000_000));

        assert_eq!(update.mode, LocalizationMode::Initializing);
        assert_eq!(update.pose, None);
        assert_eq!(update.new_revision, None);
    }

    #[test]
    fn tracking_after_sim_pose_with_initial_revision() {
        let mut backend = SimulatorTruthBackend::new();

        backend.ingest_sim_pose(sim_pose_sample(10, [1.0, 2.0, 0.0]));
        let update = step_backend(&mut backend, step_at(20_000_000));

        assert_eq!(update.mode, LocalizationMode::Tracking);
        assert_eq!(
            update.pose.as_ref().map(|pose| pose.translation_m),
            Some([1.0, 2.0, 0.0])
        );
        assert_eq!(
            update
                .pose
                .as_ref()
                .map(|pose| (&pose.frame_id, &pose.child_frame_id)),
            Some((&FrameId::new(MAP_FRAME_ID), &FrameId::new(BASE_FRAME_ID)))
        );
        assert!(update.status.healthy);
        assert_eq!(update.status.reasons, Vec::new());
        assert_eq!(
            update.new_revision,
            Some(crate::runtime::NewRevision {
                cause: LocalizationRevisionCause::SensorIntegration,
                affected_keyframes: AffectedKeyframeSummary {
                    keyframe_ids: Vec::new(),
                    region: None,
                },
            })
        );

        let Some(keyframe) = update.keyframe else {
            panic!("first simulator-truth Tracking step must emit a keyframe");
        };
        assert_eq!(keyframe.keyframe_id, KeyframeId::new("simulator-truth-0"));
        assert_eq!(keyframe.revision, current_revision());
        assert_eq!(keyframe.pose.translation_m, [1.0, 2.0, 0.0]);
        assert_eq!(
            (&keyframe.pose.frame_id, &keyframe.pose.child_frame_id),
            (&FrameId::new(MAP_FRAME_ID), &FrameId::new(BASE_FRAME_ID))
        );
    }

    #[test]
    fn emits_one_loop_closure_revision_after_tracking_delay() {
        let mut backend = SimulatorTruthBackend::new();

        backend.ingest_sim_pose(sim_pose_sample(10, [1.0, 2.0, 0.0]));
        let first = step_backend(&mut backend, step_at(20_000_000));
        let before_delay = step_backend(&mut backend, step_at(1_000_000_000));
        let loop_closure = step_backend(&mut backend, step_at(2_020_000_000));
        let after_loop_closure = step_backend(&mut backend, step_at(4_020_000_000));

        assert_eq!(
            first.new_revision,
            Some(crate::runtime::NewRevision {
                cause: LocalizationRevisionCause::SensorIntegration,
                affected_keyframes: AffectedKeyframeSummary {
                    keyframe_ids: Vec::new(),
                    region: None,
                },
            })
        );
        assert!(first.keyframe.is_some());
        assert_eq!(before_delay.new_revision, None);
        assert_eq!(before_delay.keyframe, None);
        assert_eq!(
            loop_closure.new_revision,
            Some(crate::runtime::NewRevision {
                cause: LocalizationRevisionCause::LoopClosure,
                affected_keyframes: AffectedKeyframeSummary {
                    keyframe_ids: Vec::new(),
                    region: None,
                },
            })
        );
        assert_eq!(loop_closure.keyframe, None);
        assert_eq!(after_loop_closure.new_revision, None);
        assert_eq!(after_loop_closure.keyframe, None);
    }

    fn step_at(time_ns: u64) -> Step {
        Step::new(Clock::new(1, time_ns / 20_000_000, time_ns, 20_000_000))
    }

    fn step_backend(backend: &mut SimulatorTruthBackend, step: Step) -> BackendUpdate {
        match backend.step(step) {
            Ok(update) => update,
            Err(error) => panic!("simulator-truth step failed: {error:#}"),
        }
    }

    fn sim_pose_sample(timestamp_ns: u64, translation_m: [f64; 3]) -> Received<SimPose> {
        Received {
            at_ns: Some(timestamp_ns),
            value: SimPose {
                frame_id: "world".into(),
                translation_m,
                rotation_xyzw: [0.0, 0.0, 0.0, 1.0],
            },
        }
    }
}
