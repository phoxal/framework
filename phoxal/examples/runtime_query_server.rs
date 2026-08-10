//! A query-only participant with no scheduled step.
//!
//! `cargo run --example runtime_query_server` serves `robot/frame/lookup`; the
//! runner drives the queryable and serializes each call with lifecycle state.

use std::collections::BTreeMap;

use phoxal::api;
use phoxal::prelude::*;

struct Api;

struct FrameStoreState {
    // Runtime-private state - not a handle, so it lives on the participant
    // struct, not the `Api` struct.
    transforms: BTreeMap<(String, String), api::frame::FrameTransform>,
}

#[phoxal::service(id = "frame-store", state = FrameStoreState, api = Api)]
struct FrameStore;

impl Participant for FrameStore {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let mut transforms = BTreeMap::new();
        transforms.insert(
            ("base_link".to_string(), "lidar".to_string()),
            api::frame::FrameTransform {
                parent_frame_id: "base_link".to_string(),
                child_frame_id: "lidar".to_string(),
                translation_m: [0.12, 0.0, 0.30],
                rotation_quat_xyzw: [0.0, 0.0, 0.0, 1.0],
                stamp: None,
            },
        );
        ctx.query(api::topic::owner().frame().lookup(), Self::lookup)?;
        Ok((FrameStoreState { transforms }, Api))
    }
}

impl FrameStore {
    fn lookup(
        &self,
        _api: &Api,
        _query: QueryContext,
        request: api::frame::LookupRequest,
        state: &mut FrameStoreState,
    ) -> QueryResult<api::frame::LookupResponse> {
        // An unknown pair is an answer, not a failure: the caller learns the
        // store has no such transform without having to read an error string.
        Ok(api::frame::LookupResponse {
            transform: state
                .transforms
                .get(&(
                    request.target_frame_id.clone(),
                    request.source_frame_id.clone(),
                ))
                .cloned(),
        })
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<FrameStore>()
}
