crate::bus::topic_tree! {
    pub mod topic;
    v1 {
        drive {
            pubsub target: crate::api::drive::v1::Target, version = 1;
            pubsub state: crate::api::drive::v1::State, version = 1;
            pubsub actuator_commands: crate::api::drive::v1::ActuatorCommands, version = 1;
            pubsub saturation: crate::api::drive::v1::Saturation, version = 1;
            pubsub watchdog: crate::api::drive::v1::Watchdog, version = 1;
            pubsub kinematics: crate::api::drive::v1::Kinematics, version = 1;
        }
        odometry {
            pubsub estimate: crate::api::odometry::v1::OdometryEstimate, version = 1;
            pubsub status: crate::api::odometry::v1::Status, version = 1;
            pubsub source_health: crate::api::odometry::v1::SourceHealth, version = 1;
            pubsub residuals: crate::api::odometry::v1::Residuals, version = 1;
            pubsub integration: crate::api::odometry::v1::Integration, version = 1;
        }
        joint(id) {
            pubsub data: crate::api::joint::v1::JointState, version = 1;
        }
        frame {
            pubsub tree: crate::api::frame::v1::Tree, version = 1;
            pubsub r#static: crate::api::frame::v1::Static, version = 1;
            pubsub data: crate::api::frame::v1::FrameTransform, version = 1;
            query lookup: crate::api::frame::v1::FrameLookupRequest => crate::api::frame::v1::FrameLookupResponse, version = 1;
        }
        power {
            pubsub command: crate::api::power::v1::Command, version = 1;
            pubsub state: crate::api::power::v1::State, version = 1;
        }
        presence {
            pubsub heartbeat: crate::api::presence::Heartbeat, version = 1;
            pubsub summary: crate::api::presence::Summary, version = 1;
            pubsub readiness: crate::api::presence::DebugReadiness, version = 1;
        }
        motion {
            pubsub state: crate::api::motion::v1::State, version = 1;
            pubsub manual: crate::api::motion::v1::ManualCommand, version = 1;
            pubsub arbitration: crate::api::motion::v1::Arbitration, version = 1;
            pubsub source_freshness: crate::api::motion::v1::SourceFreshness, version = 1;
        }
        follow {
            pubsub target: crate::api::follow::v1::Target, version = 1;
            pubsub state: crate::api::follow::v1::State, version = 1;
            pubsub tracking_error: crate::api::follow::v1::TrackingError, version = 1;
            pubsub candidates: crate::api::follow::v1::Candidates, version = 1;
            pubsub costs: crate::api::follow::v1::Costs, version = 1;
            pubsub revision_inputs: crate::api::follow::v1::RevisionInputs, version = 1;
        }
        explore {
            pubsub frontiers: crate::api::explore::v1::Frontiers, version = 1;
            pubsub goal_candidates: crate::api::explore::v1::GoalCandidates, version = 1;
            pubsub state: crate::api::explore::v1::State, version = 1;
            pubsub scoring: crate::api::explore::v1::Scoring, version = 1;
            pubsub rejected_candidates: crate::api::explore::v1::RejectedCandidates, version = 1;
        }
        plan {
            pubsub path: crate::api::plan::v1::Path, version = 1;
            pubsub state: crate::api::plan::v1::State, version = 1;
            pubsub search_graph: crate::api::plan::v1::SearchGraph, version = 1;
            pubsub cost_layers: crate::api::plan::v1::CostLayers, version = 1;
            pubsub rejected_paths: crate::api::plan::v1::RejectedPaths, version = 1;
            pubsub revision_inputs: crate::api::plan::v1::RevisionInputs, version = 1;
        }
        perception {
            pubsub detections: crate::api::perception::v1::Detections, version = 1;
            pubsub state: crate::api::perception::v1::PerceptionState, version = 1;
        }
        safety {
            pubsub authorization: crate::api::safety::v1::SafetyAuthorization, version = 1;
            pubsub state: crate::api::safety::v1::State, version = 1;
            pubsub emergency_stop_request: crate::api::safety::v1::EmergencyStopRequest, version = 1;
            pubsub evidence: crate::api::safety::v1::Evidence, version = 1;
            pubsub stop_set: crate::api::safety::v1::StopSet, version = 1;
            pubsub latency_budget: crate::api::safety::v1::LatencyBudget, version = 1;
            pubsub source_health: crate::api::safety::v1::SourceHealth, version = 1;
        }
        mission {
            pubsub command: crate::api::mission::v1::MissionCommand, version = 1;
            pubsub state: crate::api::mission::v1::State, version = 1;
            pubsub goal: crate::api::mission::v1::Goal, version = 1;
            pubsub decision_trace: crate::api::mission::v1::DecisionTrace, version = 1;
            debug {
                pubsub goal_record: crate::api::mission::v1::Goal, version = 1;
            }
        }
        video {
            query open: crate::api::video::v1::OpenRequest => crate::api::video::v1::OpenResponse, version = 1;
            stream(id) {
                pubsub event: crate::api::video::v1::StreamEvent, version = 1;
            }
        }
        component(id) {
            motor(id) {
                pubsub command: crate::api::component::v1::capability::motor::Command, version = 1;
            }
            encoder(id) {
                pubsub data: crate::api::component::v1::capability::encoder::Sample, version = 1;
            }
            accelerometer(id) {
                pubsub data: crate::api::component::v1::capability::accelerometer::Sample, version = 1;
            }
            gyroscope(id) {
                pubsub data: crate::api::component::v1::capability::gyroscope::Sample, version = 1;
            }
            magnetometer(id) {
                pubsub data: crate::api::component::v1::capability::magnetometer::Sample, version = 1;
            }
            imu(id) {
                pubsub data: crate::api::component::v1::capability::imu::Sample, version = 1;
            }
            gnss(id) {
                pubsub data: crate::api::component::v1::capability::gnss::Sample, version = 1;
            }
            camera(id) {
                pubsub data: crate::api::component::v1::capability::camera::Frame, version = 1;
                profile(id) {
                    pubsub data: crate::api::component::v1::capability::camera::Frame, version = 1;
                }
            }
            depth(id) {
                pubsub data: crate::api::component::v1::capability::depth::Depth, version = 1;
                profile(id) {
                    pubsub data: crate::api::component::v1::capability::depth::Depth, version = 1;
                }
            }
            range(id) {
                pubsub data: crate::api::component::v1::capability::range::Sample, version = 1;
            }
            lidar(id) {
                pubsub data: crate::api::component::v1::capability::lidar::Scan, version = 1;
            }
            mmwave(id) {
                pubsub data: crate::api::component::v1::capability::mmwave::Scan, version = 1;
            }
            emergency_stop(id) {
                pubsub data: crate::api::component::v1::capability::emergency_stop::State, version = 1;
            }
            battery(id) {
                pubsub data: crate::api::component::v1::capability::battery::State, version = 1;
            }
        }
        asset {
            query get: crate::api::asset::v1::GetRequest => crate::api::asset::v1::GetResponse, version = 1;
        }
        simulation {
            pubsub clock: crate::api::simulation::v1::clock::Clock, version = 1;
            pubsub status: crate::api::simulation::v1::status::Status, version = 1;
            query reset: crate::api::simulation::v1::reset::Request => crate::api::simulation::v1::reset::Response, version = 1;
            robot(id) {
                pubsub pose: crate::api::simulation::v1::pose::Pose, version = 1;
                pubsub contact: crate::api::simulation::v1::contact::Contact, version = 1;
                pubsub collision: crate::api::simulation::v1::collision::Collision, version = 1;
            }
        }
        localize {
            pubsub state: crate::api::localize::v1::LocalizationState, version = 1;
            pubsub pose: crate::api::localize::v1::PoseEstimate, version = 1;
            pubsub revision: crate::api::localize::v1::LocalizationRevision, version = 1;
            pubsub keyframe: crate::api::localize::v1::Keyframe, version = 1;
            pubsub correction: crate::api::localize::v1::PoseGraphCorrection, version = 1;
            query pose_graph: crate::api::localize::v1::PoseGraphRequest => crate::api::localize::v1::PoseGraphResponse, version = 1;
            query keyframe_query: crate::api::localize::v1::KeyframeRequest => crate::api::localize::v1::KeyframeResponse, version = 1;
            query corrections: crate::api::localize::v1::CorrectionsRequest => crate::api::localize::v1::CorrectionsResponse, version = 1;
        }
        map {
            pubsub revision: crate::api::map::v1::MapRevision, version = 1;
            pubsub summary: crate::api::map::v1::Summary, version = 1;
            pubsub local_cost: crate::api::map::v1::LocalCost, version = 1;
            pubsub traversability: crate::api::map::v1::Traversability, version = 1;
            pubsub traversability_summary: crate::api::map::v1::TraversabilitySummary, version = 1;
            query submap: crate::api::map::v1::SubmapRequest => crate::api::map::v1::SubmapResponse, version = 1;
            query esdf_tile: crate::api::map::v1::EsdfTileRequest => crate::api::map::v1::EsdfTileResponse, version = 1;
            query traversability_tile: crate::api::map::v1::TraversabilityTileRequest => crate::api::map::v1::TraversabilityTileResponse, version = 1;
            query local_grid: crate::api::map::v1::LocalGridRequest => crate::api::map::v1::LocalGridResponse, version = 1;
            query global_grid: crate::api::map::v1::GlobalGridRequest => crate::api::map::v1::GlobalGridResponse, version = 1;
            query snapshot: crate::api::map::v1::SnapshotRequest => crate::api::map::v1::SnapshotResponse, version = 1;
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
        assert_pubsub::<crate::api::drive::v1::Target>(
            topic::new().v1().drive().target(),
            "v1/drive/target",
            "v1/drive/target",
        );
        assert_pubsub::<crate::api::drive::v1::State>(
            topic::new().v1().drive().state(),
            "v1/drive/state",
            "v1/drive/state",
        );
        assert_pubsub::<crate::api::drive::v1::ActuatorCommands>(
            topic::new().v1().drive().actuator_commands(),
            "v1/drive/actuator_commands",
            "v1/drive/actuator_commands",
        );
        assert_pubsub::<crate::api::drive::v1::Saturation>(
            topic::new().v1().drive().saturation(),
            "v1/drive/saturation",
            "v1/drive/saturation",
        );
        assert_pubsub::<crate::api::drive::v1::Watchdog>(
            topic::new().v1().drive().watchdog(),
            "v1/drive/watchdog",
            "v1/drive/watchdog",
        );
        assert_pubsub::<crate::api::drive::v1::Kinematics>(
            topic::new().v1().drive().kinematics(),
            "v1/drive/kinematics",
            "v1/drive/kinematics",
        );
        assert_pubsub::<crate::api::odometry::v1::OdometryEstimate>(
            topic::new().v1().odometry().estimate(),
            "v1/odometry/estimate",
            "v1/odometry/estimate",
        );
        assert_pubsub::<crate::api::odometry::v1::Status>(
            topic::new().v1().odometry().status(),
            "v1/odometry/status",
            "v1/odometry/status",
        );
        assert_pubsub::<crate::api::odometry::v1::SourceHealth>(
            topic::new().v1().odometry().source_health(),
            "v1/odometry/source_health",
            "v1/odometry/source_health",
        );
        assert_pubsub::<crate::api::odometry::v1::Residuals>(
            topic::new().v1().odometry().residuals(),
            "v1/odometry/residuals",
            "v1/odometry/residuals",
        );
        assert_pubsub::<crate::api::odometry::v1::Integration>(
            topic::new().v1().odometry().integration(),
            "v1/odometry/integration",
            "v1/odometry/integration",
        );
        assert_pubsub::<crate::api::joint::v1::JointState>(
            topic::new().v1().joint("left_wheel").data(),
            "v1/joint/left_wheel/data",
            "v1/joint/data",
        );
        assert_pubsub::<crate::api::frame::v1::Tree>(
            topic::new().v1().frame().tree(),
            "v1/frame/tree",
            "v1/frame/tree",
        );
        assert_pubsub::<crate::api::frame::v1::Static>(
            topic::new().v1().frame().r#static(),
            "v1/frame/static",
            "v1/frame/static",
        );
        assert_pubsub::<crate::api::frame::v1::FrameTransform>(
            topic::new().v1().frame().data(),
            "v1/frame/data",
            "v1/frame/data",
        );
        assert_pubsub::<crate::api::power::v1::Command>(
            topic::new().v1().power().command(),
            "v1/power/command",
            "v1/power/command",
        );
        assert_pubsub::<crate::api::power::v1::State>(
            topic::new().v1().power().state(),
            "v1/power/state",
            "v1/power/state",
        );
        assert_pubsub::<crate::api::presence::Heartbeat>(
            topic::new().v1().presence().heartbeat(),
            "v1/presence/heartbeat",
            "v1/presence/heartbeat",
        );
        assert_pubsub::<crate::api::presence::Summary>(
            topic::new().v1().presence().summary(),
            "v1/presence/summary",
            "v1/presence/summary",
        );
        assert_pubsub::<crate::api::presence::DebugReadiness>(
            topic::new().v1().presence().readiness(),
            "v1/presence/readiness",
            "v1/presence/readiness",
        );
        assert_pubsub::<crate::api::motion::v1::State>(
            topic::new().v1().motion().state(),
            "v1/motion/state",
            "v1/motion/state",
        );
        assert_pubsub::<crate::api::motion::v1::ManualCommand>(
            topic::new().v1().motion().manual(),
            "v1/motion/manual",
            "v1/motion/manual",
        );
        assert_pubsub::<crate::api::motion::v1::Arbitration>(
            topic::new().v1().motion().arbitration(),
            "v1/motion/arbitration",
            "v1/motion/arbitration",
        );
        assert_pubsub::<crate::api::motion::v1::SourceFreshness>(
            topic::new().v1().motion().source_freshness(),
            "v1/motion/source_freshness",
            "v1/motion/source_freshness",
        );
        assert_pubsub::<crate::api::follow::v1::Target>(
            topic::new().v1().follow().target(),
            "v1/follow/target",
            "v1/follow/target",
        );
        assert_pubsub::<crate::api::follow::v1::State>(
            topic::new().v1().follow().state(),
            "v1/follow/state",
            "v1/follow/state",
        );
        assert_pubsub::<crate::api::follow::v1::TrackingError>(
            topic::new().v1().follow().tracking_error(),
            "v1/follow/tracking_error",
            "v1/follow/tracking_error",
        );
        assert_pubsub::<crate::api::follow::v1::Candidates>(
            topic::new().v1().follow().candidates(),
            "v1/follow/candidates",
            "v1/follow/candidates",
        );
        assert_pubsub::<crate::api::follow::v1::Costs>(
            topic::new().v1().follow().costs(),
            "v1/follow/costs",
            "v1/follow/costs",
        );
        assert_pubsub::<crate::api::follow::v1::RevisionInputs>(
            topic::new().v1().follow().revision_inputs(),
            "v1/follow/revision_inputs",
            "v1/follow/revision_inputs",
        );
        assert_pubsub::<crate::api::explore::v1::Frontiers>(
            topic::new().v1().explore().frontiers(),
            "v1/explore/frontiers",
            "v1/explore/frontiers",
        );
        assert_pubsub::<crate::api::explore::v1::GoalCandidates>(
            topic::new().v1().explore().goal_candidates(),
            "v1/explore/goal_candidates",
            "v1/explore/goal_candidates",
        );
        assert_pubsub::<crate::api::explore::v1::State>(
            topic::new().v1().explore().state(),
            "v1/explore/state",
            "v1/explore/state",
        );
        assert_pubsub::<crate::api::explore::v1::Scoring>(
            topic::new().v1().explore().scoring(),
            "v1/explore/scoring",
            "v1/explore/scoring",
        );
        assert_pubsub::<crate::api::explore::v1::RejectedCandidates>(
            topic::new().v1().explore().rejected_candidates(),
            "v1/explore/rejected_candidates",
            "v1/explore/rejected_candidates",
        );
        assert_pubsub::<crate::api::plan::v1::Path>(
            topic::new().v1().plan().path(),
            "v1/plan/path",
            "v1/plan/path",
        );
        assert_pubsub::<crate::api::plan::v1::State>(
            topic::new().v1().plan().state(),
            "v1/plan/state",
            "v1/plan/state",
        );
        assert_pubsub::<crate::api::plan::v1::SearchGraph>(
            topic::new().v1().plan().search_graph(),
            "v1/plan/search_graph",
            "v1/plan/search_graph",
        );
        assert_pubsub::<crate::api::plan::v1::CostLayers>(
            topic::new().v1().plan().cost_layers(),
            "v1/plan/cost_layers",
            "v1/plan/cost_layers",
        );
        assert_pubsub::<crate::api::plan::v1::RejectedPaths>(
            topic::new().v1().plan().rejected_paths(),
            "v1/plan/rejected_paths",
            "v1/plan/rejected_paths",
        );
        assert_pubsub::<crate::api::plan::v1::RevisionInputs>(
            topic::new().v1().plan().revision_inputs(),
            "v1/plan/revision_inputs",
            "v1/plan/revision_inputs",
        );
        assert_pubsub::<crate::api::perception::v1::Detections>(
            topic::new().v1().perception().detections(),
            "v1/perception/detections",
            "v1/perception/detections",
        );
        assert_pubsub::<crate::api::perception::v1::PerceptionState>(
            topic::new().v1().perception().state(),
            "v1/perception/state",
            "v1/perception/state",
        );
        assert_pubsub::<crate::api::safety::v1::SafetyAuthorization>(
            topic::new().v1().safety().authorization(),
            "v1/safety/authorization",
            "v1/safety/authorization",
        );
        assert_pubsub::<crate::api::safety::v1::State>(
            topic::new().v1().safety().state(),
            "v1/safety/state",
            "v1/safety/state",
        );
        assert_pubsub::<crate::api::safety::v1::EmergencyStopRequest>(
            topic::new().v1().safety().emergency_stop_request(),
            "v1/safety/emergency_stop_request",
            "v1/safety/emergency_stop_request",
        );
        assert_pubsub::<crate::api::safety::v1::Evidence>(
            topic::new().v1().safety().evidence(),
            "v1/safety/evidence",
            "v1/safety/evidence",
        );
        assert_pubsub::<crate::api::safety::v1::StopSet>(
            topic::new().v1().safety().stop_set(),
            "v1/safety/stop_set",
            "v1/safety/stop_set",
        );
        assert_pubsub::<crate::api::safety::v1::LatencyBudget>(
            topic::new().v1().safety().latency_budget(),
            "v1/safety/latency_budget",
            "v1/safety/latency_budget",
        );
        assert_pubsub::<crate::api::safety::v1::SourceHealth>(
            topic::new().v1().safety().source_health(),
            "v1/safety/source_health",
            "v1/safety/source_health",
        );
        assert_pubsub::<crate::api::mission::v1::MissionCommand>(
            topic::new().v1().mission().command(),
            "v1/mission/command",
            "v1/mission/command",
        );
        assert_pubsub::<crate::api::mission::v1::State>(
            topic::new().v1().mission().state(),
            "v1/mission/state",
            "v1/mission/state",
        );
        assert_pubsub::<crate::api::mission::v1::Goal>(
            topic::new().v1().mission().goal(),
            "v1/mission/goal",
            "v1/mission/goal",
        );
        assert_pubsub::<crate::api::mission::v1::DecisionTrace>(
            topic::new().v1().mission().decision_trace(),
            "v1/mission/decision_trace",
            "v1/mission/decision_trace",
        );
        assert_pubsub::<crate::api::mission::v1::Goal>(
            topic::new().v1().mission().debug().goal_record(),
            "v1/mission/debug/goal_record",
            "v1/mission/debug/goal_record",
        );
        assert_pubsub::<crate::api::video::v1::StreamEvent>(
            topic::new().v1().video().stream("front").event(),
            "v1/video/stream/front/event",
            "v1/video/stream/event",
        );
        assert_query::<crate::api::asset::v1::GetRequest, crate::api::asset::v1::GetResponse>(
            topic::new().v1().asset().get(),
            "v1/asset/get",
            "v1/asset/get",
        );
        assert_pubsub::<crate::api::simulation::v1::clock::Clock>(
            topic::new().v1().simulation().clock(),
            "v1/simulation/clock",
            "v1/simulation/clock",
        );
        assert_pubsub::<crate::api::simulation::v1::status::Status>(
            topic::new().v1().simulation().status(),
            "v1/simulation/status",
            "v1/simulation/status",
        );
        assert_pubsub::<crate::api::localize::v1::LocalizationState>(
            topic::new().v1().localize().state(),
            "v1/localize/state",
            "v1/localize/state",
        );
        assert_pubsub::<crate::api::localize::v1::PoseEstimate>(
            topic::new().v1().localize().pose(),
            "v1/localize/pose",
            "v1/localize/pose",
        );
        assert_pubsub::<crate::api::localize::v1::LocalizationRevision>(
            topic::new().v1().localize().revision(),
            "v1/localize/revision",
            "v1/localize/revision",
        );
        assert_pubsub::<crate::api::localize::v1::Keyframe>(
            topic::new().v1().localize().keyframe(),
            "v1/localize/keyframe",
            "v1/localize/keyframe",
        );
        assert_pubsub::<crate::api::localize::v1::PoseGraphCorrection>(
            topic::new().v1().localize().correction(),
            "v1/localize/correction",
            "v1/localize/correction",
        );
        assert_pubsub::<crate::api::map::v1::MapRevision>(
            topic::new().v1().map().revision(),
            "v1/map/revision",
            "v1/map/revision",
        );
        assert_pubsub::<crate::api::map::v1::Summary>(
            topic::new().v1().map().summary(),
            "v1/map/summary",
            "v1/map/summary",
        );
        assert_pubsub::<crate::api::map::v1::LocalCost>(
            topic::new().v1().map().local_cost(),
            "v1/map/local_cost",
            "v1/map/local_cost",
        );
        assert_pubsub::<crate::api::map::v1::Traversability>(
            topic::new().v1().map().traversability(),
            "v1/map/traversability",
            "v1/map/traversability",
        );
        assert_pubsub::<crate::api::map::v1::TraversabilitySummary>(
            topic::new().v1().map().traversability_summary(),
            "v1/map/traversability_summary",
            "v1/map/traversability_summary",
        );
    }

    #[test]
    fn topic_tree_query_leaves_omit_query_path_segment() {
        assert_query::<
            crate::api::frame::v1::FrameLookupRequest,
            crate::api::frame::v1::FrameLookupResponse,
        >(
            topic::new().v1().frame().lookup(),
            "v1/frame/lookup",
            "v1/frame/lookup",
        );
        assert_query::<crate::api::video::v1::OpenRequest, crate::api::video::v1::OpenResponse>(
            topic::new().v1().video().open(),
            "v1/video/open",
            "v1/video/open",
        );
        assert_query::<crate::api::asset::v1::GetRequest, crate::api::asset::v1::GetResponse>(
            topic::new().v1().asset().get(),
            "v1/asset/get",
            "v1/asset/get",
        );
        assert_query::<
            crate::api::localize::v1::PoseGraphRequest,
            crate::api::localize::v1::PoseGraphResponse,
        >(
            topic::new().v1().localize().pose_graph(),
            "v1/localize/pose_graph",
            "v1/localize/pose_graph",
        );
        assert_query::<
            crate::api::localize::v1::KeyframeRequest,
            crate::api::localize::v1::KeyframeResponse,
        >(
            topic::new().v1().localize().keyframe_query(),
            "v1/localize/keyframe_query",
            "v1/localize/keyframe_query",
        );
        assert_query::<
            crate::api::localize::v1::CorrectionsRequest,
            crate::api::localize::v1::CorrectionsResponse,
        >(
            topic::new().v1().localize().corrections(),
            "v1/localize/corrections",
            "v1/localize/corrections",
        );
        assert_query::<
            crate::api::map::v1::TraversabilityTileRequest,
            crate::api::map::v1::TraversabilityTileResponse,
        >(
            topic::new().v1().map().traversability_tile(),
            "v1/map/traversability_tile",
            "v1/map/traversability_tile",
        );
        assert_query::<crate::api::map::v1::SubmapRequest, crate::api::map::v1::SubmapResponse>(
            topic::new().v1().map().submap(),
            "v1/map/submap",
            "v1/map/submap",
        );
        assert_query::<crate::api::map::v1::EsdfTileRequest, crate::api::map::v1::EsdfTileResponse>(
            topic::new().v1().map().esdf_tile(),
            "v1/map/esdf_tile",
            "v1/map/esdf_tile",
        );
        assert_query::<crate::api::map::v1::LocalGridRequest, crate::api::map::v1::LocalGridResponse>(
            topic::new().v1().map().local_grid(),
            "v1/map/local_grid",
            "v1/map/local_grid",
        );
        assert_query::<
            crate::api::map::v1::GlobalGridRequest,
            crate::api::map::v1::GlobalGridResponse,
        >(
            topic::new().v1().map().global_grid(),
            "v1/map/global_grid",
            "v1/map/global_grid",
        );
        assert_query::<crate::api::map::v1::SnapshotRequest, crate::api::map::v1::SnapshotResponse>(
            topic::new().v1().map().snapshot(),
            "v1/map/snapshot",
            "v1/map/snapshot",
        );
        assert_query::<
            crate::api::simulation::v1::reset::Request,
            crate::api::simulation::v1::reset::Response,
        >(
            topic::new().v1().simulation().reset(),
            "v1/simulation/reset",
            "v1/simulation/reset",
        );
    }

    #[test]
    fn topic_tree_component_and_simulation_slots_elide_holes_in_schemas() {
        assert_pubsub::<crate::api::component::v1::capability::motor::Command>(
            topic::new()
                .v1()
                .component("base")
                .motor("left_wheel")
                .command(),
            "v1/component/base/motor/left_wheel/command",
            "v1/component/motor/command",
        );
        assert_pubsub::<crate::api::component::v1::capability::encoder::Sample>(
            topic::new()
                .v1()
                .component("base")
                .encoder("left_wheel")
                .data(),
            "v1/component/base/encoder/left_wheel/data",
            "v1/component/encoder/data",
        );
        assert_pubsub::<crate::api::component::v1::capability::accelerometer::Sample>(
            topic::new()
                .v1()
                .component("imu_board")
                .accelerometer("accel")
                .data(),
            "v1/component/imu_board/accelerometer/accel/data",
            "v1/component/accelerometer/data",
        );
        assert_pubsub::<crate::api::component::v1::capability::gyroscope::Sample>(
            topic::new()
                .v1()
                .component("imu_board")
                .gyroscope("gyro")
                .data(),
            "v1/component/imu_board/gyroscope/gyro/data",
            "v1/component/gyroscope/data",
        );
        assert_pubsub::<crate::api::component::v1::capability::magnetometer::Sample>(
            topic::new()
                .v1()
                .component("imu_board")
                .magnetometer("mag")
                .data(),
            "v1/component/imu_board/magnetometer/mag/data",
            "v1/component/magnetometer/data",
        );
        assert_pubsub::<crate::api::component::v1::capability::imu::Sample>(
            topic::new().v1().component("imu_board").imu("imu").data(),
            "v1/component/imu_board/imu/imu/data",
            "v1/component/imu/data",
        );
        assert_pubsub::<crate::api::component::v1::capability::gnss::Sample>(
            topic::new().v1().component("gps").gnss("gnss").data(),
            "v1/component/gps/gnss/gnss/data",
            "v1/component/gnss/data",
        );
        assert_pubsub::<crate::api::component::v1::capability::camera::Frame>(
            topic::new().v1().component("head").camera("front").data(),
            "v1/component/head/camera/front/data",
            "v1/component/camera/data",
        );
        assert_pubsub::<crate::api::component::v1::capability::camera::Frame>(
            topic::new()
                .v1()
                .component("head")
                .camera("front")
                .profile("r640x480_h10_rgb8")
                .data(),
            "v1/component/head/camera/front/profile/r640x480_h10_rgb8/data",
            "v1/component/camera/profile/data",
        );
        assert_pubsub::<crate::api::component::v1::capability::depth::Depth>(
            topic::new()
                .v1()
                .component("head")
                .depth("front_depth")
                .data(),
            "v1/component/head/depth/front_depth/data",
            "v1/component/depth/data",
        );
        assert_pubsub::<crate::api::component::v1::capability::depth::Depth>(
            topic::new()
                .v1()
                .component("head")
                .depth("front_depth")
                .profile("r320x240_h5_depth_mm")
                .data(),
            "v1/component/head/depth/front_depth/profile/r320x240_h5_depth_mm/data",
            "v1/component/depth/profile/data",
        );
        assert_pubsub::<crate::api::component::v1::capability::range::Sample>(
            topic::new()
                .v1()
                .component("base")
                .range("front_tof")
                .data(),
            "v1/component/base/range/front_tof/data",
            "v1/component/range/data",
        );
        assert_pubsub::<crate::api::component::v1::capability::lidar::Scan>(
            topic::new()
                .v1()
                .component("front_lidar")
                .lidar("scan")
                .data(),
            "v1/component/front_lidar/lidar/scan/data",
            "v1/component/lidar/data",
        );
        assert_pubsub::<crate::api::component::v1::capability::mmwave::Scan>(
            topic::new().v1().component("radar").mmwave("mmwave").data(),
            "v1/component/radar/mmwave/mmwave/data",
            "v1/component/mmwave/data",
        );
        assert_pubsub::<crate::api::component::v1::capability::emergency_stop::State>(
            topic::new()
                .v1()
                .component("safety_panel")
                .emergency_stop("estop")
                .data(),
            "v1/component/safety_panel/emergency_stop/estop/data",
            "v1/component/emergency_stop/data",
        );
        assert_pubsub::<crate::api::component::v1::capability::battery::State>(
            topic::new()
                .v1()
                .component("power_board")
                .battery("main_battery")
                .data(),
            "v1/component/power_board/battery/main_battery/data",
            "v1/component/battery/data",
        );
        assert_pubsub::<crate::api::simulation::v1::pose::Pose>(
            topic::new().v1().simulation().robot("r1").pose(),
            "v1/simulation/robot/r1/pose",
            "v1/simulation/robot/pose",
        );
        assert_pubsub::<crate::api::simulation::v1::contact::Contact>(
            topic::new().v1().simulation().robot("r1").contact(),
            "v1/simulation/robot/r1/contact",
            "v1/simulation/robot/contact",
        );
        assert_pubsub::<crate::api::simulation::v1::collision::Collision>(
            topic::new().v1().simulation().robot("r1").collision(),
            "v1/simulation/robot/r1/collision",
            "v1/simulation/robot/collision",
        );
    }
}
