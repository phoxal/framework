pub const SCHEMA_NAME: &str = "phoxal-simulator-api/v1";
pub const SCHEMA_VERSION: u32 = 1;

pub mod clock;
pub mod collision;
pub mod contact;
pub mod pose;
pub mod reset;
pub mod status;

#[cfg(test)]
mod tests {
    use crate::bus::zenoh::TypedSchema;

    use super::{SCHEMA_NAME, SCHEMA_VERSION, clock, collision, contact, pose, reset, status};

    #[test]
    fn schema_contracts_do_not_drift() {
        assert_eq!(SCHEMA_NAME, "phoxal-simulator-api/v1");
        assert_eq!(SCHEMA_VERSION, 1);
        assert_eq!(clock::Clock::SCHEMA_NAME, "simulation/clock");
        assert_eq!(clock::Clock::SCHEMA_VERSION, 1);
        assert_eq!(
            collision::Collision::SCHEMA_NAME,
            "simulation/robot/collision"
        );
        assert_eq!(collision::Collision::SCHEMA_VERSION, 1);
        assert_eq!(contact::Contact::SCHEMA_NAME, "simulation/robot/contact");
        assert_eq!(contact::Contact::SCHEMA_VERSION, 1);
        assert_eq!(pose::Pose::SCHEMA_NAME, "simulation/robot/pose");
        assert_eq!(pose::Pose::SCHEMA_VERSION, 1);
        assert_eq!(reset::Request::SCHEMA_NAME, "simulation/reset/request");
        assert_eq!(reset::Request::SCHEMA_VERSION, 1);
        assert_eq!(reset::Response::SCHEMA_NAME, "simulation/reset/response");
        assert_eq!(reset::Response::SCHEMA_VERSION, 1);
        assert_eq!(status::Status::SCHEMA_NAME, "simulation/status");
        assert_eq!(status::Status::SCHEMA_VERSION, 1);
    }

    #[test]
    fn topic_paths_are_stable() {
        assert_eq!(clock::path(), "simulation/clock");
        assert_eq!(status::path(), "simulation/status");
        assert_eq!(pose::path("robot-1"), "simulation/robot/robot-1/pose");
        assert_eq!(
            collision::path("robot-1"),
            "simulation/robot/robot-1/collision"
        );
        assert_eq!(contact::path("robot-1"), "simulation/robot/robot-1/contact");
        assert_eq!(reset::path(), "simulation/reset");
    }
}

#[cfg(test)]
mod v1_version_tests {
    use super::{SCHEMA_NAME, SCHEMA_VERSION};

    #[test]
    fn api_contract_version_is_stable() {
        assert_eq!(SCHEMA_NAME, "phoxal-simulator-api/v1");
        assert_eq!(SCHEMA_VERSION, 1);
    }
}
