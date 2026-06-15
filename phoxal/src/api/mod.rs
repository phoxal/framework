//! Domain-first typed bus wire contracts.

use serde::Serialize;

/// Wire equality: two contracts are equal iff they serialize identically.
pub(crate) fn r#wire_eq<T: Serialize>(left: &T, right: &T) -> bool {
    matches!(
        (rmp_serde::to_vec_named(left), rmp_serde::to_vec_named(right)),
        (Ok(left), Ok(right)) if left == right
    )
}

/// Declares a domain-first versioned wire contract enum.
macro_rules! contract {
    (
        $(#[$attr:meta])*
        $vis:vis enum $name:ident { $($tok:literal => $variant:ident($inner:path)),+ $(,)? }
    ) => {
        #[derive(Debug, Clone, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(tag = "v", content = "data")]
        $(#[$attr])*
        $vis enum $name { $( #[serde(rename = $tok)] $variant($inner), )+ }

        impl ::core::cmp::PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                $crate::api::wire_eq(self, other)
            }
        }
    };
}

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

crate::topic_tree! {
    pub mod topic;
    drive {
        pubsub target: crate::api::drive::Target;
        pubsub state: crate::api::drive::State;
        pubsub actuator_commands: crate::api::drive::ActuatorCommands;
        pubsub saturation: crate::api::drive::Saturation;
        pubsub watchdog: crate::api::drive::Watchdog;
        pubsub kinematics: crate::api::drive::Kinematics;
    }
    odometry {
        pubsub estimate: crate::api::odometry::OdometryEstimate;
        pubsub status: crate::api::odometry::Status;
        pubsub source_health: crate::api::odometry::SourceHealth;
        pubsub residuals: crate::api::odometry::Residuals;
        pubsub integration: crate::api::odometry::Integration;
    }
    joint(id) {
        pubsub data: crate::api::joint::JointState;
    }
    frame {
        pubsub tree: crate::api::frame::Tree;
        pubsub r#static: crate::api::frame::Static;
        pubsub data: crate::api::frame::FrameTransform;
        query lookup: crate::api::frame::FrameLookupRequest => crate::api::frame::FrameLookupResponse;
    }
    power {
        pubsub command: crate::api::power::Command;
        pubsub state: crate::api::power::State;
    }
    presence {
        pubsub heartbeat: crate::api::presence::Heartbeat;
        pubsub summary: crate::api::presence::Summary;
        pubsub readiness: crate::api::presence::DebugReadiness;
    }
    motion {
        pubsub state: crate::api::motion::State;
        pubsub manual: crate::api::motion::ManualCommand;
        pubsub arbitration: crate::api::motion::Arbitration;
        pubsub source_freshness: crate::api::motion::SourceFreshness;
    }
    follow {
        pubsub target: crate::api::follow::Target;
        pubsub state: crate::api::follow::State;
        pubsub tracking_error: crate::api::follow::TrackingError;
        pubsub candidates: crate::api::follow::Candidates;
        pubsub costs: crate::api::follow::Costs;
        pubsub revision_inputs: crate::api::follow::RevisionInputs;
    }
    explore {
        pubsub frontiers: crate::api::explore::Frontiers;
        pubsub goal_candidates: crate::api::explore::GoalCandidates;
        pubsub state: crate::api::explore::State;
        pubsub scoring: crate::api::explore::Scoring;
        pubsub rejected_candidates: crate::api::explore::RejectedCandidates;
    }
    plan {
        pubsub path: crate::api::plan::Path;
        pubsub state: crate::api::plan::State;
        pubsub search_graph: crate::api::plan::SearchGraph;
        pubsub cost_layers: crate::api::plan::CostLayers;
        pubsub rejected_paths: crate::api::plan::RejectedPaths;
        pubsub revision_inputs: crate::api::plan::RevisionInputs;
    }
    perception {
        pubsub detections: crate::api::perception::Detections;
        pubsub state: crate::api::perception::PerceptionState;
    }
    safety {
        pubsub authorization: crate::api::safety::SafetyAuthorization;
        pubsub state: crate::api::safety::State;
        pubsub emergency_stop_request: crate::api::safety::EmergencyStopRequest;
        pubsub evidence: crate::api::safety::Evidence;
        pubsub stop_set: crate::api::safety::StopSet;
        pubsub latency_budget: crate::api::safety::LatencyBudget;
        pubsub source_health: crate::api::safety::SourceHealth;
    }
    mission {
        pubsub command: crate::api::mission::MissionCommand;
        pubsub state: crate::api::mission::State;
        pubsub goal: crate::api::mission::Goal;
        pubsub decision_trace: crate::api::mission::DecisionTrace;
        debug {
            pubsub goal_record: crate::api::mission::Goal;
        }
    }
    video {
        query open: crate::api::video::OpenRequest => crate::api::video::OpenResponse;
        stream(id) {
            pubsub event: crate::api::video::StreamEvent;
        }
    }
    component(id) {
        motor(id) {
            pubsub command: crate::api::component::capability::motor::Command;
        }
        encoder(id) {
            pubsub data: crate::api::component::capability::encoder::Sample;
        }
        accelerometer(id) {
            pubsub data: crate::api::component::capability::accelerometer::Sample;
        }
        gyroscope(id) {
            pubsub data: crate::api::component::capability::gyroscope::Sample;
        }
        magnetometer(id) {
            pubsub data: crate::api::component::capability::magnetometer::Sample;
        }
        imu(id) {
            pubsub data: crate::api::component::capability::imu::Sample;
        }
        gnss(id) {
            pubsub data: crate::api::component::capability::gnss::Sample;
        }
        camera(id) {
            pubsub data: crate::api::component::capability::camera::Frame;
            profile(id) {
                pubsub data: crate::api::component::capability::camera::Frame;
            }
        }
        depth(id) {
            pubsub data: crate::api::component::capability::depth::Depth;
            profile(id) {
                pubsub data: crate::api::component::capability::depth::Depth;
            }
        }
        range(id) {
            pubsub data: crate::api::component::capability::range::Sample;
        }
        lidar(id) {
            pubsub data: crate::api::component::capability::lidar::Scan;
        }
        mmwave(id) {
            pubsub data: crate::api::component::capability::mmwave::Scan;
        }
        emergency_stop(id) {
            pubsub data: crate::api::component::capability::emergency_stop::State;
        }
        battery(id) {
            pubsub data: crate::api::component::capability::battery::State;
        }
        led(id) {
            pubsub command: crate::api::component::capability::led::Command;
        }
        microphone(id) {
            pubsub data: crate::api::component::capability::microphone::Frame;
        }
        speaker(id) {
            pubsub audio: crate::api::component::capability::speaker::audio::Audio;
            pubsub command: crate::api::component::capability::speaker::command::Command;
        }
    }
    asset {
        query get: crate::api::asset::GetRequest => crate::api::asset::GetResponse;
    }
    simulation {
        pubsub clock: crate::api::simulation::clock::Clock;
        pubsub status: crate::api::simulation::status::Status;
        query reset: crate::api::simulation::reset::Request => crate::api::simulation::reset::Response;
        robot(id) {
            pubsub pose: crate::api::simulation::pose::Pose;
            pubsub contact: crate::api::simulation::contact::Contact;
            pubsub collision: crate::api::simulation::collision::Collision;
        }
    }
    localize {
        pubsub state: crate::api::localize::LocalizationState;
        pubsub pose: crate::api::localize::PoseEstimate;
        pubsub revision: crate::api::localize::LocalizationRevision;
        pubsub keyframe: crate::api::localize::Keyframe;
        pubsub correction: crate::api::localize::PoseGraphCorrection;
        query pose_graph: crate::api::localize::PoseGraphRequest => crate::api::localize::PoseGraphResponse;
        query keyframe_query: crate::api::localize::KeyframeRequest => crate::api::localize::KeyframeResponse;
        query corrections: crate::api::localize::CorrectionsRequest => crate::api::localize::CorrectionsResponse;
    }
    map {
        pubsub revision: crate::api::map::MapRevision;
        pubsub summary: crate::api::map::Summary;
        pubsub local_cost: crate::api::map::LocalCost;
        pubsub traversability: crate::api::map::Traversability;
        pubsub traversability_summary: crate::api::map::TraversabilitySummary;
        query submap: crate::api::map::SubmapRequest => crate::api::map::SubmapResponse;
        query esdf_tile: crate::api::map::EsdfTileRequest => crate::api::map::EsdfTileResponse;
        query traversability_tile: crate::api::map::TraversabilityTileRequest => crate::api::map::TraversabilityTileResponse;
        query local_grid: crate::api::map::LocalGridRequest => crate::api::map::LocalGridResponse;
        query global_grid: crate::api::map::GlobalGridRequest => crate::api::map::GlobalGridResponse;
        query snapshot: crate::api::map::SnapshotRequest => crate::api::map::SnapshotResponse;
    }
}

#[cfg(test)]
mod tests {
    use crate::bus::topic::{PubSub, Query, Topic};

    use super::topic;

    fn assert_pubsub<T>(topic: Topic<PubSub<T>>, key: &str, schema: &str) {
        assert_eq!(topic.key(), key);
        assert_eq!(topic.schema(), schema);
    }

    fn assert_query<Req, Resp>(topic: Topic<Query<Req, Resp>>, key: &str, schema: &str) {
        assert_eq!(topic.key(), key);
        assert_eq!(topic.schema(), schema);
    }

    #[test]
    fn topic_tree_keys_and_schemas_cover_api_domains() {
        assert_pubsub::<crate::api::drive::Target>(
            topic::new().drive().target(),
            "drive/target",
            "drive/target",
        );
        assert_pubsub::<crate::api::drive::State>(
            topic::new().drive().state(),
            "drive/state",
            "drive/state",
        );
        assert_pubsub::<crate::api::drive::ActuatorCommands>(
            topic::new().drive().actuator_commands(),
            "drive/actuator_commands",
            "drive/actuator_commands",
        );
        assert_pubsub::<crate::api::drive::Saturation>(
            topic::new().drive().saturation(),
            "drive/saturation",
            "drive/saturation",
        );
        assert_pubsub::<crate::api::drive::Watchdog>(
            topic::new().drive().watchdog(),
            "drive/watchdog",
            "drive/watchdog",
        );
        assert_pubsub::<crate::api::drive::Kinematics>(
            topic::new().drive().kinematics(),
            "drive/kinematics",
            "drive/kinematics",
        );
        assert_pubsub::<crate::api::odometry::OdometryEstimate>(
            topic::new().odometry().estimate(),
            "odometry/estimate",
            "odometry/estimate",
        );
        assert_pubsub::<crate::api::odometry::Status>(
            topic::new().odometry().status(),
            "odometry/status",
            "odometry/status",
        );
        assert_pubsub::<crate::api::odometry::SourceHealth>(
            topic::new().odometry().source_health(),
            "odometry/source_health",
            "odometry/source_health",
        );
        assert_pubsub::<crate::api::odometry::Residuals>(
            topic::new().odometry().residuals(),
            "odometry/residuals",
            "odometry/residuals",
        );
        assert_pubsub::<crate::api::odometry::Integration>(
            topic::new().odometry().integration(),
            "odometry/integration",
            "odometry/integration",
        );
        assert_pubsub::<crate::api::joint::JointState>(
            topic::new().joint("left_wheel").data(),
            "joint/left_wheel/data",
            "joint/data",
        );
        assert_pubsub::<crate::api::frame::Tree>(
            topic::new().frame().tree(),
            "frame/tree",
            "frame/tree",
        );
        assert_pubsub::<crate::api::frame::Static>(
            topic::new().frame().r#static(),
            "frame/static",
            "frame/static",
        );
        assert_pubsub::<crate::api::frame::FrameTransform>(
            topic::new().frame().data(),
            "frame/data",
            "frame/data",
        );
        assert_pubsub::<crate::api::power::Command>(
            topic::new().power().command(),
            "power/command",
            "power/command",
        );
        assert_pubsub::<crate::api::power::State>(
            topic::new().power().state(),
            "power/state",
            "power/state",
        );
        assert_pubsub::<crate::api::presence::Heartbeat>(
            topic::new().presence().heartbeat(),
            "presence/heartbeat",
            "presence/heartbeat",
        );
        assert_pubsub::<crate::api::presence::Summary>(
            topic::new().presence().summary(),
            "presence/summary",
            "presence/summary",
        );
        assert_pubsub::<crate::api::presence::DebugReadiness>(
            topic::new().presence().readiness(),
            "presence/readiness",
            "presence/readiness",
        );
        assert_pubsub::<crate::api::motion::State>(
            topic::new().motion().state(),
            "motion/state",
            "motion/state",
        );
        assert_pubsub::<crate::api::motion::ManualCommand>(
            topic::new().motion().manual(),
            "motion/manual",
            "motion/manual",
        );
        assert_pubsub::<crate::api::motion::Arbitration>(
            topic::new().motion().arbitration(),
            "motion/arbitration",
            "motion/arbitration",
        );
        assert_pubsub::<crate::api::motion::SourceFreshness>(
            topic::new().motion().source_freshness(),
            "motion/source_freshness",
            "motion/source_freshness",
        );
        assert_pubsub::<crate::api::follow::Target>(
            topic::new().follow().target(),
            "follow/target",
            "follow/target",
        );
        assert_pubsub::<crate::api::follow::State>(
            topic::new().follow().state(),
            "follow/state",
            "follow/state",
        );
        assert_pubsub::<crate::api::follow::TrackingError>(
            topic::new().follow().tracking_error(),
            "follow/tracking_error",
            "follow/tracking_error",
        );
        assert_pubsub::<crate::api::follow::Candidates>(
            topic::new().follow().candidates(),
            "follow/candidates",
            "follow/candidates",
        );
        assert_pubsub::<crate::api::follow::Costs>(
            topic::new().follow().costs(),
            "follow/costs",
            "follow/costs",
        );
        assert_pubsub::<crate::api::follow::RevisionInputs>(
            topic::new().follow().revision_inputs(),
            "follow/revision_inputs",
            "follow/revision_inputs",
        );
        assert_pubsub::<crate::api::explore::Frontiers>(
            topic::new().explore().frontiers(),
            "explore/frontiers",
            "explore/frontiers",
        );
        assert_pubsub::<crate::api::explore::GoalCandidates>(
            topic::new().explore().goal_candidates(),
            "explore/goal_candidates",
            "explore/goal_candidates",
        );
        assert_pubsub::<crate::api::explore::State>(
            topic::new().explore().state(),
            "explore/state",
            "explore/state",
        );
        assert_pubsub::<crate::api::explore::Scoring>(
            topic::new().explore().scoring(),
            "explore/scoring",
            "explore/scoring",
        );
        assert_pubsub::<crate::api::explore::RejectedCandidates>(
            topic::new().explore().rejected_candidates(),
            "explore/rejected_candidates",
            "explore/rejected_candidates",
        );
        assert_pubsub::<crate::api::plan::Path>(
            topic::new().plan().path(),
            "plan/path",
            "plan/path",
        );
        assert_pubsub::<crate::api::plan::State>(
            topic::new().plan().state(),
            "plan/state",
            "plan/state",
        );
        assert_pubsub::<crate::api::plan::SearchGraph>(
            topic::new().plan().search_graph(),
            "plan/search_graph",
            "plan/search_graph",
        );
        assert_pubsub::<crate::api::plan::CostLayers>(
            topic::new().plan().cost_layers(),
            "plan/cost_layers",
            "plan/cost_layers",
        );
        assert_pubsub::<crate::api::plan::RejectedPaths>(
            topic::new().plan().rejected_paths(),
            "plan/rejected_paths",
            "plan/rejected_paths",
        );
        assert_pubsub::<crate::api::plan::RevisionInputs>(
            topic::new().plan().revision_inputs(),
            "plan/revision_inputs",
            "plan/revision_inputs",
        );
        assert_pubsub::<crate::api::perception::Detections>(
            topic::new().perception().detections(),
            "perception/detections",
            "perception/detections",
        );
        assert_pubsub::<crate::api::perception::PerceptionState>(
            topic::new().perception().state(),
            "perception/state",
            "perception/state",
        );
        assert_pubsub::<crate::api::safety::SafetyAuthorization>(
            topic::new().safety().authorization(),
            "safety/authorization",
            "safety/authorization",
        );
        assert_pubsub::<crate::api::safety::State>(
            topic::new().safety().state(),
            "safety/state",
            "safety/state",
        );
        assert_pubsub::<crate::api::safety::EmergencyStopRequest>(
            topic::new().safety().emergency_stop_request(),
            "safety/emergency_stop_request",
            "safety/emergency_stop_request",
        );
        assert_pubsub::<crate::api::safety::Evidence>(
            topic::new().safety().evidence(),
            "safety/evidence",
            "safety/evidence",
        );
        assert_pubsub::<crate::api::safety::StopSet>(
            topic::new().safety().stop_set(),
            "safety/stop_set",
            "safety/stop_set",
        );
        assert_pubsub::<crate::api::safety::LatencyBudget>(
            topic::new().safety().latency_budget(),
            "safety/latency_budget",
            "safety/latency_budget",
        );
        assert_pubsub::<crate::api::safety::SourceHealth>(
            topic::new().safety().source_health(),
            "safety/source_health",
            "safety/source_health",
        );
        assert_pubsub::<crate::api::mission::MissionCommand>(
            topic::new().mission().command(),
            "mission/command",
            "mission/command",
        );
        assert_pubsub::<crate::api::mission::State>(
            topic::new().mission().state(),
            "mission/state",
            "mission/state",
        );
        assert_pubsub::<crate::api::mission::Goal>(
            topic::new().mission().goal(),
            "mission/goal",
            "mission/goal",
        );
        assert_pubsub::<crate::api::mission::DecisionTrace>(
            topic::new().mission().decision_trace(),
            "mission/decision_trace",
            "mission/decision_trace",
        );
        assert_pubsub::<crate::api::mission::Goal>(
            topic::new().mission().debug().goal_record(),
            "mission/debug/goal_record",
            "mission/debug/goal_record",
        );
        assert_pubsub::<crate::api::video::StreamEvent>(
            topic::new().video().stream("front").event(),
            "video/stream/front/event",
            "video/stream/event",
        );
        assert_query::<crate::api::asset::GetRequest, crate::api::asset::GetResponse>(
            topic::new().asset().get(),
            "asset/get",
            "asset/get",
        );
        assert_pubsub::<crate::api::simulation::clock::Clock>(
            topic::new().simulation().clock(),
            "simulation/clock",
            "simulation/clock",
        );
        assert_pubsub::<crate::api::simulation::status::Status>(
            topic::new().simulation().status(),
            "simulation/status",
            "simulation/status",
        );
        assert_pubsub::<crate::api::localize::LocalizationState>(
            topic::new().localize().state(),
            "localize/state",
            "localize/state",
        );
        assert_pubsub::<crate::api::localize::PoseEstimate>(
            topic::new().localize().pose(),
            "localize/pose",
            "localize/pose",
        );
        assert_pubsub::<crate::api::localize::LocalizationRevision>(
            topic::new().localize().revision(),
            "localize/revision",
            "localize/revision",
        );
        assert_pubsub::<crate::api::localize::Keyframe>(
            topic::new().localize().keyframe(),
            "localize/keyframe",
            "localize/keyframe",
        );
        assert_pubsub::<crate::api::localize::PoseGraphCorrection>(
            topic::new().localize().correction(),
            "localize/correction",
            "localize/correction",
        );
        assert_pubsub::<crate::api::map::MapRevision>(
            topic::new().map().revision(),
            "map/revision",
            "map/revision",
        );
        assert_pubsub::<crate::api::map::Summary>(
            topic::new().map().summary(),
            "map/summary",
            "map/summary",
        );
        assert_pubsub::<crate::api::map::LocalCost>(
            topic::new().map().local_cost(),
            "map/local_cost",
            "map/local_cost",
        );
        assert_pubsub::<crate::api::map::Traversability>(
            topic::new().map().traversability(),
            "map/traversability",
            "map/traversability",
        );
        assert_pubsub::<crate::api::map::TraversabilitySummary>(
            topic::new().map().traversability_summary(),
            "map/traversability_summary",
            "map/traversability_summary",
        );
    }

    #[test]
    fn topic_tree_query_leaves_omit_query_path_segment() {
        assert_query::<crate::api::frame::FrameLookupRequest, crate::api::frame::FrameLookupResponse>(
            topic::new().frame().lookup(),
            "frame/lookup",
            "frame/lookup",
        );
        assert_query::<crate::api::video::OpenRequest, crate::api::video::OpenResponse>(
            topic::new().video().open(),
            "video/open",
            "video/open",
        );
        assert_query::<crate::api::asset::GetRequest, crate::api::asset::GetResponse>(
            topic::new().asset().get(),
            "asset/get",
            "asset/get",
        );
        assert_query::<
            crate::api::localize::PoseGraphRequest,
            crate::api::localize::PoseGraphResponse,
        >(
            topic::new().localize().pose_graph(),
            "localize/pose_graph",
            "localize/pose_graph",
        );
        assert_query::<crate::api::localize::KeyframeRequest, crate::api::localize::KeyframeResponse>(
            topic::new().localize().keyframe_query(),
            "localize/keyframe_query",
            "localize/keyframe_query",
        );
        assert_query::<
            crate::api::localize::CorrectionsRequest,
            crate::api::localize::CorrectionsResponse,
        >(
            topic::new().localize().corrections(),
            "localize/corrections",
            "localize/corrections",
        );
        assert_query::<
            crate::api::map::TraversabilityTileRequest,
            crate::api::map::TraversabilityTileResponse,
        >(
            topic::new().map().traversability_tile(),
            "map/traversability_tile",
            "map/traversability_tile",
        );
        assert_query::<crate::api::map::SubmapRequest, crate::api::map::SubmapResponse>(
            topic::new().map().submap(),
            "map/submap",
            "map/submap",
        );
        assert_query::<crate::api::map::EsdfTileRequest, crate::api::map::EsdfTileResponse>(
            topic::new().map().esdf_tile(),
            "map/esdf_tile",
            "map/esdf_tile",
        );
        assert_query::<crate::api::map::LocalGridRequest, crate::api::map::LocalGridResponse>(
            topic::new().map().local_grid(),
            "map/local_grid",
            "map/local_grid",
        );
        assert_query::<crate::api::map::GlobalGridRequest, crate::api::map::GlobalGridResponse>(
            topic::new().map().global_grid(),
            "map/global_grid",
            "map/global_grid",
        );
        assert_query::<crate::api::map::SnapshotRequest, crate::api::map::SnapshotResponse>(
            topic::new().map().snapshot(),
            "map/snapshot",
            "map/snapshot",
        );
        assert_query::<
            crate::api::simulation::reset::Request,
            crate::api::simulation::reset::Response,
        >(
            topic::new().simulation().reset(),
            "simulation/reset",
            "simulation/reset",
        );
    }

    #[test]
    fn topic_tree_component_and_simulation_slots_elide_holes_in_schemas() {
        assert_pubsub::<crate::api::component::capability::motor::Command>(
            topic::new().component("base").motor("left_wheel").command(),
            "component/base/motor/left_wheel/command",
            "component/motor/command",
        );
        assert_pubsub::<crate::api::component::capability::encoder::Sample>(
            topic::new().component("base").encoder("left_wheel").data(),
            "component/base/encoder/left_wheel/data",
            "component/encoder/data",
        );
        assert_pubsub::<crate::api::component::capability::accelerometer::Sample>(
            topic::new()
                .component("imu_board")
                .accelerometer("accel")
                .data(),
            "component/imu_board/accelerometer/accel/data",
            "component/accelerometer/data",
        );
        assert_pubsub::<crate::api::component::capability::gyroscope::Sample>(
            topic::new().component("imu_board").gyroscope("gyro").data(),
            "component/imu_board/gyroscope/gyro/data",
            "component/gyroscope/data",
        );
        assert_pubsub::<crate::api::component::capability::magnetometer::Sample>(
            topic::new()
                .component("imu_board")
                .magnetometer("mag")
                .data(),
            "component/imu_board/magnetometer/mag/data",
            "component/magnetometer/data",
        );
        assert_pubsub::<crate::api::component::capability::imu::Sample>(
            topic::new().component("imu_board").imu("imu").data(),
            "component/imu_board/imu/imu/data",
            "component/imu/data",
        );
        assert_pubsub::<crate::api::component::capability::gnss::Sample>(
            topic::new().component("gps").gnss("gnss").data(),
            "component/gps/gnss/gnss/data",
            "component/gnss/data",
        );
        assert_pubsub::<crate::api::component::capability::camera::Frame>(
            topic::new().component("head").camera("front").data(),
            "component/head/camera/front/data",
            "component/camera/data",
        );
        assert_pubsub::<crate::api::component::capability::camera::Frame>(
            topic::new()
                .component("head")
                .camera("front")
                .profile("r640x480_h10_rgb8")
                .data(),
            "component/head/camera/front/profile/r640x480_h10_rgb8/data",
            "component/camera/profile/data",
        );
        assert_pubsub::<crate::api::component::capability::depth::Depth>(
            topic::new().component("head").depth("front_depth").data(),
            "component/head/depth/front_depth/data",
            "component/depth/data",
        );
        assert_pubsub::<crate::api::component::capability::depth::Depth>(
            topic::new()
                .component("head")
                .depth("front_depth")
                .profile("r320x240_h5_depth_mm")
                .data(),
            "component/head/depth/front_depth/profile/r320x240_h5_depth_mm/data",
            "component/depth/profile/data",
        );
        assert_pubsub::<crate::api::component::capability::range::Sample>(
            topic::new().component("base").range("front_tof").data(),
            "component/base/range/front_tof/data",
            "component/range/data",
        );
        assert_pubsub::<crate::api::component::capability::lidar::Scan>(
            topic::new().component("front_lidar").lidar("scan").data(),
            "component/front_lidar/lidar/scan/data",
            "component/lidar/data",
        );
        assert_pubsub::<crate::api::component::capability::mmwave::Scan>(
            topic::new().component("radar").mmwave("mmwave").data(),
            "component/radar/mmwave/mmwave/data",
            "component/mmwave/data",
        );
        assert_pubsub::<crate::api::component::capability::emergency_stop::State>(
            topic::new()
                .component("safety_panel")
                .emergency_stop("estop")
                .data(),
            "component/safety_panel/emergency_stop/estop/data",
            "component/emergency_stop/data",
        );
        assert_pubsub::<crate::api::component::capability::battery::State>(
            topic::new()
                .component("power_board")
                .battery("main_battery")
                .data(),
            "component/power_board/battery/main_battery/data",
            "component/battery/data",
        );
        assert_pubsub::<crate::api::component::capability::led::Command>(
            topic::new()
                .component("status_panel")
                .led("status")
                .command(),
            "component/status_panel/led/status/command",
            "component/led/command",
        );
        assert_pubsub::<crate::api::component::capability::microphone::Frame>(
            topic::new().component("head").microphone("mic").data(),
            "component/head/microphone/mic/data",
            "component/microphone/data",
        );
        assert_pubsub::<crate::api::component::capability::speaker::audio::Audio>(
            topic::new().component("head").speaker("speaker").audio(),
            "component/head/speaker/speaker/audio",
            "component/speaker/audio",
        );
        assert_pubsub::<crate::api::component::capability::speaker::command::Command>(
            topic::new().component("head").speaker("speaker").command(),
            "component/head/speaker/speaker/command",
            "component/speaker/command",
        );
        assert_pubsub::<crate::api::simulation::pose::Pose>(
            topic::new().simulation().robot("r1").pose(),
            "simulation/robot/r1/pose",
            "simulation/robot/pose",
        );
        assert_pubsub::<crate::api::simulation::contact::Contact>(
            topic::new().simulation().robot("r1").contact(),
            "simulation/robot/r1/contact",
            "simulation/robot/contact",
        );
        assert_pubsub::<crate::api::simulation::collision::Collision>(
            topic::new().simulation().robot("r1").collision(),
            "simulation/robot/r1/collision",
            "simulation/robot/collision",
        );
    }
}
