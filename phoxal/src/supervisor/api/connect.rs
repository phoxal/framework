//! The frozen attachment bootstrap: the one exchange two Phoxal binaries can
//! complete before they know whether they agree on anything else.
//!
//! `supervisor/connect` answers exactly one question - which framework train
//! built this supervisor - so a client that disagrees can say so precisely
//! instead of failing to decode a richer reply and reporting nothing.
//!
//! The reply reports the exact version, never a line or a verdict. Deciding
//! compatibility is the reader's, through
//! [`FrameworkVersion::is_compatible_with`]; a supervisor that shipped a
//! verdict would be answering a question only the peer can ask.
//!
//! # The promise includes the transport beneath it
//!
//! A frozen document is worth nothing if the path to it moves: a client that
//! cannot reach this key never gets to decode the one reply that would have
//! named the disagreement. So the freeze covers everything an attaching client
//! traverses before this reply, each owned and pinned by [`crate::bus`]:
//!
//! - **discovery** - a client-mode session on the operator's endpoint, with
//!   multicast scouting off, reading the executions of the routers it is
//!   directly connected to;
//! - **the key grammar** - `phoxal/{execution}/{topic}`, which this endpoint's
//!   own `supervisor/connect` composes into;
//! - **the query envelopes** - the `BusMetadata` attachment a reply carries and
//!   the `QueryFailure` body the error leg carries;
//! - **the encoding** - `phoxal/v0;codec=1`, MessagePack with named fields;
//! - **the Zenoh wire protocol version**, without which no session forms at
//!   all.
//!
//! All of them are preserved across framework majors on the same terms as the
//! documents below.

crate::endpoints! {
    self: Query<ConnectRequest, ConnectReply>;
}

use crate::version::FrameworkVersion;

/// Execution-scoped supervisor presence lease. This is a Liveliness key, not
/// an endpoint payload, and therefore deliberately sits outside the endpoint
/// manifest. It composes under one already-known execution root and signals
/// loss of that execution's control-plane authority; it performs no discovery.
///
/// It lives beside the bootstrap because the two are one attachment surface: a
/// client learns the train here and watches this key to learn the authority is
/// gone.
pub const PRESENCE_KEY: &str = "supervisor/presence";

/// The bootstrap request.
///
/// `V0` is frozen for every future framework line. It carries no fields and
/// never will: any argument would be a second thing the two peers must already
/// agree on before they are allowed to disagree.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(tag = "schema")]
pub enum ConnectRequest {
    #[serde(rename = "phoxal/supervisor-connect/v0")]
    V0 {},
}

/// The bootstrap reply: the framework train that built this supervisor, and
/// nothing else.
///
/// `V0` is frozen for every future framework line. It never grows a field -
/// no process state, robot identity, clock mode, manifest data, capability
/// list, CLI version, schema inventory, or node topology - so a binary from any
/// line can decode it and name the mismatch. Everything else a client wants is
/// behind an ordinary endpoint that the client may only call once the trains
/// agree.
#[derive(
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(tag = "schema")]
pub enum ConnectReply {
    #[serde(rename = "phoxal/supervisor-connect/v0")]
    V0 { framework: FrameworkVersion },
}


#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ConnectReply, ConnectRequest, FrameworkVersion, PRESENCE_KEY};

    /// The key the tree renders for the bootstrap, against the literal it is
    /// frozen at. The literal is written out rather than read back from the
    /// tree: a frozen bootstrap that inherits its key from whatever the tree
    /// currently renders is not frozen at all.
    #[test]
    fn the_bootstrap_key_is_pinned_to_its_literal() {
        let bootstrap = crate::supervisor::api::topics().connect().client();
        assert_eq!(bootstrap.key(), "supervisor/connect");
        assert_ne!(PRESENCE_KEY, bootstrap.key());
    }

    /// Both documents are pinned as literal JSON, including the tag strings and
    /// the canonical framework spelling, so no serde attribute change can move
    /// them without this test saying so.
    #[test]
    fn the_bootstrap_documents_are_pinned_to_their_literal_json() {
        assert_eq!(
            serde_json::to_value(ConnectRequest::V0 {}).unwrap(),
            json!({"schema": "phoxal/supervisor-connect/v0"})
        );
        assert_eq!(
            serde_json::to_value(ConnectReply::V0 {
                framework: FrameworkVersion::CURRENT,
            })
            .unwrap(),
            json!({
                "schema": "phoxal/supervisor-connect/v0",
                "framework": FrameworkVersion::CURRENT_SPELLING,
            })
        );
        // A fixed train, so the spelling itself is pinned and not merely
        // whatever `CURRENT` happens to render to today.
        assert_eq!(
            serde_json::to_value(ConnectReply::V0 {
                framework: FrameworkVersion::new(9, 9, 9),
            })
            .unwrap(),
            json!({
                "schema": "phoxal/supervisor-connect/v0",
                "framework": "9.9.9",
            })
        );
        assert_eq!(
            serde_json::from_value::<ConnectReply>(json!({
                "schema": "phoxal/supervisor-connect/v0",
                "framework": "9.9.9",
            }))
            .unwrap(),
            ConnectReply::V0 {
                framework: FrameworkVersion::new(9, 9, 9),
            }
        );
    }

    /// A reply from a line this binary does not implement fails by naming the
    /// foreign tag, which is the whole point of tagging the bootstrap.
    #[test]
    fn a_foreign_schema_tag_fails_by_naming_itself() {
        const FOREIGN: &str = "phoxal/supervisor-connect/v1";
        let foreign = json!({"schema": FOREIGN, "framework": "9.9.9"});

        let error = serde_json::from_value::<ConnectReply>(foreign.clone()).unwrap_err();
        assert!(
            error.to_string().contains(FOREIGN),
            "the mismatch diagnostic must name the foreign tag: {error}"
        );

    }
}
