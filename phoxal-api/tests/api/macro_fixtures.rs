/// Fragment groups collect version-first source modules without filesystem
/// scanning, and an overlay replaces only the endpoint it changes.
mod fragment_collection {
    use std::any::TypeId;

    use phoxal_bus::{EndpointDescriptor, EndpointKind};

    // The source prefix deliberately has the same identifier as the child
    // revision. Provenance must use the exact segment after this prefix.
    pub mod v1_1 {
        pub mod source {
            pub mod v1_0 {
                pub mod component {
                    pub mod motor {
                        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                        pub enum Command {
                            Velocity(f32),
                            Stop,
                        }

                        #[expect(dead_code, reason = "duplicate support-name facade regression")]
                        pub struct Support(pub u8);

                        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                        pub struct Status {
                            pub stopped: bool,
                        }

                        crate::phoxal_api_fragment! {
                            path component(instance) / motor(capability);
                            version v1_0;
                            command command: Setpoint<Command>;
                            topic status: State<Status>;
                        }
                    }
                }
            }

            pub mod v1_1 {
                pub mod component {
                    pub mod motor {
                        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                        pub enum Command {
                            Position(f32),
                            Velocity(f32),
                            Stop,
                        }

                        #[expect(dead_code, reason = "duplicate support-name facade regression")]
                        pub struct Support(pub u16);

                        crate::phoxal_api_fragment! {
                            path component(instance) / motor(capability);
                            version v1_1;
                            replace command command: Setpoint<Command>;
                        }
                    }
                }
            }
        }
    }

    mod component {
        pub(super) use super::v1_1::source::v1_0::component::motor as base;
        pub(super) use super::v1_1::source::v1_1::component::motor as overlay;

        crate::phoxal_api_fragment_group! {
            fragments { base; overlay; }
        }
    }

    crate::phoxal_api_tree! {
        output generated;
        source crate::macro_fixtures::fragment_collection::v1_1::source;
        versions {
            version v1_0;
            latest version v1_1 extends v1_0;
        }
        fragments { component; }
    }

    #[test]
    fn group_and_overlay_materialize_distinct_payloads() {
        type Old = generated::v1_0::component::motor::Command;
        type New = generated::v1_1::component::motor::Command;
        let _: Old = Old::Stop;
        let _: New = New::Stop;
        assert_ne!(TypeId::of::<Old>(), TypeId::of::<New>());
        assert_eq!(
            <generated::v1_1::endpoint::component::motor::CommandEndpoint as EndpointDescriptor>::KIND,
            EndpointKind::Setpoint,
        );
        assert_eq!(
            <generated::v1_1::endpoint::component::motor::CommandEndpoint as EndpointDescriptor>::TOPIC,
            "v1.1/component/{instance}/motor/{capability}/command",
        );
        assert_eq!(
            TypeId::of::<generated::v1_0::component::motor::Status>(),
            TypeId::of::<<generated::v1_1::endpoint::component::motor::StatusEndpoint as EndpointDescriptor>::Payload>(),
        );
        assert_eq!(
            TypeId::of::<generated::v1_1::component::motor::Support>(),
            TypeId::of::<v1_1::source::v1_1::component::motor::Support>(),
        );
    }
}

/// Endpoint add/remove/inherit is the complete revision delta algebra. An
/// unchanged inherited endpoint keeps the parent revision's Rust payload type.
mod endpoint_delta_algebra {
    use std::any::TypeId;

    pub mod source {
        pub mod v1_0 {
            pub mod data {
                #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
                pub struct Shared {
                    pub value: u8,
                }
                #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
                #[expect(dead_code, reason = "removed endpoint fixture payload")]
                pub struct Retired {
                    pub value: u8,
                }

                crate::phoxal_api_fragment! {
                    path data;
                    version v1_0;
                    topic shared: State<Shared>;
                    topic retired: State<Retired>;
                }
            }
        }

        pub mod v1_1 {
            pub mod data {
                #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
                #[expect(dead_code, reason = "added endpoint fixture payload")]
                pub struct Added {
                    pub value: u16,
                }

                crate::phoxal_api_fragment! {
                    path data;
                    version v1_1;
                    remove endpoint retired;
                    topic added: State<Added>;
                }
            }
        }
    }

    crate::phoxal_api_tree! {
        output generated;
        source crate::macro_fixtures::endpoint_delta_algebra::source;
        versions {
            version v1_0;
            latest version v1_1 extends v1_0;
        }
        fragments { source::v1_0::data; source::v1_1::data; }
    }

    #[test]
    fn remove_add_and_inheritance_materialize_together() {
        assert_eq!(
            TypeId::of::<generated::v1_0::data::Shared>(),
            TypeId::of::<generated::v1_1::data::Shared>(),
        );
        let endpoints = generated::API_CONTRACT_MANIFEST[1]
            .contracts
            .iter()
            .map(|contract| contract.endpoint)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(endpoints.contains("v1.1::data::SharedEndpoint"));
        assert!(endpoints.contains("v1.1::data::AddedEndpoint"));
        assert!(!endpoints.contains("v1.1::data::RetiredEndpoint"));
    }
}

/// Root fragment order is not part of endpoint identity.
mod fragment_order_independence {
    macro_rules! fixture {
        ($module:ident, $($fragment:path);+ $(;)?) => {
            mod $module {
                pub mod source {
                    pub mod v1_0 {
                        pub mod alpha {
                            #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
                            #[expect(dead_code, reason = "order-only endpoint fixture payload")]
                            pub struct State { pub value: u8 }
                            crate::phoxal_api_fragment! {
                                path alpha;
                                version v1_0;
                                topic state: State<State>;
                            }
                        }
                        pub mod beta {
                            #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
                            #[expect(dead_code, reason = "order-only endpoint fixture payload")]
                            pub struct State { pub value: u16 }
                            crate::phoxal_api_fragment! {
                                path beta;
                                version v1_0;
                                topic state: State<State>;
                            }
                        }
                    }
                }
                crate::phoxal_api_tree! {
                    output generated;
                    source crate::macro_fixtures::fragment_order_independence::$module::source;
                    versions { latest version v1_0; }
                    fragments { $($fragment;)+ }
                }
            }
        };
    }

    fixture!(forward, source::v1_0::alpha; source::v1_0::beta;);
    fixture!(reverse, source::v1_0::beta; source::v1_0::alpha;);

    #[test]
    fn listing_order_does_not_change_output_contract() {
        let forward = &forward::generated::API_CONTRACT_MANIFEST[0];
        let reverse = &reverse::generated::API_CONTRACT_MANIFEST[0];
        assert_eq!(forward.name, reverse.name);
        assert_eq!(forward.contracts.len(), reverse.contracts.len());
        for (forward, reverse) in forward.contracts.iter().zip(reverse.contracts) {
            assert_eq!(forward.endpoint, reverse.endpoint);
            assert_eq!(forward.topic, reverse.topic);
            assert_eq!(forward.kind, reverse.kind);
            assert_eq!(forward.delivery, reverse.delivery);
        }
    }
}

/// Ordinary sibling types can be shared by several endpoints and queries.
mod semantic_surface {
    use phoxal_bus::{
        ApiVersion, EndpointDescriptor, QueryEndpointDescriptor, SampleDeliveryContract,
        StateContract, StateDeliveryContract,
    };

    pub mod source {
        pub mod v1_0 {
            pub mod data {
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct Supporting {
                    pub code: u8,
                }
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct SharedPayload {
                    pub value: u8,
                    pub support: Supporting,
                }
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct QueryRequest {
                    pub value: u8,
                }
                #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                pub struct QueryResponse {
                    pub value: u8,
                }

                crate::phoxal_api_fragment! {
                    path data;
                    version v1_0;
                    topic mirror: State<SharedPayload>;
                    topic sample: Sample<SharedPayload>;
                    query lookup: QueryRequest => QueryResponse;
                }
            }
        }
    }

    crate::phoxal_api_tree! {
        output generated;
        source crate::macro_fixtures::semantic_surface::source;
        versions { latest version v1_0; }
        fragments { source::v1_0::data; }
    }

    #[test]
    fn semantic_descriptors_keep_payloads_and_latest_mapping() {
        assert_eq!(<generated::v1_0::Api as ApiVersion>::ID, "v1.0");
        type Mirror = generated::v1_0::endpoint::data::MirrorEndpoint;
        type Sample = generated::v1_0::endpoint::data::SampleEndpoint;
        type Lookup = generated::v1_0::endpoint::data::LookupEndpoint;
        fn state<E: StateContract + StateDeliveryContract>() {}
        fn sample<E: SampleDeliveryContract>() {}
        fn shared<E: EndpointDescriptor<Payload = generated::v1_0::data::SharedPayload>>() {}
        fn query<E: QueryEndpointDescriptor>() {}
        state::<Mirror>();
        sample::<Sample>();
        shared::<Mirror>();
        shared::<Sample>();
        query::<Lookup>();
    }

    #[test]
    fn endpoint_catalogue_keeps_shared_payloads_as_distinct_records() {
        let contracts = generated::API_CONTRACT_MANIFEST[0].contracts;
        assert_eq!(contracts.len(), 3);
        let mirror = contracts
            .iter()
            .find(|contract| contract.endpoint.ends_with("MirrorEndpoint"))
            .expect("mirror endpoint record");
        let sample = contracts
            .iter()
            .find(|contract| contract.endpoint.ends_with("SampleEndpoint"))
            .expect("sample endpoint record");
        let lookup = contracts
            .iter()
            .find(|contract| contract.endpoint.ends_with("LookupEndpoint"))
            .expect("query endpoint record");
        assert_ne!(mirror.endpoint, sample.endpoint);
        assert_eq!(mirror.payload, sample.payload);
        assert!(
            lookup
                .request
                .is_some_and(|path| path.ends_with("QueryRequest"))
        );
        assert!(
            lookup
                .response
                .is_some_and(|path| path.ends_with("QueryResponse"))
        );
    }
}

/// Every strict semantic endpoint form resolves to the expected descriptor
/// kind while reusing ordinary sibling Rust payloads.
mod semantic_forms {
    use phoxal_bus::{
        EndpointDescriptor, EndpointKind, EventContract, QueryEndpointDescriptor,
        SampleDeliveryContract, SetpointDeliveryContract, StateContract, StateDeliveryContract,
        StreamContract, StreamDeliveryContract,
    };

    pub mod source {
        pub mod v1_0 {
            pub mod forms {
                #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
                pub struct Payload {
                    pub value: u8,
                }
                #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
                pub struct Request {
                    pub value: u8,
                }
                #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
                pub struct Response {
                    pub value: u8,
                }

                crate::phoxal_api_fragment! {
                    path forms;
                    version v1_0;
                    topic state: State<Payload>;
                    topic sample: Sample<Payload>;
                    topic event: Event<Payload>;
                    topic stream: Stream<Payload>;
                    command setpoint: Setpoint<Payload>;
                    command chunks: Stream<Payload>;
                    query lookup: Request => Response;
                }
            }
        }
    }

    crate::phoxal_api_tree! {
        output generated;
        source crate::macro_fixtures::semantic_forms::source;
        versions { latest version v1_0; }
        fragments { source::v1_0::forms; }
    }

    #[test]
    fn all_semantic_forms_have_expected_endpoint_kinds_and_markers() {
        type State = generated::v1_0::endpoint::forms::StateEndpoint;
        type Sample = generated::v1_0::endpoint::forms::SampleEndpoint;
        type Event = generated::v1_0::endpoint::forms::EventEndpoint;
        type Stream = generated::v1_0::endpoint::forms::StreamEndpoint;
        type Setpoint = generated::v1_0::endpoint::forms::SetpointEndpoint;
        type Chunks = generated::v1_0::endpoint::forms::ChunksEndpoint;
        type Lookup = generated::v1_0::endpoint::forms::LookupEndpoint;

        fn state<E: StateContract + StateDeliveryContract>() {}
        fn sample<E: SampleDeliveryContract>() {}
        fn event<E: EventContract + StreamDeliveryContract>() {}
        fn stream<E: StreamContract + StreamDeliveryContract>() {}
        fn setpoint<E: SetpointDeliveryContract>() {}
        fn query<E: QueryEndpointDescriptor>() {}
        state::<State>();
        sample::<Sample>();
        event::<Event>();
        stream::<Stream>();
        setpoint::<Setpoint>();
        stream::<Chunks>();
        query::<Lookup>();
        assert_eq!(<State as EndpointDescriptor>::KIND, EndpointKind::State);
        assert_eq!(<Sample as EndpointDescriptor>::KIND, EndpointKind::Sample);
        assert_eq!(<Event as EndpointDescriptor>::KIND, EndpointKind::Event);
        assert_eq!(<Stream as EndpointDescriptor>::KIND, EndpointKind::Stream);
        assert_eq!(
            <Setpoint as EndpointDescriptor>::KIND,
            EndpointKind::Setpoint
        );
        assert_eq!(<Chunks as EndpointDescriptor>::KIND, EndpointKind::Stream);
        assert_eq!(<Lookup as EndpointDescriptor>::KIND, EndpointKind::Query);
    }
}

/// A representative protocol tree. Protocol authoring remains separate from
/// the version-first Robot API fragment system.
mod protocol_tree {
    use phoxal_bus::{
        ApiVersion, AskQuery, EndpointDescriptor, Publish, QueryEndpointDescriptor, ServeQuery,
        Subscribe, Topic,
    };
    use phoxal_macros::phoxal_protocol;

    mod payload {
        pub mod connect {
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(tag = "schema")]
            pub enum Hello {
                #[serde(rename = "fixture.hello/v0")]
                V0 { token: String },
            }
        }

        pub mod run {
            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct SnapshotRequest {
                pub limit: u32,
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            #[serde(tag = "schema")]
            pub enum Snapshot {
                #[serde(rename = "fixture.snapshot/v0")]
                V0 { running: bool },
            }

            #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
            pub struct Progress {
                pub completed: u32,
            }
        }
    }

    phoxal_protocol! {
        protocol fixture {
            connect {
                command hello: Setpoint<crate::macro_fixtures::protocol_tree::payload::connect::Hello>;
            }

            run(execution) {
                query snapshot: crate::macro_fixtures::protocol_tree::payload::run::SnapshotRequest => crate::macro_fixtures::protocol_tree::payload::run::Snapshot;
                topic progress: Stream<crate::macro_fixtures::protocol_tree::payload::run::Progress>;
            }
        }
    }

    fn assert_publish<E: EndpointDescriptor>(_topic: Topic<Publish<E>>) {}
    fn assert_subscribe<E: EndpointDescriptor>(_topic: Topic<Subscribe<E>>) {}
    fn assert_ask<E: QueryEndpointDescriptor>(_topic: Topic<AskQuery<E>>) {}
    fn assert_serve<E: QueryEndpointDescriptor>(_topic: Topic<ServeQuery<E>>) {}

    #[test]
    fn protocol_keys_carry_no_version_segment() {
        assert_eq!(<fixture::Api as ApiVersion>::ID, "fixture");
        assert_eq!(
            <fixture::endpoint::connect::HelloEndpoint as EndpointDescriptor>::TOPIC,
            "fixture/connect/hello"
        );
        assert_eq!(
            fixture::topic::client().connect().hello().key(),
            "fixture/connect/hello"
        );
    }

    #[test]
    fn a_dynamic_segment_is_filled_from_the_builder() {
        assert_eq!(
            fixture::topic::client()
                .run("x7f")
                .expect("valid run segment")
                .progress()
                .key(),
            "fixture/run/x7f/progress"
        );
        fn assert_query<
            E: QueryEndpointDescriptor<
                    Request = payload::run::SnapshotRequest,
                    Response = payload::run::Snapshot,
                >,
        >() {
        }
        assert_query::<fixture::endpoint::run::SnapshotEndpoint>();
    }

    #[test]
    fn both_side_brandings_are_generated() {
        assert_publish(fixture::topic::client().connect().hello());
        assert_subscribe(fixture::topic::owner().connect().hello());
        assert_subscribe(
            fixture::topic::client()
                .run("x7f")
                .expect("valid run segment")
                .progress(),
        );
        assert_publish(
            fixture::topic::owner()
                .run("x7f")
                .expect("valid run segment")
                .progress(),
        );
        assert_ask(
            fixture::topic::client()
                .run("x7f")
                .expect("valid run segment")
                .snapshot(),
        );
        assert_serve(
            fixture::topic::owner()
                .run("x7f")
                .expect("valid run segment")
                .snapshot(),
        );
    }

    #[test]
    fn a_schema_tagged_body_round_trips_with_its_tag() {
        let hello = fixture::connect::Hello::V0 {
            token: "abc".to_string(),
        };
        let json = serde_json::to_value(&hello).unwrap();
        assert_eq!(json["schema"], "fixture.hello/v0");
        let bytes = rmp_serde::to_vec_named(&hello).unwrap();
        let decoded: fixture::connect::Hello = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(hello, decoded);
    }
}
