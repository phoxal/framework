/// Fragment groups collect source modules without filesystem scanning, and
/// every relayed fragment lands in the tree of the family its path roots at.
mod fragment_collection {
    use std::any::TypeId;

    use phoxal_bus::{EndpointDescriptor, EndpointKind};

    // The source prefix deliberately ends with the same identifier as the
    // family root. Payload provenance must use the exact segments after this
    // prefix, never the last segment of the prefix itself.
    pub mod robot {
        pub mod source {
            pub mod robot {
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
                            path robot / component(instance) / motor(capability);
                            command command: Setpoint<Command>;
                            topic status: State<Status>;
                        }
                    }
                }

                pub mod drive {
                    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
                    pub struct Target {
                        pub speed: f32,
                    }

                    crate::phoxal_api_fragment! {
                        path robot / drive;
                        command target: Setpoint<Target>;
                    }
                }
            }
        }
    }

    mod relay {
        pub(super) use super::robot::source::robot::component::motor;
        pub(super) use super::robot::source::robot::drive;

        crate::phoxal_api_fragment_group! {
            fragments { motor; drive; }
        }
    }

    crate::phoxal_api_tree! {
        output generated;
        source crate::macro_fixtures::fragment_collection::robot::source;
        fragments { relay; }
    }

    #[test]
    fn a_relayed_group_materializes_every_fragment_into_one_family() {
        assert_eq!(
            <generated::robot::endpoint::component::motor::CommandEndpoint as EndpointDescriptor>::KIND,
            EndpointKind::Setpoint,
        );
        assert_eq!(
            <generated::robot::endpoint::component::motor::CommandEndpoint as EndpointDescriptor>::TOPIC,
            "robot/component/{instance}/motor/{capability}/command",
        );
        assert_eq!(
            <generated::robot::endpoint::drive::TargetEndpoint as EndpointDescriptor>::TOPIC,
            "robot/drive/target",
        );
        assert_eq!(
            TypeId::of::<robot::source::robot::component::motor::Status>(),
            TypeId::of::<<generated::robot::endpoint::component::motor::StatusEndpoint as EndpointDescriptor>::Payload>(),
        );
        assert_eq!(
            TypeId::of::<generated::robot::component::motor::Support>(),
            TypeId::of::<robot::source::robot::component::motor::Support>(),
        );
    }
}

/// Root fragment order is not part of endpoint identity.
mod fragment_order_independence {
    macro_rules! fixture {
        ($module:ident, $($fragment:path);+ $(;)?) => {
            mod $module {
                pub mod source {
                    pub mod robot {
                        pub mod alpha {
                            #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
                            #[expect(dead_code, reason = "order-only endpoint fixture payload")]
                            pub struct State { pub value: u8 }
                            crate::phoxal_api_fragment! {
                                path robot / alpha;
                                topic state: State<State>;
                            }
                        }
                        pub mod beta {
                            #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
                            #[expect(dead_code, reason = "order-only endpoint fixture payload")]
                            pub struct State { pub value: u16 }
                            crate::phoxal_api_fragment! {
                                path robot / beta;
                                topic state: State<State>;
                            }
                        }
                    }
                }
                crate::phoxal_api_tree! {
                    output generated;
                    source crate::macro_fixtures::fragment_order_independence::$module::source;
                    fragments { $($fragment;)+ }
                }
            }
        };
    }

    fixture!(forward, source::robot::alpha; source::robot::beta;);
    fixture!(reverse, source::robot::beta; source::robot::alpha;);

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
        ApiFamily, EndpointDescriptor, QueryEndpointDescriptor, SampleDeliveryContract,
        StateContract, StateDeliveryContract,
    };

    pub mod source {
        pub mod robot {
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
                    path robot / data;
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
        fragments { source::robot::data; }
    }

    #[test]
    fn semantic_descriptors_keep_payloads_and_family_mapping() {
        assert_eq!(<generated::robot::Api as ApiFamily>::ID, "robot");
        type Mirror = generated::robot::endpoint::data::MirrorEndpoint;
        type Sample = generated::robot::endpoint::data::SampleEndpoint;
        type Lookup = generated::robot::endpoint::data::LookupEndpoint;
        fn state<E: StateContract + StateDeliveryContract>() {}
        fn sample<E: SampleDeliveryContract>() {}
        fn shared<E: EndpointDescriptor<Payload = generated::robot::data::SharedPayload>>() {}
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
        pub mod robot {
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
                    path robot / forms;
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
        fragments { source::robot::forms; }
    }

    #[test]
    fn all_semantic_forms_have_expected_endpoint_kinds_and_markers() {
        type State = generated::robot::endpoint::forms::StateEndpoint;
        type Sample = generated::robot::endpoint::forms::SampleEndpoint;
        type Event = generated::robot::endpoint::forms::EventEndpoint;
        type Stream = generated::robot::endpoint::forms::StreamEndpoint;
        type Setpoint = generated::robot::endpoint::forms::SetpointEndpoint;
        type Chunks = generated::robot::endpoint::forms::ChunksEndpoint;
        type Lookup = generated::robot::endpoint::forms::LookupEndpoint;

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
/// the fragment system that assembles contract families.
mod protocol_tree {
    use phoxal_bus::{
        ApiFamily, AskQuery, EndpointDescriptor, Publish, QueryEndpointDescriptor, ServeQuery,
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
    fn protocol_keys_are_rooted_at_the_protocol_name() {
        assert_eq!(<fixture::Api as ApiFamily>::ID, "fixture");
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
