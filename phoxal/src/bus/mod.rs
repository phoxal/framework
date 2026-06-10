#![allow(clippy::module_name_repetitions)]

use derive_new::new;
use thiserror::Error;

pub mod builder;
pub mod liveliness;
pub mod metadata;
pub mod query;
pub mod topic;
pub mod typed;
#[allow(clippy::module_name_repetitions)]
pub mod zenoh;

pub use topic::topic_tree;

#[derive(Clone, new)]
pub struct Bus {
    session: ::zenoh::Session,
    prefix: String,
}

impl Bus {
    pub fn builder(router: impl Into<String>) -> builder::Builder {
        builder::Builder::new(router)
    }

    pub async fn close(&self) -> Result<()> {
        self.session.close().await?;
        Ok(())
    }

    pub fn topic(&self, suffix: &str) -> String {
        if self.prefix.is_empty() {
            suffix.to_string()
        } else {
            format!("{}/{suffix}", self.prefix)
        }
    }

    pub fn session(&self) -> &::zenoh::Session {
        &self.session
    }

    pub fn cloned_session(&self) -> ::zenoh::Session {
        self.session.clone()
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to encode bus payload: {0}")]
    Encode(#[from] rmp_serde::encode::Error),

    #[error("failed to decode bus payload: {0}")]
    Decode(#[from] rmp_serde::decode::Error),

    #[error("invalid bus identifier: {0}")]
    InvalidIdentifier(String),

    #[error("invalid bus topic: {0}")]
    InvalidTopic(String),

    #[error("invalid query retry: {0}")]
    InvalidRetry(String),

    #[error("bus transport error: {0}")]
    Transport(#[from] ::zenoh::Error),

    #[error("typed bus decode error: {0}")]
    TypedDecode(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::bus::builder::Builder;

    #[test]
    fn builder_keeps_router_endpoint_as_given() {
        assert_eq!(
            Builder::new("tcp/127.0.0.1:7447")
                .with_prefix("dev")
                .router(),
            "tcp/127.0.0.1:7447"
        );
    }

    #[test]
    fn builder_keeps_prefix_as_given() {
        let builder = Builder::new("tcp/router:7447").with_prefix("dev/team");
        assert_eq!(builder.prefix().unwrap(), "dev/team");
    }

    #[test]
    fn builder_accepts_empty_prefix() {
        let builder = Builder::new("tcp/router:7447").with_connect_timeout(Duration::from_secs(1));
        assert_eq!(builder.prefix(), None);
    }
}
