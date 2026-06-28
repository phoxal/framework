//! `asset` — the official asset runtime.
//!
//! A server-only official runtime: no `#[step]`, one exclusive `#[server]` serving
//! `asset/get` from the deploy bundle. It reads the bundle root via
//! `ctx.bundle_root()` (D33: official runtimes build their state from the model /
//! bundle, not a typed config block) and returns file bytes, rejecting path
//! traversal before touching the filesystem.

use std::path::PathBuf;

use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "asset", api = y2026_1)]
struct Asset {
    bundle_root: PathBuf,
}

#[phoxal::runtime]
impl Asset {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        Ok(Self {
            bundle_root: ctx.bundle_root()?.to_path_buf(),
        })
    }

    #[server(topic = api::topic::new().asset().get())]
    async fn get(
        &mut self,
        request: api::asset::GetRequest,
    ) -> ServerResult<api::asset::GetResponse> {
        Ok(resolve(&self.bundle_root, &request.path))
    }
}

/// Resolve a requested asset path against the bundle root, rejecting traversal.
fn resolve(bundle_root: &std::path::Path, path: &str) -> api::asset::GetResponse {
    let requested = path.trim().trim_start_matches('/');
    if !is_safe_relative(requested) {
        return api::asset::GetResponse::InvalidPath;
    }
    let full = bundle_root.join(requested);
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
    phoxal::run::<Asset>()
}

#[cfg(test)]
mod tests {
    use super::{is_safe_relative, resolve};
    use phoxal::api::y2026_1 as api;

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
}
