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
        $vis:vis enum $name:ident { $($variant:ident($inner:path)),+ $(,)? }
    ) => {
        // The wire version tag is the lowercased variant name (`V1` -> "v1"), so
        // a contract declares only its variants — no separate token to keep in
        // sync. Variants are append-only and never renamed (renaming one changes
        // the wire), per src/api/README.md.
        #[derive(Debug, Clone, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(tag = "v", content = "data", rename_all = "lowercase")]
        $(#[$attr])*
        $vis enum $name { $( $variant($inner), )+ }

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
mod tests;
