//! Owned staging for one imported Robot's assets and decoded textures.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use phoxal::identity::ExecutionId;
use phoxal::model::asset::AssetId;

use crate::generation::stage_decoded_images;
use crate::glb::DecodedMesh;

#[derive(Clone)]
pub struct StagedRobotAssets {
    execution: ExecutionId,
    asset_root: PathBuf,
    texture_root: PathBuf,
}

impl StagedRobotAssets {
    #[must_use]
    pub fn new(project_root: &Path, execution: ExecutionId) -> Self {
        Self {
            execution,
            asset_root: project_root
                .join("assets")
                .join("robots")
                .join(execution.to_string()),
            texture_root: project_root
                .join(".phoxal")
                .join("textures")
                .join("robots")
                .join(execution.to_string()),
        }
    }

    pub async fn stage(&self, assets: &BTreeMap<AssetId, Vec<u8>>) -> Result<()> {
        let result = self.stage_inner(assets).await;
        match result {
            Ok(()) => Ok(()),
            Err(error) => match self.cleanup().await {
                Ok(()) => Err(error),
                Err(cleanup) => {
                    Err(error.context(format!("staged asset cleanup was incomplete: {cleanup:#}")))
                }
            },
        }
    }

    async fn stage_inner(&self, assets: &BTreeMap<AssetId, Vec<u8>>) -> Result<()> {
        tokio::fs::create_dir_all(&self.asset_root)
            .await
            .with_context(|| format!("failed to create {}", self.asset_root.display()))?;
        for (id, bytes) in assets {
            let target = self.asset_root.join(id.as_str());
            ensure!(
                target.starts_with(&self.asset_root),
                "asset {id} escapes Robot staging"
            );
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&target, bytes)
                .await
                .with_context(|| format!("failed to stage Robot asset {}", target.display()))?;
            if Path::new(id.as_str())
                .extension()
                .and_then(std::ffi::OsStr::to_str)
                == Some("glb")
            {
                let decoded = DecodedMesh::decode(bytes)
                    .with_context(|| format!("failed to decode staged Robot GLB {id}"))?;
                stage_decoded_images(&self.texture_root, id.as_str(), &decoded)?;
            }
        }
        Ok(())
    }

    /// Attempt both independent roots before returning an aggregate failure.
    pub async fn cleanup(&self) -> Result<()> {
        let mut failures = Vec::new();
        for root in [&self.asset_root, &self.texture_root] {
            match tokio::fs::remove_dir_all(root).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => failures.push(format!("{}: {error}", root.display())),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(
                "failed to remove staged Robot assets for {}: {}",
                self.execution,
                failures.join("; ")
            )
        }
    }
}
