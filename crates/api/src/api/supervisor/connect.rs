//! The frozen attachment bootstrap: the one exchange two Phoxal binaries can
//! complete before they know whether they agree on anything else.
//!
//! `supervisor/connect` answers exactly one question - which framework train
//! built this supervisor - so a client that disagrees can say so precisely
//! instead of failing to decode a richer reply and reporting nothing.

use phoxal_runtime_contract::version::FrameworkVersion;

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
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "schema")]
pub enum ConnectReply {
    #[serde(rename = "phoxal/supervisor-connect/v0")]
    V0 { framework: FrameworkVersion },
}

phoxal_macros::phoxal_api_fragment! {
    path supervisor / connect;

    query self: ConnectRequest => ConnectReply;
}

#[cfg(test)]
mod tests {
    use phoxal_bus::EndpointDescriptor;
    use serde_json::json;

    use super::{ConnectReply, ConnectRequest, FrameworkVersion, PRESENCE_KEY};

    type ConnectEndpoint = crate::api::generated::supervisor::endpoint::connect::TopicEndpoint;

    /// The key is written out rather than read back from the descriptor: a
    /// frozen bootstrap that inherits its key from whatever the tree currently
    /// generates is not frozen at all.
    #[test]
    fn the_bootstrap_key_is_pinned_to_its_literal() {
        assert_eq!(
            <ConnectEndpoint as EndpointDescriptor>::TOPIC,
            "supervisor/connect"
        );
        assert_ne!(PRESENCE_KEY, <ConnectEndpoint as EndpointDescriptor>::TOPIC);
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

    /// The bus codec is MessagePack, so the freeze has to hold there too.
    #[test]
    fn the_bootstrap_documents_round_trip_on_the_bus_codec() {
        let request = ConnectRequest::V0 {};
        let encoded = rmp_serde::to_vec_named(&request).unwrap();
        assert_eq!(
            rmp_serde::from_slice::<ConnectRequest>(&encoded).unwrap(),
            request
        );

        let reply = ConnectReply::V0 {
            framework: FrameworkVersion::CURRENT,
        };
        let encoded = rmp_serde::to_vec_named(&reply).unwrap();
        assert_eq!(
            rmp_serde::from_slice::<ConnectReply>(&encoded).unwrap(),
            reply
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

        let encoded = rmp_serde::to_vec_named(&foreign).unwrap();
        let error = rmp_serde::from_slice::<ConnectReply>(&encoded).unwrap_err();
        assert!(
            error.to_string().contains(FOREIGN),
            "the mismatch diagnostic must name the foreign tag: {error}"
        );
    }
}
