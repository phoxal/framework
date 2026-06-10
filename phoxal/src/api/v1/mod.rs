crate::bus::topic_tree! {
    pub mod topic;
    v1 {
        drive {
            pubsub target: crate::api::drive::v1::Target, v = 1;
            pubsub state: crate::api::drive::v1::State, v = 1;
            pubsub actuator_commands: crate::api::drive::v1::ActuatorCommands, v = 1;
            pubsub saturation: crate::api::drive::v1::Saturation, v = 1;
            pubsub watchdog: crate::api::drive::v1::Watchdog, v = 1;
            pubsub kinematics: crate::api::drive::v1::Kinematics, v = 1;
        }
        odometry {
            pubsub estimate: crate::api::odometry::v1::OdometryEstimate, v = 1;
            pubsub status: crate::api::odometry::v1::Status, v = 1;
            pubsub source_health: crate::api::odometry::v1::SourceHealth, v = 1;
            pubsub residuals: crate::api::odometry::v1::Residuals, v = 1;
            pubsub integration: crate::api::odometry::v1::Integration, v = 1;
        }
        joint(id) {
            pubsub data: crate::api::joint::v1::JointState, v = 1;
        }
        frame {
            pubsub tree: crate::api::frame::v1::Tree, v = 1;
            pubsub r#static: crate::api::frame::v1::Static, v = 1;
            pubsub data: crate::api::frame::v1::FrameTransform, v = 1;
            query lookup: crate::api::frame::v1::FrameLookupRequest => crate::api::frame::v1::FrameLookupResponse, v = 1;
        }
        power {
            pubsub command: crate::api::power::v1::Command, v = 1;
            pubsub state: crate::api::power::v1::State, v = 1;
        }
        presence {
            pubsub heartbeat: crate::api::presence::Heartbeat, v = 1;
            pubsub summary: crate::api::presence::Summary, v = 1;
            pubsub readiness: crate::api::presence::DebugReadiness, v = 1;
        }
        motion {
            pubsub state: crate::api::motion::v1::State, v = 1;
            pubsub manual: crate::api::motion::v1::ManualCommand, v = 1;
            pubsub arbitration: crate::api::motion::v1::Arbitration, v = 1;
            pubsub source_freshness: crate::api::motion::v1::SourceFreshness, v = 1;
        }
        follow {
            pubsub target: crate::api::follow::v1::Target, v = 1;
            pubsub state: crate::api::follow::v1::State, v = 1;
            pubsub tracking_error: crate::api::follow::v1::TrackingError, v = 1;
            pubsub candidates: crate::api::follow::v1::Candidates, v = 1;
            pubsub costs: crate::api::follow::v1::Costs, v = 1;
            pubsub revision_inputs: crate::api::follow::v1::RevisionInputs, v = 1;
        }
        explore {
            pubsub frontiers: crate::api::explore::v1::Frontiers, v = 1;
            pubsub goal_candidates: crate::api::explore::v1::GoalCandidates, v = 1;
            pubsub state: crate::api::explore::v1::State, v = 1;
            pubsub scoring: crate::api::explore::v1::Scoring, v = 1;
            pubsub rejected_candidates: crate::api::explore::v1::RejectedCandidates, v = 1;
        }
        plan {
            pubsub path: crate::api::plan::v1::Path, v = 1;
            pubsub state: crate::api::plan::v1::State, v = 1;
            pubsub search_graph: crate::api::plan::v1::SearchGraph, v = 1;
            pubsub cost_layers: crate::api::plan::v1::CostLayers, v = 1;
            pubsub rejected_paths: crate::api::plan::v1::RejectedPaths, v = 1;
            pubsub revision_inputs: crate::api::plan::v1::RevisionInputs, v = 1;
        }
        perception {
            pubsub detections: crate::api::perception::v1::Detections, v = 1;
            pubsub state: crate::api::perception::v1::PerceptionState, v = 1;
        }
        safety {
            pubsub authorization: crate::api::safety::v1::SafetyAuthorization, v = 1;
            pubsub state: crate::api::safety::v1::State, v = 1;
            pubsub emergency_stop_request: crate::api::safety::v1::EmergencyStopRequest, v = 1;
            pubsub evidence: crate::api::safety::v1::Evidence, v = 1;
            pubsub stop_set: crate::api::safety::v1::StopSet, v = 1;
            pubsub latency_budget: crate::api::safety::v1::LatencyBudget, v = 1;
            pubsub source_health: crate::api::safety::v1::SourceHealth, v = 1;
        }
        mission {
            pubsub command: crate::api::mission::v1::MissionCommand, v = 1;
            pubsub state: crate::api::mission::v1::State, v = 1;
            pubsub goal: crate::api::mission::v1::Goal, v = 1;
            pubsub decision_trace: crate::api::mission::v1::DecisionTrace, v = 1;
        }
        video {
            query open: crate::api::video::v1::OpenRequest => crate::api::video::v1::OpenResponse, v = 1;
            stream(id) {
                pubsub event: crate::api::video::v1::StreamEvent, v = 1;
            }
        }
        component(id) {
            motor(id) {
                pubsub command: crate::api::component::v1::capability::motor::Command, v = 1;
            }
            encoder(id) {
                pubsub data: crate::api::component::v1::capability::encoder::Sample, v = 1;
            }
            accelerometer(id) {
                pubsub data: crate::api::component::v1::capability::accelerometer::Sample, v = 1;
            }
            gyroscope(id) {
                pubsub data: crate::api::component::v1::capability::gyroscope::Sample, v = 1;
            }
            magnetometer(id) {
                pubsub data: crate::api::component::v1::capability::magnetometer::Sample, v = 1;
            }
            imu(id) {
                pubsub data: crate::api::component::v1::capability::imu::Sample, v = 1;
            }
            gnss(id) {
                pubsub data: crate::api::component::v1::capability::gnss::Sample, v = 1;
            }
            camera(id) {
                pubsub data: crate::api::component::v1::capability::camera::Frame, v = 1;
            }
            depth(id) {
                pubsub data: crate::api::component::v1::capability::depth::Depth, v = 1;
            }
            range(id) {
                pubsub data: crate::api::component::v1::capability::range::Sample, v = 1;
            }
            lidar(id) {
                pubsub data: crate::api::component::v1::capability::lidar::Scan, v = 1;
            }
            mmwave(id) {
                pubsub data: crate::api::component::v1::capability::mmwave::Scan, v = 1;
            }
            emergency_stop(id) {
                pubsub data: crate::api::component::v1::capability::emergency_stop::State, v = 1;
            }
            battery(id) {
                pubsub data: crate::api::component::v1::capability::battery::State, v = 1;
            }
        }
        asset {
            query get: crate::api::asset::v1::GetRequest => crate::api::asset::v1::GetResponse, v = 1;
        }
        simulation {
            pubsub clock: crate::api::simulation::v1::clock::Clock, v = 1;
            pubsub status: crate::api::simulation::v1::status::Status, v = 1;
            query reset: crate::api::simulation::v1::reset::Request => crate::api::simulation::v1::reset::Response, v = 1;
            robot(id) {
                pubsub pose: crate::api::simulation::v1::pose::Pose, v = 1;
                pubsub contact: crate::api::simulation::v1::contact::Contact, v = 1;
                pubsub collision: crate::api::simulation::v1::collision::Collision, v = 1;
            }
        }
        localize {
            pubsub state: crate::api::localize::v1::LocalizationState, v = 1;
            pubsub pose: crate::api::localize::v1::PoseEstimate, v = 1;
            pubsub revision: crate::api::localize::v1::LocalizationRevision, v = 1;
            pubsub keyframe: crate::api::localize::v1::Keyframe, v = 1;
            pubsub correction: crate::api::localize::v1::PoseGraphCorrection, v = 1;
            query pose_graph: crate::api::localize::v1::PoseGraphRequest => crate::api::localize::v1::PoseGraphResponse, v = 1;
            query keyframe_query: crate::api::localize::v1::KeyframeRequest => crate::api::localize::v1::KeyframeResponse, v = 1;
            query corrections: crate::api::localize::v1::CorrectionsRequest => crate::api::localize::v1::CorrectionsResponse, v = 1;
        }
        map {
            pubsub revision: crate::api::map::v1::MapRevision, v = 1;
            pubsub summary: crate::api::map::v1::Summary, v = 1;
            pubsub local_cost: crate::api::map::v1::LocalCost, v = 1;
            pubsub traversability: crate::api::map::v1::Traversability, v = 1;
            pubsub traversability_summary: crate::api::map::v1::TraversabilitySummary, v = 1;
            query submap: crate::api::map::v1::SubmapRequest => crate::api::map::v1::SubmapResponse, v = 1;
            query esdf_tile: crate::api::map::v1::EsdfTileRequest => crate::api::map::v1::EsdfTileResponse, v = 1;
            query traversability_tile: crate::api::map::v1::TraversabilityTileRequest => crate::api::map::v1::TraversabilityTileResponse, v = 1;
            query local_grid: crate::api::map::v1::LocalGridRequest => crate::api::map::v1::LocalGridResponse, v = 1;
            query global_grid: crate::api::map::v1::GlobalGridRequest => crate::api::map::v1::GlobalGridResponse, v = 1;
            query snapshot: crate::api::map::v1::SnapshotRequest => crate::api::map::v1::SnapshotResponse, v = 1;
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
        assert_pubsub::<crate::api::odometry::v1::OdometryEstimate>(
            topic::new().v1().odometry().estimate(),
            "v1/odometry/estimate",
            "v1/odometry/estimate",
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
        assert_pubsub::<crate::api::motion::v1::ManualCommand>(
            topic::new().v1().motion().manual(),
            "v1/motion/manual",
            "v1/motion/manual",
        );
        assert_pubsub::<crate::api::follow::v1::Target>(
            topic::new().v1().follow().target(),
            "v1/follow/target",
            "v1/follow/target",
        );
        assert_pubsub::<crate::api::explore::v1::GoalCandidates>(
            topic::new().v1().explore().goal_candidates(),
            "v1/explore/goal_candidates",
            "v1/explore/goal_candidates",
        );
        assert_pubsub::<crate::api::plan::v1::Path>(
            topic::new().v1().plan().path(),
            "v1/plan/path",
            "v1/plan/path",
        );
        assert_pubsub::<crate::api::perception::v1::Detections>(
            topic::new().v1().perception().detections(),
            "v1/perception/detections",
            "v1/perception/detections",
        );
        assert_pubsub::<crate::api::safety::v1::SafetyAuthorization>(
            topic::new().v1().safety().authorization(),
            "v1/safety/authorization",
            "v1/safety/authorization",
        );
        assert_pubsub::<crate::api::mission::v1::MissionCommand>(
            topic::new().v1().mission().command(),
            "v1/mission/command",
            "v1/mission/command",
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
        assert_pubsub::<crate::api::localize::v1::LocalizationState>(
            topic::new().v1().localize().state(),
            "v1/localize/state",
            "v1/localize/state",
        );
        assert_query::<crate::api::map::v1::SubmapRequest, crate::api::map::v1::SubmapResponse>(
            topic::new().v1().map().submap(),
            "v1/map/submap",
            "v1/map/submap",
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
        assert_pubsub::<crate::api::component::v1::capability::camera::Frame>(
            topic::new().v1().component("head").camera("front").data(),
            "v1/component/head/camera/front/data",
            "v1/component/camera/data",
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
        assert_pubsub::<crate::api::component::v1::capability::range::Sample>(
            topic::new()
                .v1()
                .component("base")
                .range("front_tof")
                .data(),
            "v1/component/base/range/front_tof/data",
            "v1/component/range/data",
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
