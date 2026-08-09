//! Standalone `phoxal_api_tree!` invocations that exercise the generator on
//! shapes the production tree does not contain, and prove a nested invocation
//! stays self-contained.

/// A nested dynamic tree that reuses the same var name across levels. This
/// module must *compile*: it proves the builder's positional field storage
/// (`__seg0`, `__seg1`) does not collide into duplicate struct fields, and that
/// each level's value is carried independently (a regression would fail to build
/// or collapse the key).
mod reused_var_name {
    crate::phoxal_api_tree! {
        version v0_1 {
            outer(id) {
                inner(id) {
                    struct Body { x: u8 }
                    topic event: state Body;
                }
            }
        }
        latest v0_1;
    }

    #[test]
    fn nested_reused_var_carries_each_level_independently() {
        let topic = v0_1::topic::client()
            .outer("a")
            .expect("valid outer segment")
            .inner("b")
            .expect("valid inner segment")
            .event();
        assert_eq!(topic.key(), "v0.1/outer/a/inner/b/event");
    }
}

/// A standalone API revision used to exercise the macro independently of the
/// production tree.
mod standalone_version {
    use phoxal_bus::{ApiVersion, ContractBody};

    crate::phoxal_api_tree! {
        version v0_1 {
            sample {
                struct Body { value: u8, note: Option<String> }
                topic body: state Body;
            }
        }
        latest v0_1;
    }

    #[test]
    fn standalone_version_expands_and_round_trips() {
        assert_eq!(<v0_1::Api as ApiVersion>::ID, "v0.1");
        assert_eq!(
            <v0_1::sample::Body as ContractBody>::TOPIC,
            "v0.1/sample/body"
        );
        assert_eq!(
            v0_1::topic::client().sample().body().key(),
            "v0.1/sample/body"
        );

        let body = v0_1::sample::Body {
            value: 7,
            note: Some("standalone".to_string()),
        };
        let bytes = rmp_serde::to_vec_named(&body).unwrap();
        let decoded: v0_1::sample::Body = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(body, decoded);
    }
}

/// The #1002 semantic surface keeps payload ownership in ordinary Rust
/// modules. The macro only materializes revision-local aliases, descriptors,
/// and builders around those paths.
mod semantic_surface {
    use phoxal_bus::{
        ApiVersion, EndpointDescriptor, QueryEndpointDescriptor, SampleDeliveryContract,
        StateContract, StateDeliveryContract,
    };

    #[allow(dead_code)]
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct Supporting {
        pub code: u8,
    }

    #[allow(dead_code)]
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct SharedPayload {
        pub value: u8,
        pub support: Supporting,
    }

    #[allow(dead_code)]
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct V1QueryRequest {
        pub value: u8,
    }

    #[allow(dead_code)]
    #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    pub struct V1QueryResponse {
        pub value: u8,
    }

    crate::phoxal_api! {
        latest version v0.2 {
            data {
                topic mirror: State<crate::tests::macro_fixtures::semantic_surface::SharedPayload>;
                topic sample: Sample<crate::tests::macro_fixtures::semantic_surface::SharedPayload>;
                query lookup: crate::tests::macro_fixtures::semantic_surface::V1QueryRequest => crate::tests::macro_fixtures::semantic_surface::V1QueryResponse;
            }
        }
    }

    #[test]
    fn semantic_descriptors_keep_external_payloads_and_latest_mapping() {
        assert_eq!(<v0_2::Api as ApiVersion>::ID, "v0.2");
        assert_eq!(
            <v0_2::endpoint::data::SampleEndpoint as EndpointDescriptor>::TOPIC,
            "v0.2/data/sample"
        );
        type Mirror = v0_2::endpoint::data::MirrorEndpoint;
        type Sample = v0_2::endpoint::data::SampleEndpoint;
        type Lookup = v0_2::endpoint::data::LookupEndpoint;
        // Semantic node modules re-export the authored domain module, not just
        // endpoint payload leaves. Supporting siblings remain available from
        // the generated version facade.
        let _: Option<v0_2::data::Supporting> = None;
        fn state<E: StateContract + StateDeliveryContract>() {}
        fn sample<E: SampleDeliveryContract>() {}
        state::<Mirror>();
        sample::<Sample>();
        assert_ne!(
            <Mirror as EndpointDescriptor>::TOPIC,
            <Sample as EndpointDescriptor>::TOPIC
        );
        fn shared<E: EndpointDescriptor<Payload = SharedPayload>>() {}
        shared::<Mirror>();
        shared::<Sample>();
        fn query<E: QueryEndpointDescriptor>() {}
        query::<Lookup>();
    }
}

/// Every strict semantic endpoint form compiles through the public macro and
/// resolves to a distinct descriptor kind while reusing ordinary Rust payloads.
mod semantic_forms {
    use phoxal_bus::{
        EndpointDescriptor, EndpointKind, EventContract, QueryEndpointDescriptor,
        SampleDeliveryContract, SetpointDeliveryContract, StateContract, StateDeliveryContract,
        StreamContract, StreamDeliveryContract,
    };

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

    crate::phoxal_api! {
        latest version v0.3 {
            forms {
                topic state: State<crate::tests::macro_fixtures::semantic_forms::Payload>;
                topic sample: Sample<crate::tests::macro_fixtures::semantic_forms::Payload>;
                topic event: Event<crate::tests::macro_fixtures::semantic_forms::Payload>;
                topic stream: Stream<crate::tests::macro_fixtures::semantic_forms::Payload>;
                command setpoint: Setpoint<crate::tests::macro_fixtures::semantic_forms::Payload>;
                command chunks: Stream<crate::tests::macro_fixtures::semantic_forms::Payload>;
                query lookup: crate::tests::macro_fixtures::semantic_forms::Request => crate::tests::macro_fixtures::semantic_forms::Response;
            }
        }
    }

    #[test]
    fn all_semantic_forms_have_expected_endpoint_kinds_and_markers() {
        type State = v0_3::endpoint::forms::StateEndpoint;
        type Sample = v0_3::endpoint::forms::SampleEndpoint;
        type Event = v0_3::endpoint::forms::EventEndpoint;
        type Stream = v0_3::endpoint::forms::StreamEndpoint;
        type Setpoint = v0_3::endpoint::forms::SetpointEndpoint;
        type Chunks = v0_3::endpoint::forms::ChunksEndpoint;
        type Lookup = v0_3::endpoint::forms::LookupEndpoint;

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

/// A representative **protocol** tree: the mode `phoxal-supervisor-api` is
/// authored in. It is deliberately a fixture rather than a real protocol - this
/// crate owns the robot API, not any process-boundary protocol - and covers the
/// whole surface a protocol author uses: relative keys, both side brandings, a
/// query endpoint, a dynamic segment, and a developer-owned schema-tagged
/// document body.
mod protocol_tree {
    use phoxal_bus::{
        ApiVersion, AskQuery, ContractBody, Publish, ServeQuery, Subscribe, Topic, TopicRole,
    };

    crate::phoxal_protocol! {
        protocol fixture {
            connect {
                /// The version tag is the developer's, not the macro's: the
                /// enum variant IS the schema version, selected by serde at
                /// parse time. Pre-v1, `V0` is edited in place.
                #[serde(tag = "schema")]
                enum Hello {
                    #[serde(rename = "fixture.hello/v0")]
                    V0 { token: String },
                }

                topic hello: command Hello;
            }

            run(execution) {
                struct SnapshotRequest {
                    limit: u32,
                }

                #[serde(tag = "schema")]
                enum Snapshot {
                    #[serde(rename = "fixture.snapshot/v0")]
                    V0 { running: bool },
                }

                struct Progress {
                    completed: u32,
                }

                topic snapshot: query SnapshotRequest => Snapshot;
                topic progress: state Progress;
            }
        }
    }

    fn assert_publish<B: ContractBody>(_topic: Topic<Publish<B>>) {}
    fn assert_subscribe<B: ContractBody>(_topic: Topic<Subscribe<B>>) {}
    fn assert_ask<E: phoxal_bus::QueryEndpointDescriptor>(_topic: Topic<AskQuery<E>>) {}
    fn assert_serve<E: phoxal_bus::QueryEndpointDescriptor>(_topic: Topic<ServeQuery<E>>) {}

    /// A protocol key carries no `v0.1/` segment: it is relative to the
    /// protocol, which the bus session then mounts under its execution-scoped
    /// root.
    #[test]
    fn protocol_keys_carry_no_version_segment() {
        assert_eq!(<fixture::Api as ApiVersion>::ID, "fixture");
        assert_eq!(
            <fixture::connect::Hello as ContractBody>::TOPIC,
            "fixture/connect/hello"
        );
        assert_eq!(
            <fixture::connect::Hello as ContractBody>::NAME,
            "fixture::connect::Hello"
        );
        assert_eq!(
            <fixture::connect::Hello as ContractBody>::CONTRACT,
            "connect::Hello"
        );
        assert_eq!(
            <fixture::connect::Hello as ContractBody>::ROLE,
            TopicRole::Command
        );
        assert_eq!(
            fixture::topic::client().connect().hello().key(),
            "fixture/connect/hello"
        );
    }

    /// A dynamic node fills its segment exactly as in API mode, one segment
    /// shorter.
    #[test]
    fn a_dynamic_segment_is_filled_from_the_builder() {
        assert_eq!(
            <fixture::run::Progress as ContractBody>::TOPIC,
            "fixture/run/{execution}/progress"
        );
        assert_eq!(
            fixture::topic::client()
                .run("x7f")
                .expect("valid run segment")
                .progress()
                .key(),
            "fixture/run/x7f/progress"
        );
        // A query's request and response share the one key, as in API mode.
        assert_eq!(
            <fixture::run::SnapshotRequest as ContractBody>::TOPIC,
            <fixture::run::Snapshot as ContractBody>::TOPIC
        );
    }

    /// Both side-branded builder trees are generated, and taking the wrong side
    /// of a protocol topic is the same compile error as in API mode - these
    /// calls would not build if a brand flipped.
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

    /// The schema tag is inside the payload, where the developer put it - the
    /// macro neither adds nor interprets one.
    #[test]
    fn a_schema_tagged_body_round_trips_with_its_tag() {
        let hello = fixture::connect::Hello::V0 {
            token: "abc".to_string(),
        };
        let json = serde_json::to_value(&hello).unwrap();
        assert_eq!(json["schema"], "fixture.hello/v0");
        assert_eq!(json["token"], "abc");

        let bytes = rmp_serde::to_vec_named(&hello).unwrap();
        let decoded: fixture::connect::Hello = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(hello, decoded);

        // An unknown schema tag fails at parse time, in the type system,
        // rather than reaching a runtime version comparison.
        let foreign = serde_json::json!({"schema": "fixture.hello/v9", "token": "abc"});
        assert!(serde_json::from_value::<fixture::connect::Hello>(foreign).is_err());
    }
}
