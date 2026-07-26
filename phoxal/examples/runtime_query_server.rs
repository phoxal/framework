//! A server-only participant: `#[setup]` + one exclusive `#[server]`, no `#[step]`.
//!
//! `cargo run --example runtime_query_server` serves `asset/get`; the runner
//! drives the queryable and serializes each call with `&mut self`.

use std::collections::BTreeMap;

use phoxal::api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    get: Server<api::asset::GetRequest, api::asset::GetResponse>,
}

#[phoxal::service(id = "asset-store")]
struct AssetStore {
    // Runtime-private state - not a handle, so it lives on the participant
    // struct, not the `Api` struct.
    assets: BTreeMap<String, Vec<u8>>,
}

#[phoxal::behavior]
impl AssetStore {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let mut assets = BTreeMap::new();
        assets.insert("map.pgm".to_string(), vec![0x50, 0x35, 0x0a]);
        Ok((
            Self { assets },
            Self::Api {
                get: ctx.server(api::topic::client().asset().get()).await?,
            },
        ))
    }

    // The default exclusive server: serialized with any `#[step]`, holds `&mut
    // self` and `&mut Self::Api`. `api = get` names the `Api` struct's `Server`
    // slot this handler implements ("`Api` declares the bus contract,
    // `behavior` implements runtime logic").
    #[server(api = get)]
    async fn get(
        &mut self,
        _api: &mut Self::Api,
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
