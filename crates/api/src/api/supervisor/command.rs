//! Acknowledged operations against supervisor authority.
//!
//! `phoxald` decides what a command does and whether it is allowed; this
//! fragment owns only the request and reply documents. The schema tags are
//! parse-time format discriminators owned by the documents themselves, so
//! renaming the endpoint does not rename a persisted tag.

pub use crate::api::supervisor::execution::{Command, CommandOutcome, CommandRejection};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "schema")]
pub enum Request {
    #[serde(rename = "phoxal/supervisor-control/request/v0")]
    V0 { command: Command },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "schema")]
pub enum Reply {
    #[serde(rename = "phoxal/supervisor-control/reply/v0")]
    V0 { outcome: CommandOutcome },
}

phoxal_macros::phoxal_api_fragment! {
    path supervisor / command;

    query self: Request => Reply;
}

#[cfg(test)]
mod tests {
    use super::{Command, CommandOutcome, Reply, Request};

    /// The documents carry their own format tag, independent of the endpoint
    /// key they travel on.
    #[test]
    fn command_documents_round_trip_with_explicit_schema_tags() {
        let request = Request::V0 {
            command: Command::Stop {
                expected_revision: 17,
            },
        };
        let encoded = rmp_serde::to_vec_named(&request).unwrap();
        assert_eq!(rmp_serde::from_slice::<Request>(&encoded).unwrap(), request);
        assert_eq!(
            serde_json::to_value(&request).unwrap()["schema"],
            "phoxal/supervisor-control/request/v0"
        );

        let reply = Reply::V0 {
            outcome: CommandOutcome::Accepted { at_revision: 18 },
        };
        let encoded = rmp_serde::to_vec_named(&reply).unwrap();
        assert_eq!(rmp_serde::from_slice::<Reply>(&encoded).unwrap(), reply);
        assert_eq!(
            serde_json::to_value(reply).unwrap()["schema"],
            "phoxal/supervisor-control/reply/v0"
        );
    }
}
