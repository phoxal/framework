//! A server-only runtime: `#[setup]` + one exclusive `#[server]`, no `#[step]`.
//!
//! `cargo run --example runtime_query_server` serves `asset/get`; the runner
//! drives the queryable and serializes each call with `&mut self`.

use std::collections::BTreeMap;

use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "asset-store", api = y2026_1)]
struct AssetStore {
    // Runtime-private state — not a handle, so the derive ignores it.
    assets: BTreeMap<String, Vec<u8>>,
}

#[phoxal::runtime]
impl AssetStore {
    #[setup]
    async fn setup(_ctx: &mut SetupContext<Self>) -> Result<Self> {
        let mut assets = BTreeMap::new();
        assets.insert("map.pgm".to_string(), vec![0x50, 0x35, 0x0a]);
        Ok(Self { assets })
    }

    // The default exclusive server: serialized with any `#[step]`, holds `&mut self`.
    #[server(topic = api::topic::new().asset().get())]
    async fn get(
        &mut self,
        request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        match self.assets.get(&request.path) {
            Some(bytes) => Ok(api::asset::GetResponse::Found {
                bytes: bytes.clone(),
            }),
            None => Ok(api::asset::GetResponse::Missing),
        }
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<AssetStore>()
}
