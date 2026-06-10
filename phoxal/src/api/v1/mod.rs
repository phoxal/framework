//! Versioned typed bus wire contracts.

pub mod asset;
pub mod component;
pub mod drive;
pub mod explore;
pub mod follow;
pub mod frame;
pub mod joint;
pub mod localize;
pub mod map;
pub mod mission;
pub mod motion;
pub mod odometry;
pub mod perception;
pub mod plan;
pub mod power;
pub mod presence;
pub mod safety;
pub mod simulation;
pub mod video;

crate::bus::topic_tree! {
    pub mod topic;
    v1 {
        drive {
            pubsub target: crate::api::v1::drive::Target, version = 1;
            pubsub state: crate::api::v1::drive::State, version = 1;
            pubsub actuator_commands: crate::api::v1::drive::ActuatorCommands, version = 1;
            pubsub saturation: crate::api::v1::drive::Saturation, version = 1;
            pubsub watchdog: crate::api::v1::drive::Watchdog, version = 1;
            pubsub kinematics: crate::api::v1::drive::Kinematics, version = 1;
        }
        odometry {
            pubsub estimate: crate::api::v1::odometry::OdometryEstimate, version = 1;
            pubsub status: crate::api::v1::odometry::Status, version = 1;
            pubsub source_health: crate::api::v1::odometry::SourceHealth, version = 1;
            pubsub residuals: crate::api::v1::odometry::Residuals, version = 1;
            pubsub integration: crate::api::v1::odometry::Integration, version = 1;
        }
        joint(id) {
            pubsub data: crate::api::v1::joint::JointState, version = 1;
        }
        frame {
            pubsub tree: crate::api::v1::frame::Tree, version = 1;
            pubsub r#static: crate::api::v1::frame::Static, version = 1;
            pubsub data: crate::api::v1::frame::FrameTransform, version = 1;
            query lookup: crate::api::v1::frame::FrameLookupRequest => crate::api::v1::frame::FrameLookupResponse, version = 1;
        }
        power {
            pubsub command: crate::api::v1::power::Command, version = 1;
            pubsub state: crate::api::v1::power::State, version = 1;
        }
        presence {
            pubsub heartbeat: crate::api::v1::presence::Heartbeat, version = 1;
            pubsub summary: crate::api::v1::presence::Summary, version = 1;
            pubsub readiness: crate::api::v1::presence::DebugReadiness, version = 1;
        }
        motion {
            pubsub state: crate::api::v1::motion::State, version = 1;
            pubsub manual: crate::api::v1::motion::ManualCommand, version = 1;
            pubsub arbitration: crate::api::v1::motion::Arbitration, version = 1;
            pubsub source_freshness: crate::api::v1::motion::SourceFreshness, version = 1;
        }
        follow {
            pubsub target: crate::api::v1::follow::Target, version = 1;
            pubsub state: crate::api::v1::follow::State, version = 1;
            pubsub tracking_error: crate::api::v1::follow::TrackingError, version = 1;
            pubsub candidates: crate::api::v1::follow::Candidates, version = 1;
            pubsub costs: crate::api::v1::follow::Costs, version = 1;
            pubsub revision_inputs: crate::api::v1::follow::RevisionInputs, version = 1;
        }
        explore {
            pubsub frontiers: crate::api::v1::explore::Frontiers, version = 1;
            pubsub goal_candidates: crate::api::v1::explore::GoalCandidates, version = 1;
            pubsub state: crate::api::v1::explore::State, version = 1;
            pubsub scoring: crate::api::v1::explore::Scoring, version = 1;
            pubsub rejected_candidates: crate::api::v1::explore::RejectedCandidates, version = 1;
        }
        plan {
            pubsub path: crate::api::v1::plan::Path, version = 1;
            pubsub state: crate::api::v1::plan::State, version = 1;
            pubsub search_graph: crate::api::v1::plan::SearchGraph, version = 1;
            pubsub cost_layers: crate::api::v1::plan::CostLayers, version = 1;
            pubsub rejected_paths: crate::api::v1::plan::RejectedPaths, version = 1;
            pubsub revision_inputs: crate::api::v1::plan::RevisionInputs, version = 1;
        }
        perception {
            pubsub detections: crate::api::v1::perception::Detections, version = 1;
            pubsub state: crate::api::v1::perception::PerceptionState, version = 1;
        }
        safety {
            pubsub authorization: crate::api::v1::safety::SafetyAuthorization, version = 1;
            pubsub state: crate::api::v1::safety::State, version = 1;
            pubsub emergency_stop_request: crate::api::v1::safety::EmergencyStopRequest, version = 1;
            pubsub evidence: crate::api::v1::safety::Evidence, version = 1;
            pubsub stop_set: crate::api::v1::safety::StopSet, version = 1;
            pubsub latency_budget: crate::api::v1::safety::LatencyBudget, version = 1;
            pubsub source_health: crate::api::v1::safety::SourceHealth, version = 1;
        }
        mission {
            pubsub command: crate::api::v1::mission::MissionCommand, version = 1;
            pubsub state: crate::api::v1::mission::State, version = 1;
            pubsub goal: crate::api::v1::mission::Goal, version = 1;
            pubsub decision_trace: crate::api::v1::mission::DecisionTrace, version = 1;
            debug {
                pubsub goal_record: crate::api::v1::mission::Goal, version = 1;
            }
        }
        video {
            query open: crate::api::v1::video::OpenRequest => crate::api::v1::video::OpenResponse, version = 1;
            stream(id) {
                pubsub event: crate::api::v1::video::StreamEvent, version = 1;
            }
        }
        component(id) {
            motor(id) {
                pubsub command: crate::api::v1::component::capability::motor::Command, version = 1;
            }
            encoder(id) {
                pubsub data: crate::api::v1::component::capability::encoder::Sample, version = 1;
            }
            accelerometer(id) {
                pubsub data: crate::api::v1::component::capability::accelerometer::Sample, version = 1;
            }
            gyroscope(id) {
                pubsub data: crate::api::v1::component::capability::gyroscope::Sample, version = 1;
            }
            magnetometer(id) {
                pubsub data: crate::api::v1::component::capability::magnetometer::Sample, version = 1;
            }
            imu(id) {
                pubsub data: crate::api::v1::component::capability::imu::Sample, version = 1;
            }
            gnss(id) {
                pubsub data: crate::api::v1::component::capability::gnss::Sample, version = 1;
            }
            camera(id) {
                pubsub data: crate::api::v1::component::capability::camera::Frame, version = 1;
                profile(id) {
                    pubsub data: crate::api::v1::component::capability::camera::Frame, version = 1;
                }
            }
            depth(id) {
                pubsub data: crate::api::v1::component::capability::depth::Depth, version = 1;
                profile(id) {
                    pubsub data: crate::api::v1::component::capability::depth::Depth, version = 1;
                }
            }
            range(id) {
                pubsub data: crate::api::v1::component::capability::range::Sample, version = 1;
            }
            lidar(id) {
                pubsub data: crate::api::v1::component::capability::lidar::Scan, version = 1;
            }
            mmwave(id) {
                pubsub data: crate::api::v1::component::capability::mmwave::Scan, version = 1;
            }
            emergency_stop(id) {
                pubsub data: crate::api::v1::component::capability::emergency_stop::State, version = 1;
            }
            battery(id) {
                pubsub data: crate::api::v1::component::capability::battery::State, version = 1;
            }
        }
        asset {
            query get: crate::api::v1::asset::GetRequest => crate::api::v1::asset::GetResponse, version = 1;
        }
        simulation {
            pubsub clock: crate::api::v1::simulation::clock::Clock, version = 1;
            pubsub status: crate::api::v1::simulation::status::Status, version = 1;
            query reset: crate::api::v1::simulation::reset::Request => crate::api::v1::simulation::reset::Response, version = 1;
            robot(id) {
                pubsub pose: crate::api::v1::simulation::pose::Pose, version = 1;
                pubsub contact: crate::api::v1::simulation::contact::Contact, version = 1;
                pubsub collision: crate::api::v1::simulation::collision::Collision, version = 1;
            }
        }
        localize {
            pubsub state: crate::api::v1::localize::LocalizationState, version = 1;
            pubsub pose: crate::api::v1::localize::PoseEstimate, version = 1;
            pubsub revision: crate::api::v1::localize::LocalizationRevision, version = 1;
            pubsub keyframe: crate::api::v1::localize::Keyframe, version = 1;
            pubsub correction: crate::api::v1::localize::PoseGraphCorrection, version = 1;
            query pose_graph: crate::api::v1::localize::PoseGraphRequest => crate::api::v1::localize::PoseGraphResponse, version = 1;
            query keyframe_query: crate::api::v1::localize::KeyframeRequest => crate::api::v1::localize::KeyframeResponse, version = 1;
            query corrections: crate::api::v1::localize::CorrectionsRequest => crate::api::v1::localize::CorrectionsResponse, version = 1;
        }
        map {
            pubsub revision: crate::api::v1::map::MapRevision, version = 1;
            pubsub summary: crate::api::v1::map::Summary, version = 1;
            pubsub local_cost: crate::api::v1::map::LocalCost, version = 1;
            pubsub traversability: crate::api::v1::map::Traversability, version = 1;
            pubsub traversability_summary: crate::api::v1::map::TraversabilitySummary, version = 1;
            query submap: crate::api::v1::map::SubmapRequest => crate::api::v1::map::SubmapResponse, version = 1;
            query esdf_tile: crate::api::v1::map::EsdfTileRequest => crate::api::v1::map::EsdfTileResponse, version = 1;
            query traversability_tile: crate::api::v1::map::TraversabilityTileRequest => crate::api::v1::map::TraversabilityTileResponse, version = 1;
            query local_grid: crate::api::v1::map::LocalGridRequest => crate::api::v1::map::LocalGridResponse, version = 1;
            query global_grid: crate::api::v1::map::GlobalGridRequest => crate::api::v1::map::GlobalGridResponse, version = 1;
            query snapshot: crate::api::v1::map::SnapshotRequest => crate::api::v1::map::SnapshotResponse, version = 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::topic::{PubSub, Query, Topic};

    use super::topic;

    fn assert_pubsub<T>(topic: Topic<PubSub<T>>, key: &str, schema: &str) {
        assert_eq!(topic.key(), key);
        assert_eq!(topic.schema(), schema);
        assert_eq!(topic.version(), 1);
    }

    fn assert_query<Req, Resp>(topic: Topic<Query<Req, Resp>>, key: &str, schema: &str) {
        assert_eq!(topic.key(), key);
        assert_eq!(topic.schema(), schema);
        assert_eq!(topic.version(), 1);
    }

    #[test]
    fn topic_tree_keys_and_schemas_cover_api_domains() {
        assert_pubsub::<crate::api::v1::drive::Target>(
            topic::new().v1().drive().target(),
            "v1/drive/target",
            "v1/drive/target",
        );
        assert_pubsub::<crate::api::v1::drive::State>(
            topic::new().v1().drive().state(),
            "v1/drive/state",
            "v1/drive/state",
        );
        assert_pubsub::<crate::api::v1::drive::ActuatorCommands>(
            topic::new().v1().drive().actuator_commands(),
            "v1/drive/actuator_commands",
            "v1/drive/actuator_commands",
        );
        assert_pubsub::<crate::api::v1::drive::Saturation>(
            topic::new().v1().drive().saturation(),
            "v1/drive/saturation",
            "v1/drive/saturation",
        );
        assert_pubsub::<crate::api::v1::drive::Watchdog>(
            topic::new().v1().drive().watchdog(),
            "v1/drive/watchdog",
            "v1/drive/watchdog",
        );
        assert_pubsub::<crate::api::v1::drive::Kinematics>(
            topic::new().v1().drive().kinematics(),
            "v1/drive/kinematics",
            "v1/drive/kinematics",
        );
        assert_pubsub::<crate::api::v1::odometry::OdometryEstimate>(
            topic::new().v1().odometry().estimate(),
            "v1/odometry/estimate",
            "v1/odometry/estimate",
        );
        assert_pubsub::<crate::api::v1::odometry::Status>(
            topic::new().v1().odometry().status(),
            "v1/odometry/status",
            "v1/odometry/status",
        );
        assert_pubsub::<crate::api::v1::odometry::SourceHealth>(
            topic::new().v1().odometry().source_health(),
            "v1/odometry/source_health",
            "v1/odometry/source_health",
        );
        assert_pubsub::<crate::api::v1::odometry::Residuals>(
            topic::new().v1().odometry().residuals(),
            "v1/odometry/residuals",
            "v1/odometry/residuals",
        );
        assert_pubsub::<crate::api::v1::odometry::Integration>(
            topic::new().v1().odometry().integration(),
            "v1/odometry/integration",
            "v1/odometry/integration",
        );
        assert_pubsub::<crate::api::v1::joint::JointState>(
            topic::new().v1().joint("left_wheel").data(),
            "v1/joint/left_wheel/data",
            "v1/joint/data",
        );
        assert_pubsub::<crate::api::v1::frame::Tree>(
            topic::new().v1().frame().tree(),
            "v1/frame/tree",
            "v1/frame/tree",
        );
        assert_pubsub::<crate::api::v1::frame::Static>(
            topic::new().v1().frame().r#static(),
            "v1/frame/static",
            "v1/frame/static",
        );
        assert_pubsub::<crate::api::v1::frame::FrameTransform>(
            topic::new().v1().frame().data(),
            "v1/frame/data",
            "v1/frame/data",
        );
        assert_pubsub::<crate::api::v1::power::Command>(
            topic::new().v1().power().command(),
            "v1/power/command",
            "v1/power/command",
        );
        assert_pubsub::<crate::api::v1::power::State>(
            topic::new().v1().power().state(),
            "v1/power/state",
            "v1/power/state",
        );
        assert_pubsub::<crate::api::v1::presence::Heartbeat>(
            topic::new().v1().presence().heartbeat(),
            "v1/presence/heartbeat",
            "v1/presence/heartbeat",
        );
        assert_pubsub::<crate::api::v1::presence::Summary>(
            topic::new().v1().presence().summary(),
            "v1/presence/summary",
            "v1/presence/summary",
        );
        assert_pubsub::<crate::api::v1::presence::DebugReadiness>(
            topic::new().v1().presence().readiness(),
            "v1/presence/readiness",
            "v1/presence/readiness",
        );
        assert_pubsub::<crate::api::v1::motion::State>(
            topic::new().v1().motion().state(),
            "v1/motion/state",
            "v1/motion/state",
        );
        assert_pubsub::<crate::api::v1::motion::ManualCommand>(
            topic::new().v1().motion().manual(),
            "v1/motion/manual",
            "v1/motion/manual",
        );
        assert_pubsub::<crate::api::v1::motion::Arbitration>(
            topic::new().v1().motion().arbitration(),
            "v1/motion/arbitration",
            "v1/motion/arbitration",
        );
        assert_pubsub::<crate::api::v1::motion::SourceFreshness>(
            topic::new().v1().motion().source_freshness(),
            "v1/motion/source_freshness",
            "v1/motion/source_freshness",
        );
        assert_pubsub::<crate::api::v1::follow::Target>(
            topic::new().v1().follow().target(),
            "v1/follow/target",
            "v1/follow/target",
        );
        assert_pubsub::<crate::api::v1::follow::State>(
            topic::new().v1().follow().state(),
            "v1/follow/state",
            "v1/follow/state",
        );
        assert_pubsub::<crate::api::v1::follow::TrackingError>(
            topic::new().v1().follow().tracking_error(),
            "v1/follow/tracking_error",
            "v1/follow/tracking_error",
        );
        assert_pubsub::<crate::api::v1::follow::Candidates>(
            topic::new().v1().follow().candidates(),
            "v1/follow/candidates",
            "v1/follow/candidates",
        );
        assert_pubsub::<crate::api::v1::follow::Costs>(
            topic::new().v1().follow().costs(),
            "v1/follow/costs",
            "v1/follow/costs",
        );
        assert_pubsub::<crate::api::v1::follow::RevisionInputs>(
            topic::new().v1().follow().revision_inputs(),
            "v1/follow/revision_inputs",
            "v1/follow/revision_inputs",
        );
        assert_pubsub::<crate::api::v1::explore::Frontiers>(
            topic::new().v1().explore().frontiers(),
            "v1/explore/frontiers",
            "v1/explore/frontiers",
        );
        assert_pubsub::<crate::api::v1::explore::GoalCandidates>(
            topic::new().v1().explore().goal_candidates(),
            "v1/explore/goal_candidates",
            "v1/explore/goal_candidates",
        );
        assert_pubsub::<crate::api::v1::explore::State>(
            topic::new().v1().explore().state(),
            "v1/explore/state",
            "v1/explore/state",
        );
        assert_pubsub::<crate::api::v1::explore::Scoring>(
            topic::new().v1().explore().scoring(),
            "v1/explore/scoring",
            "v1/explore/scoring",
        );
        assert_pubsub::<crate::api::v1::explore::RejectedCandidates>(
            topic::new().v1().explore().rejected_candidates(),
            "v1/explore/rejected_candidates",
            "v1/explore/rejected_candidates",
        );
        assert_pubsub::<crate::api::v1::plan::Path>(
            topic::new().v1().plan().path(),
            "v1/plan/path",
            "v1/plan/path",
        );
        assert_pubsub::<crate::api::v1::plan::State>(
            topic::new().v1().plan().state(),
            "v1/plan/state",
            "v1/plan/state",
        );
        assert_pubsub::<crate::api::v1::plan::SearchGraph>(
            topic::new().v1().plan().search_graph(),
            "v1/plan/search_graph",
            "v1/plan/search_graph",
        );
        assert_pubsub::<crate::api::v1::plan::CostLayers>(
            topic::new().v1().plan().cost_layers(),
            "v1/plan/cost_layers",
            "v1/plan/cost_layers",
        );
        assert_pubsub::<crate::api::v1::plan::RejectedPaths>(
            topic::new().v1().plan().rejected_paths(),
            "v1/plan/rejected_paths",
            "v1/plan/rejected_paths",
        );
        assert_pubsub::<crate::api::v1::plan::RevisionInputs>(
            topic::new().v1().plan().revision_inputs(),
            "v1/plan/revision_inputs",
            "v1/plan/revision_inputs",
        );
        assert_pubsub::<crate::api::v1::perception::Detections>(
            topic::new().v1().perception().detections(),
            "v1/perception/detections",
            "v1/perception/detections",
        );
        assert_pubsub::<crate::api::v1::perception::PerceptionState>(
            topic::new().v1().perception().state(),
            "v1/perception/state",
            "v1/perception/state",
        );
        assert_pubsub::<crate::api::v1::safety::SafetyAuthorization>(
            topic::new().v1().safety().authorization(),
            "v1/safety/authorization",
            "v1/safety/authorization",
        );
        assert_pubsub::<crate::api::v1::safety::State>(
            topic::new().v1().safety().state(),
            "v1/safety/state",
            "v1/safety/state",
        );
        assert_pubsub::<crate::api::v1::safety::EmergencyStopRequest>(
            topic::new().v1().safety().emergency_stop_request(),
            "v1/safety/emergency_stop_request",
            "v1/safety/emergency_stop_request",
        );
        assert_pubsub::<crate::api::v1::safety::Evidence>(
            topic::new().v1().safety().evidence(),
            "v1/safety/evidence",
            "v1/safety/evidence",
        );
        assert_pubsub::<crate::api::v1::safety::StopSet>(
            topic::new().v1().safety().stop_set(),
            "v1/safety/stop_set",
            "v1/safety/stop_set",
        );
        assert_pubsub::<crate::api::v1::safety::LatencyBudget>(
            topic::new().v1().safety().latency_budget(),
            "v1/safety/latency_budget",
            "v1/safety/latency_budget",
        );
        assert_pubsub::<crate::api::v1::safety::SourceHealth>(
            topic::new().v1().safety().source_health(),
            "v1/safety/source_health",
            "v1/safety/source_health",
        );
        assert_pubsub::<crate::api::v1::mission::MissionCommand>(
            topic::new().v1().mission().command(),
            "v1/mission/command",
            "v1/mission/command",
        );
        assert_pubsub::<crate::api::v1::mission::State>(
            topic::new().v1().mission().state(),
            "v1/mission/state",
            "v1/mission/state",
        );
        assert_pubsub::<crate::api::v1::mission::Goal>(
            topic::new().v1().mission().goal(),
            "v1/mission/goal",
            "v1/mission/goal",
        );
        assert_pubsub::<crate::api::v1::mission::DecisionTrace>(
            topic::new().v1().mission().decision_trace(),
            "v1/mission/decision_trace",
            "v1/mission/decision_trace",
        );
        assert_pubsub::<crate::api::v1::mission::Goal>(
            topic::new().v1().mission().debug().goal_record(),
            "v1/mission/debug/goal_record",
            "v1/mission/debug/goal_record",
        );
        assert_pubsub::<crate::api::v1::video::StreamEvent>(
            topic::new().v1().video().stream("front").event(),
            "v1/video/stream/front/event",
            "v1/video/stream/event",
        );
        assert_query::<crate::api::v1::asset::GetRequest, crate::api::v1::asset::GetResponse>(
            topic::new().v1().asset().get(),
            "v1/asset/get",
            "v1/asset/get",
        );
        assert_pubsub::<crate::api::v1::simulation::clock::Clock>(
            topic::new().v1().simulation().clock(),
            "v1/simulation/clock",
            "v1/simulation/clock",
        );
        assert_pubsub::<crate::api::v1::simulation::status::Status>(
            topic::new().v1().simulation().status(),
            "v1/simulation/status",
            "v1/simulation/status",
        );
        assert_pubsub::<crate::api::v1::localize::LocalizationState>(
            topic::new().v1().localize().state(),
            "v1/localize/state",
            "v1/localize/state",
        );
        assert_pubsub::<crate::api::v1::localize::PoseEstimate>(
            topic::new().v1().localize().pose(),
            "v1/localize/pose",
            "v1/localize/pose",
        );
        assert_pubsub::<crate::api::v1::localize::LocalizationRevision>(
            topic::new().v1().localize().revision(),
            "v1/localize/revision",
            "v1/localize/revision",
        );
        assert_pubsub::<crate::api::v1::localize::Keyframe>(
            topic::new().v1().localize().keyframe(),
            "v1/localize/keyframe",
            "v1/localize/keyframe",
        );
        assert_pubsub::<crate::api::v1::localize::PoseGraphCorrection>(
            topic::new().v1().localize().correction(),
            "v1/localize/correction",
            "v1/localize/correction",
        );
        assert_pubsub::<crate::api::v1::map::MapRevision>(
            topic::new().v1().map().revision(),
            "v1/map/revision",
            "v1/map/revision",
        );
        assert_pubsub::<crate::api::v1::map::Summary>(
            topic::new().v1().map().summary(),
            "v1/map/summary",
            "v1/map/summary",
        );
        assert_pubsub::<crate::api::v1::map::LocalCost>(
            topic::new().v1().map().local_cost(),
            "v1/map/local_cost",
            "v1/map/local_cost",
        );
        assert_pubsub::<crate::api::v1::map::Traversability>(
            topic::new().v1().map().traversability(),
            "v1/map/traversability",
            "v1/map/traversability",
        );
        assert_pubsub::<crate::api::v1::map::TraversabilitySummary>(
            topic::new().v1().map().traversability_summary(),
            "v1/map/traversability_summary",
            "v1/map/traversability_summary",
        );
    }

    #[test]
    fn topic_tree_query_leaves_omit_query_path_segment() {
        assert_query::<
            crate::api::v1::frame::FrameLookupRequest,
            crate::api::v1::frame::FrameLookupResponse,
        >(
            topic::new().v1().frame().lookup(),
            "v1/frame/lookup",
            "v1/frame/lookup",
        );
        assert_query::<crate::api::v1::video::OpenRequest, crate::api::v1::video::OpenResponse>(
            topic::new().v1().video().open(),
            "v1/video/open",
            "v1/video/open",
        );
        assert_query::<crate::api::v1::asset::GetRequest, crate::api::v1::asset::GetResponse>(
            topic::new().v1().asset().get(),
            "v1/asset/get",
            "v1/asset/get",
        );
        assert_query::<
            crate::api::v1::localize::PoseGraphRequest,
            crate::api::v1::localize::PoseGraphResponse,
        >(
            topic::new().v1().localize().pose_graph(),
            "v1/localize/pose_graph",
            "v1/localize/pose_graph",
        );
        assert_query::<
            crate::api::v1::localize::KeyframeRequest,
            crate::api::v1::localize::KeyframeResponse,
        >(
            topic::new().v1().localize().keyframe_query(),
            "v1/localize/keyframe_query",
            "v1/localize/keyframe_query",
        );
        assert_query::<
            crate::api::v1::localize::CorrectionsRequest,
            crate::api::v1::localize::CorrectionsResponse,
        >(
            topic::new().v1().localize().corrections(),
            "v1/localize/corrections",
            "v1/localize/corrections",
        );
        assert_query::<
            crate::api::v1::map::TraversabilityTileRequest,
            crate::api::v1::map::TraversabilityTileResponse,
        >(
            topic::new().v1().map().traversability_tile(),
            "v1/map/traversability_tile",
            "v1/map/traversability_tile",
        );
        assert_query::<crate::api::v1::map::SubmapRequest, crate::api::v1::map::SubmapResponse>(
            topic::new().v1().map().submap(),
            "v1/map/submap",
            "v1/map/submap",
        );
        assert_query::<crate::api::v1::map::EsdfTileRequest, crate::api::v1::map::EsdfTileResponse>(
            topic::new().v1().map().esdf_tile(),
            "v1/map/esdf_tile",
            "v1/map/esdf_tile",
        );
        assert_query::<crate::api::v1::map::LocalGridRequest, crate::api::v1::map::LocalGridResponse>(
            topic::new().v1().map().local_grid(),
            "v1/map/local_grid",
            "v1/map/local_grid",
        );
        assert_query::<
            crate::api::v1::map::GlobalGridRequest,
            crate::api::v1::map::GlobalGridResponse,
        >(
            topic::new().v1().map().global_grid(),
            "v1/map/global_grid",
            "v1/map/global_grid",
        );
        assert_query::<crate::api::v1::map::SnapshotRequest, crate::api::v1::map::SnapshotResponse>(
            topic::new().v1().map().snapshot(),
            "v1/map/snapshot",
            "v1/map/snapshot",
        );
        assert_query::<
            crate::api::v1::simulation::reset::Request,
            crate::api::v1::simulation::reset::Response,
        >(
            topic::new().v1().simulation().reset(),
            "v1/simulation/reset",
            "v1/simulation/reset",
        );
    }

    #[test]
    fn topic_tree_component_and_simulation_slots_elide_holes_in_schemas() {
        assert_pubsub::<crate::api::v1::component::capability::motor::Command>(
            topic::new()
                .v1()
                .component("base")
                .motor("left_wheel")
                .command(),
            "v1/component/base/motor/left_wheel/command",
            "v1/component/motor/command",
        );
        assert_pubsub::<crate::api::v1::component::capability::encoder::Sample>(
            topic::new()
                .v1()
                .component("base")
                .encoder("left_wheel")
                .data(),
            "v1/component/base/encoder/left_wheel/data",
            "v1/component/encoder/data",
        );
        assert_pubsub::<crate::api::v1::component::capability::accelerometer::Sample>(
            topic::new()
                .v1()
                .component("imu_board")
                .accelerometer("accel")
                .data(),
            "v1/component/imu_board/accelerometer/accel/data",
            "v1/component/accelerometer/data",
        );
        assert_pubsub::<crate::api::v1::component::capability::gyroscope::Sample>(
            topic::new()
                .v1()
                .component("imu_board")
                .gyroscope("gyro")
                .data(),
            "v1/component/imu_board/gyroscope/gyro/data",
            "v1/component/gyroscope/data",
        );
        assert_pubsub::<crate::api::v1::component::capability::magnetometer::Sample>(
            topic::new()
                .v1()
                .component("imu_board")
                .magnetometer("mag")
                .data(),
            "v1/component/imu_board/magnetometer/mag/data",
            "v1/component/magnetometer/data",
        );
        assert_pubsub::<crate::api::v1::component::capability::imu::Sample>(
            topic::new().v1().component("imu_board").imu("imu").data(),
            "v1/component/imu_board/imu/imu/data",
            "v1/component/imu/data",
        );
        assert_pubsub::<crate::api::v1::component::capability::gnss::Sample>(
            topic::new().v1().component("gps").gnss("gnss").data(),
            "v1/component/gps/gnss/gnss/data",
            "v1/component/gnss/data",
        );
        assert_pubsub::<crate::api::v1::component::capability::camera::Frame>(
            topic::new().v1().component("head").camera("front").data(),
            "v1/component/head/camera/front/data",
            "v1/component/camera/data",
        );
        assert_pubsub::<crate::api::v1::component::capability::camera::Frame>(
            topic::new()
                .v1()
                .component("head")
                .camera("front")
                .profile("r640x480_h10_rgb8")
                .data(),
            "v1/component/head/camera/front/profile/r640x480_h10_rgb8/data",
            "v1/component/camera/profile/data",
        );
        assert_pubsub::<crate::api::v1::component::capability::depth::Depth>(
            topic::new()
                .v1()
                .component("head")
                .depth("front_depth")
                .data(),
            "v1/component/head/depth/front_depth/data",
            "v1/component/depth/data",
        );
        assert_pubsub::<crate::api::v1::component::capability::depth::Depth>(
            topic::new()
                .v1()
                .component("head")
                .depth("front_depth")
                .profile("r320x240_h5_depth_mm")
                .data(),
            "v1/component/head/depth/front_depth/profile/r320x240_h5_depth_mm/data",
            "v1/component/depth/profile/data",
        );
        assert_pubsub::<crate::api::v1::component::capability::range::Sample>(
            topic::new()
                .v1()
                .component("base")
                .range("front_tof")
                .data(),
            "v1/component/base/range/front_tof/data",
            "v1/component/range/data",
        );
        assert_pubsub::<crate::api::v1::component::capability::lidar::Scan>(
            topic::new()
                .v1()
                .component("front_lidar")
                .lidar("scan")
                .data(),
            "v1/component/front_lidar/lidar/scan/data",
            "v1/component/lidar/data",
        );
        assert_pubsub::<crate::api::v1::component::capability::mmwave::Scan>(
            topic::new().v1().component("radar").mmwave("mmwave").data(),
            "v1/component/radar/mmwave/mmwave/data",
            "v1/component/mmwave/data",
        );
        assert_pubsub::<crate::api::v1::component::capability::emergency_stop::State>(
            topic::new()
                .v1()
                .component("safety_panel")
                .emergency_stop("estop")
                .data(),
            "v1/component/safety_panel/emergency_stop/estop/data",
            "v1/component/emergency_stop/data",
        );
        assert_pubsub::<crate::api::v1::component::capability::battery::State>(
            topic::new()
                .v1()
                .component("power_board")
                .battery("main_battery")
                .data(),
            "v1/component/power_board/battery/main_battery/data",
            "v1/component/battery/data",
        );
        assert_pubsub::<crate::api::v1::simulation::pose::Pose>(
            topic::new().v1().simulation().robot("r1").pose(),
            "v1/simulation/robot/r1/pose",
            "v1/simulation/robot/pose",
        );
        assert_pubsub::<crate::api::v1::simulation::contact::Contact>(
            topic::new().v1().simulation().robot("r1").contact(),
            "v1/simulation/robot/r1/contact",
            "v1/simulation/robot/contact",
        );
        assert_pubsub::<crate::api::v1::simulation::collision::Collision>(
            topic::new().v1().simulation().robot("r1").collision(),
            "v1/simulation/robot/r1/collision",
            "v1/simulation/robot/collision",
        );
    }
}
