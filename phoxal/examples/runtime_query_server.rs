//! A query-only participant with no scheduled step.
//!
//! `cargo run --example runtime_query_server` serves `asset/get`; the runner
//! drives the queryable and serializes each call with lifecycle state.

use std::collections::BTreeMap;

use phoxal::prelude::*;
use phoxal_supervisor_api::supervisor;

struct Api;

struct AssetStoreState {
    // Runtime-private state - not a handle, so it lives on the participant
    // struct, not the `Api` struct.
    assets: BTreeMap<String, Vec<u8>>,
}

#[phoxal::service(id = "asset-store", state = AssetStoreState, api = Api)]
struct AssetStore;

impl Participant for AssetStore {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let mut assets = BTreeMap::new();
        assets.insert("map.pgm".to_string(), vec![0x50, 0x35, 0x0a]);
        ctx.query(supervisor::topic::owner().asset().get(), Self::get)?;
        Ok((AssetStoreState { assets }, Api))
    }
}

impl AssetStore {
    fn get(
        &self,
        _api: &Api,
        _query: QueryContext,
        request: supervisor::asset::GetRequest,
        state: &mut AssetStoreState,
    ) -> QueryResult<supervisor::asset::GetResponse> {
        match state.assets.get(&request.path) {
            Some(bytes) => Ok(supervisor::asset::GetResponse::Found {
                bytes: bytes.clone(),
            }),
            None => Ok(supervisor::asset::GetResponse::Missing),
        }
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<AssetStore>()
}
