//! Rust-authored `phoxal.bootstrap.v1` wire contracts.

/// The `SessionOffers` wire contract.
#[phoxal::message(package = "phoxal.bootstrap.v1")]
#[derive(Eq)]
pub struct SessionOffers {
    /// `sessions`.
    #[phoxal(tag = 1)]
    pub sessions: Vec<SessionOffer>,
}

/// The `SessionOffer` wire contract.
#[phoxal::message(package = "phoxal.bootstrap.v1")]
#[derive(Eq)]
pub struct SessionOffer {
    /// `protocol`.
    #[phoxal(tag = 1)]
    pub protocol: String,
    /// `key_prefix`.
    #[phoxal(tag = 2)]
    pub key_prefix: String,
}
