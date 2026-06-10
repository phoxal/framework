pub mod asset;
pub mod component;
pub mod drive;
pub mod simulation;

crate::bus::topic_tree! {
    pub mod topic;
    v1 {
        drive {
            pubsub target: crate::api::v1::drive::target::Target, v = 1;
            pubsub state: crate::api::v1::drive::state::State, v = 1;
        }
        component(id) {
            motor(id) {
                pubsub command: crate::api::v1::component::motor::Command, v = 1;
            }
            gnss(id) {
                pubsub data: crate::api::v1::component::gnss::Sample, v = 1;
            }
        }
        asset {
            query get: crate::api::v1::asset::get::Request => crate::api::v1::asset::get::Response, v = 1;
        }
        simulation {
            pubsub clock: crate::api::v1::simulation::clock::Clock, v = 1;
            robot(id) {
                pubsub pose: crate::api::v1::simulation::robot::pose::Pose, v = 1;
            }
        }
    }
}
