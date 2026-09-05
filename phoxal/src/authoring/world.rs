//! Compilation of one explicit `world.yaml` into a closed [`WorldBundle`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::authoring::normalized::{World as NormalizedWorld, WorldGeometry};
use crate::model::asset::AssetId;
use crate::model::geometry::Geometry;
use crate::model::identity::{EntityDeclarationId, SpawnId, WorldAssetName, WorldId};
use crate::model::world::{WorldBundle, WorldBundleError, compiled_entity, compiled_world};

/// Compile one explicit `world.yaml` path.
///
/// The file's parent is the only source root.
/// Meshes are read once during compilation, validated as closed GLB files, deduplicated by bytes, and replaced by canonical [`AssetId`] values.
///
/// # Errors
///
/// Returns [`WorldCompileError`] when the path, source document, mesh closure, or canonical bundle is invalid.
pub fn compile(path: impl AsRef<Path>) -> Result<WorldBundle, WorldCompileError> {
    let supplied = path.as_ref();
    if supplied.file_name().and_then(|name| name.to_str()) != Some("world.yaml") {
        return Err(WorldCompileError::ExplicitWorldPath(supplied.to_path_buf()));
    }
    let metadata = std::fs::symlink_metadata(supplied).map_err(|source| WorldCompileError::Io {
        path: supplied.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        return Err(WorldCompileError::SymlinkWorld(supplied.to_path_buf()));
    }
    let source = supplied
        .canonicalize()
        .map_err(|source| WorldCompileError::Io {
            path: supplied.to_path_buf(),
            source,
        })?;
    let root = source
        .parent()
        .ok_or_else(|| WorldCompileError::ExplicitWorldPath(source.clone()))?
        .to_path_buf();
    let authored = crate::authoring::source::world::Manifest::load(&source)
        .map_err(WorldCompileError::Source)?
        .normalize();
    compile_manifest(&root, authored)
}

fn compile_manifest(
    root: &Path,
    authored: NormalizedWorld,
) -> Result<WorldBundle, WorldCompileError> {
    let referenced = authored
        .entities
        .values()
        .map(|entity| entity.asset.as_str())
        .collect::<BTreeSet<_>>();
    let mut bytes_by_id = BTreeMap::new();
    let mut assets = BTreeMap::new();
    for name in referenced {
        let authored_asset = authored
            .assets
            .get(name)
            .ok_or_else(|| WorldCompileError::UnknownAsset(name.to_owned()))?;
        let asset_name = WorldAssetName::new(name)?;
        let geometry = compile_geometry(root, &authored_asset.geometry, &mut bytes_by_id)?;
        let collision = compile_geometry(root, &authored_asset.collision, &mut bytes_by_id)?;
        assets.insert(asset_name, (geometry, collision));
    }

    let mut spawn_points = BTreeMap::new();
    for (name, pose) in authored.spawn_points {
        spawn_points.insert(SpawnId::new(name)?, pose);
    }

    let mut entities = Vec::new();
    for (name, declaration) in authored.entities {
        let declaration_id = EntityDeclarationId::new(name)?;
        let asset_name = WorldAssetName::new(declaration.asset)?;
        let (geometry, collision) = assets
            .get(&asset_name)
            .ok_or_else(|| WorldCompileError::UnknownAsset(asset_name.to_string()))?;
        for (index, instance) in declaration.instances.into_iter().enumerate() {
            entities.push(compiled_entity(
                declaration_id.clone(),
                u32::try_from(index).map_err(|_| WorldCompileError::TooManyInstances)?,
                instance,
                geometry.clone(),
                collision.clone(),
            ));
        }
    }

    let time_step_ns = authored
        .time_step_ms
        .checked_mul(1_000_000)
        .ok_or(WorldCompileError::InvalidTimeStep)?;
    let world = compiled_world(
        WorldId::new(authored.id)?,
        time_step_ns,
        authored.gravity_mps2,
        spawn_points,
        entities,
    );
    WorldBundle::from_compiler(world, bytes_by_id).map_err(WorldCompileError::Bundle)
}

fn compile_geometry(
    root: &Path,
    authored: &WorldGeometry,
    assets: &mut BTreeMap<AssetId, Vec<u8>>,
) -> Result<Geometry, WorldCompileError> {
    Ok(match authored {
        WorldGeometry::Primitive(geometry) => geometry.clone(),
        WorldGeometry::Mesh { path, scale } => {
            let source = fenced_mesh(root, path)?;
            let bytes = std::fs::read(&source).map_err(|source_error| WorldCompileError::Io {
                path: source.clone(),
                source: source_error,
            })?;
            validate_glb(&source, &bytes)?;
            let digest = Sha256::digest(&bytes);
            let id = AssetId::new(format!("sha256/{digest:x}.glb"))?;
            if let Some(existing) = assets.get(&id) {
                if existing != &bytes {
                    return Err(WorldCompileError::DigestCollision(id));
                }
            } else {
                assets.insert(id.clone(), bytes);
            }
            Geometry::Mesh {
                asset: id,
                scale: *scale,
            }
        }
    })
}

fn fenced_mesh(root: &Path, relative: &Path) -> Result<PathBuf, WorldCompileError> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(WorldCompileError::EscapedMesh(relative.to_path_buf()));
    }
    let mut cursor = root.to_path_buf();
    for component in relative.components() {
        if let Component::Normal(segment) = component {
            cursor.push(segment);
            let metadata =
                std::fs::symlink_metadata(&cursor).map_err(|source| WorldCompileError::Io {
                    path: cursor.clone(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                return Err(WorldCompileError::SymlinkMesh(cursor));
            }
        }
    }
    let canonical = cursor
        .canonicalize()
        .map_err(|source| WorldCompileError::Io {
            path: cursor.clone(),
            source,
        })?;
    if !canonical.starts_with(root) {
        return Err(WorldCompileError::EscapedMesh(relative.to_path_buf()));
    }
    Ok(canonical)
}

fn validate_glb(path: &Path, bytes: &[u8]) -> Result<(), WorldCompileError> {
    crate::model::world::validate_closed_glb(bytes).map_err(|source| WorldCompileError::Glb {
        path: path.to_path_buf(),
        detail: source.to_string(),
    })
}

/// A world source that cannot become one closed deterministic bundle.
#[derive(Debug, thiserror::Error)]
pub enum WorldCompileError {
    #[error("simulation requires one explicit path named world.yaml, got {}", .0.display())]
    ExplicitWorldPath(PathBuf),
    #[error("failed to read world source {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read world.yaml: {0}")]
    Source(#[source] crate::authoring::source::SourceError),
    #[error("world references unknown asset '{0}'")]
    UnknownAsset(String),
    #[error("world contains too many instances in one declaration")]
    TooManyInstances,
    #[error("world time step cannot be represented in nanoseconds")]
    InvalidTimeStep,
    #[error("mesh path escapes the world source root: {}", .0.display())]
    EscapedMesh(PathBuf),
    #[error("world document is a forbidden symlink: {}", .0.display())]
    SymlinkWorld(PathBuf),
    #[error("mesh path contains a forbidden symlink: {}", .0.display())]
    SymlinkMesh(PathBuf),
    #[error("GLB {} is not self-contained and supported: {detail}", path.display())]
    Glb { path: PathBuf, detail: String },
    #[error("two different mesh byte sequences produced the same asset id '{0:?}'")]
    DigestCollision(AssetId),
    #[error("invalid canonical identity: {0}")]
    Model(#[from] crate::model::ModelError),
    #[error("failed to build canonical world bundle: {0}")]
    Bundle(#[source] WorldBundleError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glb(json: &str) -> Vec<u8> {
        glb_with_bin(json, None)
    }

    fn glb_with_bin(json: &str, binary: Option<&[u8]>) -> Vec<u8> {
        let mut json = json.as_bytes().to_vec();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut binary = binary.map(<[u8]>::to_vec);
        if let Some(binary) = &mut binary {
            while !binary.len().is_multiple_of(4) {
                binary.push(0);
            }
        }
        let mut total = 12_u32
            .checked_add(8)
            .and_then(|length| length.checked_add(u32::try_from(json.len()).unwrap()))
            .unwrap();
        if let Some(binary) = &binary {
            total = total
                .checked_add(8)
                .and_then(|length| length.checked_add(u32::try_from(binary.len()).unwrap()))
                .unwrap();
        }
        let mut bytes = Vec::with_capacity(total as usize);
        bytes.extend_from_slice(b"glTF");
        bytes.extend_from_slice(&2_u32.to_le_bytes());
        bytes.extend_from_slice(&total.to_le_bytes());
        bytes.extend_from_slice(&(json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&0x4E4F_534A_u32.to_le_bytes());
        bytes.extend_from_slice(&json);
        if let Some(binary) = binary {
            bytes.extend_from_slice(&(binary.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&0x004E_4942_u32.to_le_bytes());
            bytes.extend_from_slice(&binary);
        }
        bytes
    }

    fn write_world(root: &Path, body: &str) -> PathBuf {
        let path = root.join("world.yaml");
        std::fs::write(&path, body).unwrap();
        path
    }

    fn rewrite_bundle_asset(root: &Path, old: &AssetId, bytes: &[u8]) {
        let replacement = format!("sha256/{:x}.glb", Sha256::digest(bytes));
        let old_path = root.join("assets").join(old.as_str());
        let replacement_path = root.join("assets").join(&replacement);
        std::fs::rename(&old_path, &replacement_path).unwrap();
        std::fs::write(&replacement_path, bytes).unwrap();

        fn replace(value: &mut serde_json::Value, old: &str, replacement: &str) -> usize {
            match value {
                serde_json::Value::String(value) if value == old => {
                    *value = replacement.to_owned();
                    1
                }
                serde_json::Value::Array(values) => values
                    .iter_mut()
                    .map(|value| replace(value, old, replacement))
                    .sum(),
                serde_json::Value::Object(values) => values
                    .values_mut()
                    .map(|value| replace(value, old, replacement))
                    .sum(),
                _ => 0,
            }
        }

        let document_path = root.join("world.json");
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&document_path).unwrap()).unwrap();
        assert!(replace(&mut document, old.as_str(), &replacement) > 0);
        std::fs::write(document_path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    }

    fn primitive(collision: &str) -> String {
        format!(
            r#"schema: phoxal/world/v0
assets:
  floor:
    geometry: {{ kind: box, size: [10.0, 10.0, 0.1] }}
{collision}
world:
  id: warehouse
  time_step_ms: 12
  gravity_mps2: [-0.0, 0.0, -9.81]
  spawn_points:
    bay: {{ xyz: [0.0, 0.0, 0.0], rpy: [0.0, 0.0, 0.0] }}
  entities:
    floor:
      asset: floor
      instances:
        - pose: {{ xyz: [0.0, 0.0, -0.05], rpy: [0.0, 0.0, 0.0] }}
"#
        )
    }

    fn mesh_world(visual: &str, collision: &str) -> String {
        format!(
            r#"schema: phoxal/world/v0
assets:
  tree:
    geometry: {{ kind: mesh, path: {visual}, scale: [1.0, 2.0, 3.0] }}
{collision}
world:
  id: woodland
  time_step_ms: 24
  gravity_mps2: [0.0, 0.0, -9.81]
  entities:
    forest:
      asset: tree
      instances:
        - pose: {{ xyz: [1.0, 2.0, 0.0], rpy: [0.0, 0.0, 0.0] }}
        - pose: {{ xyz: [4.0, 8.0, 0.0], rpy: [0.0, 0.0, 1.2] }}
"#
        )
    }

    #[test]
    fn primitive_world_compiles_deterministically_and_expands_collision_default() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let implicit = compile(write_world(first.path(), &primitive(""))).unwrap();
        let explicit = compile(write_world(
            second.path(),
            &primitive("    collision: { kind: box, size: [10.0, 10.0, 0.1] }"),
        ))
        .unwrap();
        assert_eq!(implicit.digest(), explicit.digest());
        assert_eq!(implicit.assets().len(), 0);
        assert_eq!(
            implicit.digest().to_string(),
            "965dfa661626eb06304f638aec7b915b079ba3838558a25d120e36dbd0039339"
        );
        let entity = implicit.world().entities().next().unwrap();
        assert_eq!(entity.geometry(), entity.collision());
        assert_eq!(implicit.world().time_step_ns(), 12_000_000);
        assert_eq!(
            implicit.world().gravity_mps2()[0].to_bits(),
            0.0_f64.to_bits()
        );
    }

    #[test]
    fn bundle_round_trip_preserves_digest() {
        let source = tempfile::tempdir().unwrap();
        let bundle = compile(write_world(source.path(), &primitive(""))).unwrap();
        let target_parent = tempfile::tempdir().unwrap();
        let target = target_parent.path().join("compiled");
        bundle.write(&target).unwrap();
        let reopened = WorldBundle::open(&target).unwrap();
        assert_eq!(reopened.digest(), bundle.digest());
        assert_eq!(reopened.canonical_archive(), bundle.canonical_archive());
    }

    #[test]
    fn changed_world_bytes_change_the_digest() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let one = compile(write_world(first.path(), &primitive(""))).unwrap();
        let two = compile(write_world(
            second.path(),
            &primitive("").replace("time_step_ms: 12", "time_step_ms: 24"),
        ))
        .unwrap();
        assert_ne!(one.digest(), two.digest());
    }

    #[test]
    fn self_contained_glb_paths_become_deduplicated_asset_ids() {
        let source = tempfile::tempdir().unwrap();
        let assets = source.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        let mesh = glb(r#"{"asset":{"version":"2.0"}}"#);
        std::fs::write(assets.join("tree.glb"), &mesh).unwrap();
        std::fs::write(assets.join("tree-collision.glb"), &mesh).unwrap();
        let authored = mesh_world(
            "assets/tree.glb",
            "    collision: { kind: mesh, path: assets/tree-collision.glb }",
        );

        let bundle = compile(write_world(source.path(), &authored)).unwrap();

        assert_eq!(bundle.assets().len(), 1, "equal bytes are stored once");
        let (asset, stored) = bundle.assets().next().unwrap();
        assert_eq!(stored, mesh);
        assert_eq!(
            asset.as_str(),
            format!("sha256/{:x}.glb", Sha256::digest(&mesh))
        );
        let entities = bundle.world().entities().collect::<Vec<_>>();
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].declaration().as_str(), "forest");
        assert_eq!(entities[0].instance(), 0);
        assert_eq!(entities[1].instance(), 1);
        assert_eq!(entities[0].geometry().asset_id(), Some(asset));
        assert_eq!(entities[0].collision().asset_id(), Some(asset));
    }

    #[test]
    fn committed_glb_acceptance_world_covers_implicit_and_overridden_collision() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixture/world.yaml");
        let bundle = compile(source).expect("committed GLB world compiles");
        assert_eq!(
            bundle.assets().len(),
            1,
            "the shared GLB is bundled exactly once"
        );
        let entities = bundle.world().entities().collect::<Vec<_>>();
        assert_eq!(entities.len(), 3);

        let implicit = entities
            .iter()
            .find(|entity| entity.declaration() == "implicit-mesh")
            .expect("implicit collision entity");
        assert_eq!(implicit.geometry(), implicit.collision());
        assert!(implicit.geometry().asset_id().is_some());

        let overridden = entities
            .iter()
            .find(|entity| entity.declaration() == "detailed-visual")
            .expect("explicit collision entity");
        assert!(overridden.geometry().asset_id().is_some());
        assert!(matches!(
            overridden.collision(),
            crate::model::geometry::Geometry::Box { .. }
        ));
    }

    #[test]
    fn external_glb_resources_are_rejected() {
        let source = tempfile::tempdir().unwrap();
        let assets = source.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(
            assets.join("tree.glb"),
            glb(r#"{"asset":{"version":"2.0"},"buffers":[{"uri":"tree.bin","byteLength":4}]}"#),
        )
        .unwrap();
        let error = compile(write_world(
            source.path(),
            &mesh_world("assets/tree.glb", ""),
        ))
        .unwrap_err();
        assert!(matches!(error, WorldCompileError::Glb { .. }));
        assert!(error.to_string().contains("external URI 'tree.bin'"));
    }

    #[test]
    fn embedded_glb_buffer_requires_one_covering_binary_chunk() {
        let invalid = [
            glb(r#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":4}]}"#),
            glb_with_bin(
                r#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":8}]}"#,
                Some(&[1, 2, 3, 4]),
            ),
            glb_with_bin(r#"{"asset":{"version":"2.0"}}"#, Some(&[1, 2, 3, 4])),
        ];
        for mesh in invalid {
            let source = tempfile::tempdir().unwrap();
            let assets = source.path().join("assets");
            std::fs::create_dir(&assets).unwrap();
            std::fs::write(assets.join("tree.glb"), mesh).unwrap();

            let error = compile(write_world(
                source.path(),
                &mesh_world("assets/tree.glb", ""),
            ))
            .unwrap_err();
            assert!(matches!(error, WorldCompileError::Glb { .. }));
        }

        let source = tempfile::tempdir().unwrap();
        let assets = source.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(
            assets.join("tree.glb"),
            glb_with_bin(
                r#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":3}]}"#,
                Some(&[1, 2, 3]),
            ),
        )
        .unwrap();
        compile(write_world(
            source.path(),
            &mesh_world("assets/tree.glb", ""),
        ))
        .unwrap();
    }

    #[test]
    fn malformed_glb_chunks_are_rejected_during_world_compilation() {
        let source = tempfile::tempdir().unwrap();
        let assets = source.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        let mut mesh = glb(r#"{"asset":{"version":"2.0"}}"#);
        mesh.extend_from_slice(&[4, 0, 0, 0]);
        let declared = u32::try_from(mesh.len()).unwrap().to_le_bytes();
        mesh[8..12].copy_from_slice(&declared);
        std::fs::write(assets.join("tree.glb"), mesh).unwrap();

        let error = compile(write_world(
            source.path(),
            &mesh_world("assets/tree.glb", ""),
        ))
        .unwrap_err();
        assert!(matches!(error, WorldCompileError::Glb { .. }));
        assert!(error.to_string().contains("truncated GLB chunk header"));
    }

    #[test]
    fn reopened_bundle_rejects_asset_bytes_that_do_not_match_their_id() {
        let source = tempfile::tempdir().unwrap();
        let assets = source.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(
            assets.join("tree.glb"),
            glb(r#"{"asset":{"version":"2.0"}}"#),
        )
        .unwrap();
        let bundle = compile(write_world(
            source.path(),
            &mesh_world("assets/tree.glb", ""),
        ))
        .unwrap();
        let output_parent = tempfile::tempdir().unwrap();
        let output = output_parent.path().join("bundle");
        bundle.write(&output).unwrap();
        let asset = bundle.assets().next().unwrap().0;
        std::fs::write(output.join("assets").join(asset.as_str()), b"corrupt").unwrap();

        assert!(matches!(
            WorldBundle::open(&output),
            Err(WorldBundleError::AssetDigestMismatch { .. })
        ));
    }

    #[test]
    fn reopened_bundle_revalidates_closed_glb_bytes_after_digest_consistency() {
        for invalid in [
            b"not a glb".to_vec(),
            glb(r#"{"asset":{"version":"2.0"},"images":[{"uri":"texture.png"}]}"#),
            glb(r#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":4}]}"#),
            glb_with_bin(
                r#"{"asset":{"version":"2.0"},"buffers":[{"byteLength":8}]}"#,
                Some(&[1, 2, 3, 4]),
            ),
            glb_with_bin(r#"{"asset":{"version":"2.0"}}"#, Some(&[1, 2, 3, 4])),
        ] {
            let source = tempfile::tempdir().unwrap();
            let assets = source.path().join("assets");
            std::fs::create_dir(&assets).unwrap();
            std::fs::write(
                assets.join("tree.glb"),
                glb(r#"{"asset":{"version":"2.0"}}"#),
            )
            .unwrap();
            let bundle = compile(write_world(
                source.path(),
                &mesh_world("assets/tree.glb", ""),
            ))
            .unwrap();
            let old = bundle.assets().next().unwrap().0.clone();
            let output_parent = tempfile::tempdir().unwrap();
            let output = output_parent.path().join("bundle");
            bundle.write(&output).unwrap();
            rewrite_bundle_asset(&output, &old, &invalid);

            let error = WorldBundle::open(&output).unwrap_err();
            assert!(matches!(error, WorldBundleError::InvalidAsset { .. }));
        }
    }

    #[test]
    fn reopened_bundle_rejects_noncanonical_root_entries_and_negative_zero() {
        let source = tempfile::tempdir().unwrap();
        let bundle = compile(write_world(source.path(), &primitive(""))).unwrap();

        let extra_parent = tempfile::tempdir().unwrap();
        let extra = extra_parent.path().join("bundle");
        bundle.write(&extra).unwrap();
        std::fs::write(extra.join("unexpected"), b"not part of the bundle").unwrap();
        assert!(matches!(
            WorldBundle::open(&extra),
            Err(WorldBundleError::Invalid(_))
        ));

        let negative_parent = tempfile::tempdir().unwrap();
        let negative = negative_parent.path().join("bundle");
        bundle.write(&negative).unwrap();
        let document_path = negative.join("world.json");
        let document = std::fs::read_to_string(&document_path).unwrap();
        let document = document.replacen("0.0", "-0.0", 1);
        std::fs::write(&document_path, document).unwrap();
        assert!(matches!(
            WorldBundle::open(&negative),
            Err(WorldBundleError::Invalid(_))
        ));
    }

    #[test]
    fn compilation_requires_an_explicit_world_yaml_path() {
        let source = tempfile::tempdir().unwrap();
        let path = source.path().join("scene.yaml");
        std::fs::write(&path, primitive("")).unwrap();

        assert!(matches!(
            compile(&path),
            Err(WorldCompileError::ExplicitWorldPath(rejected)) if rejected == path
        ));
    }

    #[cfg(unix)]
    #[test]
    fn compilation_refuses_a_symlinked_world_document() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().unwrap();
        let target = source.path().join("authored.yaml");
        std::fs::write(&target, primitive("")).unwrap();
        let path = source.path().join("world.yaml");
        symlink(&target, &path).unwrap();

        assert!(matches!(
            compile(&path),
            Err(WorldCompileError::SymlinkWorld(rejected)) if rejected == path
        ));
    }

    #[cfg(unix)]
    #[test]
    fn compilation_refuses_mesh_symlinks_even_inside_the_source_root() {
        use std::os::unix::fs::symlink;

        let source = tempfile::tempdir().unwrap();
        let assets = source.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(
            assets.join("real.glb"),
            glb(r#"{"asset":{"version":"2.0"}}"#),
        )
        .unwrap();
        symlink("real.glb", assets.join("linked.glb")).unwrap();

        let error = compile(write_world(
            source.path(),
            &mesh_world("assets/linked.glb", ""),
        ))
        .unwrap_err();
        assert!(matches!(error, WorldCompileError::SymlinkMesh(_)));
    }
}
