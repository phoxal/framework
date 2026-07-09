//! `asset` - the official asset participant.
//!
//! A server-only official participant: no `#[step]`, one exclusive `#[server]` serving
//! `asset/get` from the robot root. It consumes no topics; it reads the robot
//! root via `ctx.robot_root()` (D33: official services build their state from the
//! model, not a typed config block).
//! On each request it resolves the requested path against the robot root and
//! returns the file bytes, reporting a missing file or an invalid path instead.
//! It rejects path traversal (empty paths, backslashes, `..` segments) before
//! touching the filesystem, so requests cannot escape the robot root.

use std::path::PathBuf;

use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

#[derive(phoxal::Api)]
struct Api {
    get: Server<api::asset::GetRequest, api::asset::GetResponse>,
}

#[phoxal::service(id = "asset", config = ())]
struct Asset {
    robot_root: PathBuf,
}

#[phoxal::behavior]
impl Asset {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        Ok((
            Self {
                robot_root: ctx.robot_root()?.to_path_buf(),
            },
            Self::Api {
                get: ctx.server(api::topic::new().asset().get()).await?,
            },
        ))
    }

    #[server(api = get)]
    async fn get(
        &mut self,
        api: &mut Self::Api,
        request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        let _ = api;
        Ok(resolve(&self.robot_root, &request.path))
    }
}

/// Resolve a requested asset path against the robot root, rejecting traversal.
fn resolve(robot_root: &std::path::Path, path: &str) -> api::asset::GetResponse {
    let requested = path.trim().trim_start_matches('/');
    if !is_safe_relative(requested) {
        return api::asset::GetResponse::InvalidPath;
    }
    let full = robot_root.join(requested);
    match std::fs::read(&full) {
        Ok(bytes) => api::asset::GetResponse::Found { bytes },
        Err(_) => api::asset::GetResponse::Missing,
    }
}

/// A safe relative asset path: non-empty, no backslashes, no empty/`..` segments.
fn is_safe_relative(path: &str) -> bool {
    if path.is_empty() || path.contains('\\') {
        return false;
    }
    path.split('/')
        .all(|segment| !segment.is_empty() && segment != "..")
}

fn main() -> phoxal::Result<()> {
    phoxal::run_v2::<Asset>()
}

#[cfg(test)]
mod tests {
    use super::{Asset, is_safe_relative, resolve};
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};
    use phoxal_api::ContractBody;
    use phoxal_api::y2026_1 as api;

    #[test]
    fn rejects_traversal_and_bad_paths() {
        assert!(!is_safe_relative(""));
        assert!(!is_safe_relative("../secret"));
        assert!(!is_safe_relative("a/../b"));
        assert!(!is_safe_relative("a\\b"));
        assert!(!is_safe_relative("a//b"));
        assert!(is_safe_relative("meshes/base.stl"));
    }

    #[test]
    fn resolves_existing_missing_and_invalid() {
        let dir = std::env::temp_dir().join(format!("phoxal-asset-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("meshes")).unwrap();
        std::fs::write(dir.join("meshes/base.stl"), b"solid").unwrap();

        assert!(matches!(
            resolve(&dir, "meshes/base.stl"),
            api::asset::GetResponse::Found { bytes } if bytes == b"solid"
        ));
        assert!(matches!(
            resolve(&dir, "meshes/missing.stl"),
            api::asset::GetResponse::Missing
        ));
        assert!(matches!(
            resolve(&dir, "../escape"),
            api::asset::GetResponse::InvalidPath
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn api_declares_the_y2026_1_asset_get_serve_contract() {
        assert_eq!(<Asset as Participant>::ID, "asset");

        let contracts = <<Asset as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert!(contracts.iter().any(|c| {
            c.topic == <api::asset::GetRequest as ContractBody>::TOPIC
                && c.role == ContractRole::Serve
        }));
        assert!(contracts.iter().any(|c| {
            c.topic == <api::asset::GetResponse as ContractBody>::TOPIC
                && c.role == ContractRole::Serve
        }));
    }
}
