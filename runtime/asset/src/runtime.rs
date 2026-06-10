use std::path::{Path, PathBuf};

use anyhow::Result;
use phoxal::api::asset::v1::{GetRequest, GetResponse, InvalidPathReason, UnavailableReason};
use phoxal::api::v1::topic;
use phoxal::runtime::clock::Step;
use phoxal::runtime::{EmptyArgs, QueryOptions, ReadCell, RobotRuntimeArgs};
use phoxal::runtime::{Io, Runtime, RuntimeInputs};

pub struct AssetView {
    bundle_root: PathBuf,
}

pub struct AssetRuntime {
    view: ReadCell<AssetView>,
}

pub fn asset_get(view: &AssetView, request: GetRequest) -> GetResponse {
    let requested = request.path.trim().trim_start_matches('/').to_string();
    match validate_requested_path(&requested) {
        Ok(()) => read_asset(view, &requested),
        Err(reason) => GetResponse::InvalidPath(reason),
    }
}

fn validate_requested_path(requested: &str) -> std::result::Result<(), InvalidPathReason> {
    if requested.is_empty() {
        return Err(InvalidPathReason::Empty);
    }
    if requested.contains('\\') {
        return Err(InvalidPathReason::BackslashSeparator);
    }
    if requested.split('/').any(|segment| segment == "..") {
        return Err(InvalidPathReason::ParentTraversal);
    }
    if requested.split('/').any(|segment| segment.is_empty()) {
        return Err(InvalidPathReason::EmptyComponent);
    }
    Ok(())
}

fn read_asset(view: &AssetView, requested: &str) -> GetResponse {
    let bundle_path = view.bundle_root.join(requested);
    if bundle_path.is_file() {
        return read_asset_bytes(&bundle_path);
    }
    GetResponse::NotFound
}

fn read_asset_bytes(asset_path: &Path) -> GetResponse {
    match std::fs::read(asset_path) {
        Ok(bytes) => GetResponse::Ok { bytes },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => GetResponse::NotFound,
        Err(_) => GetResponse::Unavailable(UnavailableReason::Io),
    }
}

#[async_trait::async_trait]
impl Runtime for AssetRuntime {
    const RUNTIME_ID: &'static str = "asset";

    type Args = EmptyArgs;
    type Config = PathBuf;
    type Input = std::convert::Infallible;

    fn config(_args: &Self::Args, common: &RobotRuntimeArgs) -> Result<Self::Config> {
        Ok(common.robot_config.clone())
    }

    fn clock_period(_config: &Self::Config) -> std::time::Duration {
        std::time::Duration::from_millis(20)
    }

    async fn new(io: &mut Io<Self::Input>, bundle_root: Self::Config) -> Result<Self> {
        let view = ReadCell::new(AssetView { bundle_root });
        io.serve_query_topic(
            topic::new().v1().asset().get(),
            view.reader(),
            QueryOptions::single(),
            asset_get,
        )
        .await?;
        Ok(Self { view })
    }

    async fn step(&mut self, _step: Step, _inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        let _ = &self.view;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{AssetView, asset_get};
    use anyhow::Result;
    use phoxal::api::asset::v1::{
        GetRequest as Request, GetResponse as Response, InvalidPathReason,
    };
    use phoxal::runtime::MESHES_DIR;
    use std::fs;

    #[test]
    fn resolve_existing_asset_returns_bytes() -> Result<()> {
        let bundle_root =
            std::env::temp_dir().join(format!("phoxal-runtime-asset-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&bundle_root);
        fs::create_dir_all(bundle_root.join(MESHES_DIR))?;
        fs::write(bundle_root.join(format!("{MESHES_DIR}/test.bin")), b"robot")?;

        let view = AssetView {
            bundle_root: bundle_root.clone(),
        };

        let response = asset_get(&view, Request::new(format!("{MESHES_DIR}/test.bin")));

        assert!(matches!(response, Response::Ok { bytes } if bytes == b"robot"));

        fs::remove_dir_all(&bundle_root)?;
        Ok(())
    }

    #[test]
    fn resolve_missing_asset_returns_not_found() -> Result<()> {
        let bundle_root = std::env::temp_dir().join(format!(
            "phoxal-runtime-asset-test-missing-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&bundle_root);
        fs::create_dir_all(&bundle_root)?;

        let view = AssetView {
            bundle_root: bundle_root.clone(),
        };

        let response = asset_get(&view, Request::new("meshes/missing.bin"));

        assert!(matches!(response, Response::NotFound));

        fs::remove_dir_all(&bundle_root)?;
        Ok(())
    }

    #[test]
    fn resolve_parent_traversal_path_returns_typed_reason() -> Result<()> {
        let bundle_root = std::env::temp_dir().join(format!(
            "phoxal-runtime-asset-test-traversal-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&bundle_root);
        fs::create_dir_all(&bundle_root)?;

        let view = AssetView {
            bundle_root: bundle_root.clone(),
        };

        let response = asset_get(&view, Request::new("../secret.bin"));

        assert!(matches!(
            response,
            Response::InvalidPath(InvalidPathReason::ParentTraversal)
        ));

        fs::remove_dir_all(&bundle_root)?;
        Ok(())
    }

    #[test]
    fn resolve_empty_path_returns_typed_reason() -> Result<()> {
        let bundle_root = std::env::temp_dir().join(format!(
            "phoxal-runtime-asset-test-empty-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&bundle_root);
        fs::create_dir_all(&bundle_root)?;

        let view = AssetView {
            bundle_root: bundle_root.clone(),
        };

        let response = asset_get(&view, Request::new(""));

        assert!(matches!(
            response,
            Response::InvalidPath(InvalidPathReason::Empty)
        ));

        fs::remove_dir_all(&bundle_root)?;
        Ok(())
    }

    #[test]
    fn resolve_backslash_path_returns_typed_reason() -> Result<()> {
        let bundle_root = std::env::temp_dir().join(format!(
            "phoxal-runtime-asset-test-backslash-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&bundle_root);
        fs::create_dir_all(&bundle_root)?;

        let view = AssetView {
            bundle_root: bundle_root.clone(),
        };

        let response = asset_get(&view, Request::new("meshes\\foo.bin"));

        assert!(matches!(
            response,
            Response::InvalidPath(InvalidPathReason::BackslashSeparator)
        ));

        fs::remove_dir_all(&bundle_root)?;
        Ok(())
    }

    #[test]
    fn resolve_empty_component_path_returns_typed_reason() -> Result<()> {
        let bundle_root = std::env::temp_dir().join(format!(
            "phoxal-runtime-asset-test-empty-comp-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&bundle_root);
        fs::create_dir_all(&bundle_root)?;

        let view = AssetView {
            bundle_root: bundle_root.clone(),
        };

        let response = asset_get(&view, Request::new("meshes//foo.bin"));

        assert!(matches!(
            response,
            Response::InvalidPath(InvalidPathReason::EmptyComponent)
        ));

        fs::remove_dir_all(&bundle_root)?;
        Ok(())
    }
}
