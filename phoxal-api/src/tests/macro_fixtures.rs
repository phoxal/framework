/// The semantic surface keeps payload ownership in ordinary Rust modules. The
/// macro only materializes revision-local aliases, descriptors, and builders
/// around those paths.
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

    #[test]
    fn endpoint_manifest_keeps_shared_payloads_as_distinct_endpoint_records() {
        assert_eq!(API_CONTRACT_MANIFEST.len(), 1);
        let contracts = API_CONTRACT_MANIFEST[0].contracts;
        assert_eq!(contracts.len(), 3);
        let mirror = contracts
            .iter()
            .find(|contract| contract.endpoint.ends_with("MirrorEndpoint"))
            .expect("mirror endpoint manifest record");
        let sample = contracts
            .iter()
            .find(|contract| contract.endpoint.ends_with("SampleEndpoint"))
            .expect("sample endpoint manifest record");
        let lookup = contracts
            .iter()
            .find(|contract| contract.endpoint.ends_with("LookupEndpoint"))
            .expect("query endpoint manifest record");
        assert_ne!(mirror.endpoint, sample.endpoint);
        assert_eq!(mirror.payload, sample.payload);
        assert_eq!(lookup.payload, None);
        assert_eq!(
            lookup.request,
            Some("crate::tests::macro_fixtures::semantic_surface::V1QueryRequest")
        );
        assert_eq!(
            lookup.response,
            Some("crate::tests::macro_fixtures::semantic_surface::V1QueryResponse")
        );
        assert!(!API_CONTRACT_MANIFEST[0].fingerprint.is_empty());
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
                command hello: Setpoint<crate::tests::macro_fixtures::protocol_tree::payload::connect::Hello>;
            }

            run(execution) {
                query snapshot: crate::tests::macro_fixtures::protocol_tree::payload::run::SnapshotRequest => crate::tests::macro_fixtures::protocol_tree::payload::run::Snapshot;
                topic progress: Stream<crate::tests::macro_fixtures::protocol_tree::payload::run::Progress>;
            }
        }
    }

    fn assert_publish<E: EndpointDescriptor>(_topic: Topic<Publish<E>>) {}
    fn assert_subscribe<E: EndpointDescriptor>(_topic: Topic<Subscribe<E>>) {}
    fn assert_ask<E: phoxal_bus::QueryEndpointDescriptor>(_topic: Topic<AskQuery<E>>) {}
    fn assert_serve<E: phoxal_bus::QueryEndpointDescriptor>(_topic: Topic<ServeQuery<E>>) {}

    /// A protocol key carries no `v0.1/` segment: it is relative to the
    /// protocol, which the bus session then mounts under its execution-scoped
    /// root.
    #[test]
    fn protocol_keys_carry_no_version_segment() {
        assert_eq!(<fixture::Api as ApiVersion>::ID, "fixture");
        assert_eq!(
            <fixture::endpoint::connect::HelloEndpoint as EndpointDescriptor>::TOPIC,
            "fixture/connect/hello"
        );
        assert_eq!(
            <fixture::endpoint::connect::HelloEndpoint as EndpointDescriptor>::NAME,
            "fixture::connect::HelloEndpoint"
        );
        assert_eq!(
            <fixture::endpoint::connect::HelloEndpoint as EndpointDescriptor>::CONTRACT,
            "connect::HelloEndpoint"
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
            <fixture::endpoint::run::ProgressEndpoint as EndpointDescriptor>::TOPIC,
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
        fn assert_query<
            E: QueryEndpointDescriptor<
                    Request = payload::run::SnapshotRequest,
                    Response = payload::run::Snapshot,
                >,
        >() {
        }
        assert_query::<fixture::endpoint::run::SnapshotEndpoint>();
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
