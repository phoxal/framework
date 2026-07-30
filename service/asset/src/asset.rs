//! `asset` - the official asset participant.
//!
//! A query-only official participant serving declared compiled assets. Requests
//! are resolved through `ctx.assets()`, so they cannot reach `robot.json`,
//! participant binaries, undeclared files, or paths outside the asset root.

use phoxal::api;
use phoxal::prelude::*;

pub struct Api;

pub struct AssetState {
    assets: AssetResolver,
}

#[phoxal::service(state = AssetState, api = Api)]
pub struct Asset;

impl Participant for Asset {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        ctx.query(api::topic::owner().asset().get(), Self::get)
            .await?;
        Ok((
            AssetState {
                assets: ctx.assets()?.clone(),
            },
            Api,
        ))
    }
}

impl Asset {
    async fn get(
        &self,
        _api: &Api,
        request: api::asset::GetRequest,
        state: &mut AssetState,
    ) -> QueryResult<api::asset::GetResponse> {
        Ok(resolve(&state.assets, &request.path))
    }
}

/// Resolve a requested logical id against the declared asset set.
fn resolve(assets: &AssetResolver, path: &str) -> api::asset::GetResponse {
    let Ok(id) = AssetId::new(path.trim()) else {
        return api::asset::GetResponse::InvalidPath;
    };
    match assets.read(&id) {
        Ok(bytes) => api::asset::GetResponse::Found { bytes },
        Err(_) => api::asset::GetResponse::Missing,
    }
}

#[cfg(test)]
mod tests {
    use phoxal::AssetId;

    #[test]
    fn rejects_traversal_and_bad_paths() {
        assert!(AssetId::new("").is_err());
        assert!(AssetId::new("../secret").is_err());
        assert!(AssetId::new("a/../b").is_err());
        assert!(AssetId::new("a\\b").is_err());
        assert!(AssetId::new("a//b").is_err());
        assert!(AssetId::new("meshes/base.stl").is_ok());
    }
}
