//! Robot-only Wavefront decoding from the already fetched bundle.
//!
//! World authoring remains GLB-only. Existing URDF robots use triangle OBJ meshes with
//! diffuse MTL materials; no path is ever opened by the Wavefront loader.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use phoxal::model::AssetId;

use crate::glb::{DecodedMaterial, DecodedMesh, DecodedPrimitive};

pub(crate) fn material_dependencies(asset: &AssetId, bytes: &[u8]) -> Result<Vec<AssetId>> {
    if Path::new(asset.as_str())
        .extension()
        .is_none_or(|ext| ext != "obj")
    {
        return Ok(Vec::new());
    }
    let mut dependencies = std::collections::BTreeSet::new();
    for line in std::str::from_utf8(bytes)?.lines() {
        let mut fields = line
            .split('#')
            .next()
            .unwrap_or_default()
            .split_whitespace();
        if fields.next() == Some("mtllib") {
            let path = fields.next().context("OBJ mtllib needs a material path")?;
            ensure!(
                fields.next().is_none(),
                "OBJ needs one material path per mtllib"
            );
            dependencies.insert(material_asset(asset, Path::new(path))?);
        }
    }
    Ok(dependencies.into_iter().collect())
}

pub(crate) fn decode(asset: &AssetId, assets: &BTreeMap<AssetId, Vec<u8>>) -> Result<DecodedMesh> {
    let bytes = assets
        .get(asset)
        .with_context(|| format!("missing mesh {asset}"))?;
    if bytes.starts_with(b"glTF") {
        return DecodedMesh::decode(bytes);
    }
    ensure!(
        Path::new(asset.as_str())
            .extension()
            .is_some_and(|ext| ext == "obj"),
        "Robot mesh {asset} must be GLB or triangle OBJ"
    );
    let source = std::str::from_utf8(bytes).context("OBJ is not UTF-8")?;
    for line in source.lines() {
        let fields: Vec<_> = line
            .split('#')
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .collect();
        let Some(kind) = fields.first() else { continue };
        match *kind {
            "v" | "vn" => ensure!(
                fields.len() == 4,
                "OBJ {kind} needs exactly three coordinates"
            ),
            "vt" => ensure!(
                (2..=4).contains(&fields.len())
                    && fields[1..]
                        .iter()
                        .all(|field| field.parse::<f64>().is_ok_and(f64::is_finite)),
                "OBJ vt needs one to three finite coordinates"
            ),
            "f" => ensure!(fields.len() == 4, "OBJ supports triangle faces only"),
            "mtllib" => ensure!(fields.len() == 2, "OBJ needs one material path per mtllib"),
            "o" | "g" | "s" | "usemtl" => {}
            _ => bail!("unsupported OBJ statement {kind}"),
        }
    }
    let (models, materials) = tobj::load_obj_buf(
        &mut Cursor::new(bytes),
        &tobj::LoadOptions {
            single_index: true,
            ..Default::default()
        },
        |path| {
            let material = material_asset(asset, path).map_err(|_| tobj::LoadError::ReadError)?;
            let bytes = assets.get(&material).ok_or(tobj::LoadError::ReadError)?;
            validate_material(bytes).map_err(|_| tobj::LoadError::MaterialParseError)?;
            tobj::load_mtl_buf(&mut Cursor::new(bytes))
        },
    )
    .context("invalid OBJ geometry")?;
    let materials = materials
        .context("OBJ material is missing, escapes its directory, or uses unsupported fields")?;
    for line in source.lines() {
        let mut fields = line
            .split('#')
            .next()
            .unwrap_or_default()
            .split_whitespace();
        if fields.next() == Some("usemtl") {
            let name = fields.next().context("OBJ usemtl needs a material name")?;
            ensure!(
                fields.next().is_none() && materials.iter().any(|material| material.name == name),
                "OBJ names unknown material {name}"
            );
        }
    }
    let mut primitives = Vec::new();
    for model in models {
        let mesh = model.mesh;
        ensure!(
            !mesh.positions.is_empty() && mesh.positions.len().is_multiple_of(3),
            "OBJ has no complete positions"
        );
        ensure!(
            mesh.positions.iter().all(|v| v.is_finite()),
            "OBJ positions must be finite"
        );
        let positions = mesh.positions.as_chunks::<3>().0.to_vec();
        ensure!(
            !mesh.indices.is_empty()
                && mesh.indices.len().is_multiple_of(3)
                && mesh.indices.iter().all(|&i| (i as usize) < positions.len()),
            "OBJ indices must describe valid triangles"
        );
        let normals = if mesh.normals.is_empty() {
            None
        } else {
            ensure!(
                mesh.normals.len() == positions.len() * 3,
                "OBJ normals must cover every vertex"
            );
            let normals = mesh.normals.as_chunks::<3>().0.to_vec();
            ensure!(
                normals.iter().all(|v| {
                    let length = v.iter().map(|x| x * x).sum::<f64>();
                    length.is_finite() && length > 0.0
                }),
                "OBJ normals must have finite nonzero length"
            );
            Some(normals)
        };
        ensure!(
            mesh.texcoords.iter().all(|v| v.is_finite()),
            "OBJ texture coordinates must be finite"
        );
        let color = mesh
            .material_id
            .map(|index| {
                materials
                    .get(index)
                    .context("OBJ names an unknown material")?
                    .diffuse
                    .context("OBJ material needs a diffuse color")
            })
            .transpose()?
            .unwrap_or([1.0; 3]);
        ensure!(
            color
                .iter()
                .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
            "OBJ diffuse color must be in [0, 1]"
        );
        primitives.push(DecodedPrimitive {
            positions,
            normals,
            texcoords: None,
            indices: mesh.indices,
            material: DecodedMaterial {
                base_color: [color[0], color[1], color[2], 1.0],
                metallic: 0.0,
                ..Default::default()
            },
        });
    }
    ensure!(!primitives.is_empty(), "OBJ contains no mesh");
    Ok(DecodedMesh {
        primitives,
        images: Vec::new(),
    })
}

fn material_asset(mesh: &AssetId, relative: &Path) -> Result<AssetId> {
    let relative = relative.to_str().context("MTL path is not UTF-8")?;
    // AssetId forbids absolute paths, traversal, and platform separators.
    let relative = AssetId::new(relative)?;
    let parent = Path::new(mesh.as_str())
        .parent()
        .context("mesh has no parent")?;
    AssetId::new(
        parent
            .join(relative.as_str())
            .to_str()
            .context("MTL path is not UTF-8")?,
    )
    .map_err(Into::into)
}

fn validate_material(bytes: &[u8]) -> Result<()> {
    for line in std::str::from_utf8(bytes)?.lines() {
        let fields: Vec<_> = line
            .split('#')
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .collect();
        let Some(kind) = fields.first() else { continue };
        match *kind {
            "newmtl" => ensure!(fields.len() == 2, "MTL requires a single material name"),
            "Kd" => ensure!(fields.len() == 4, "MTL requires three diffuse channels"),
            _ => bail!("unsupported MTL statement {kind}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(source: &str, material: &str) -> (AssetId, BTreeMap<AssetId, Vec<u8>>) {
        let mesh = AssetId::new("robot/meshes/test.obj").expect("mesh id");
        let mtl = AssetId::new("robot/meshes/test.mtl").expect("material id");
        (
            mesh.clone(),
            BTreeMap::from([
                (mesh, source.as_bytes().to_vec()),
                (mtl, material.as_bytes().to_vec()),
            ]),
        )
    }

    const TRIANGLE: &str = "mtllib test.mtl\nv 0 0 0\nv 1 0 0\nv 0 1 0\nusemtl paint\nf 1 2 3\n";

    #[test]
    fn robot_obj_preserves_coordinates_and_diffuse_material() {
        let (id, assets) = fixture(TRIANGLE, "newmtl paint\nKd 0.2 0.4 0.8\n");
        let mesh = decode(&id, &assets).expect("closed OBJ");
        assert_eq!(
            material_dependencies(&id, &assets[&id]).expect("closure"),
            vec![AssetId::new("robot/meshes/test.mtl").expect("material id")]
        );
        assert_eq!(
            mesh.primitives[0].positions,
            [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]
        );
        assert_eq!(mesh.primitives[0].material.base_color, [0.2, 0.4, 0.8, 1.0]);
        mesh.validate_collision()
            .expect("exact collision triangles");
    }

    #[test]
    fn obj_refuses_missing_escaping_and_unsupported_materials() {
        for reference in [
            "missing.mtl",
            "../test.mtl",
            "/tmp/test.mtl",
            "https://example.com/test.mtl",
        ] {
            let (id, assets) = fixture(
                &TRIANGLE.replace("test.mtl", reference),
                "newmtl paint\nKd 1 1 1\n",
            );
            assert!(decode(&id, &assets).is_err(), "{reference}");
        }
        let (id, assets) = fixture(TRIANGLE, "newmtl paint\nKd 1 1 1\nmap_Kd outside.png\n");
        assert!(decode(&id, &assets).is_err());
    }

    #[test]
    fn obj_refuses_invalid_geometry_and_material_values() {
        for source in [
            TRIANGLE.replace("v 1 0 0", "v NaN 0 0"),
            TRIANGLE.replace("f 1 2 3", "f 1 2 9"),
            TRIANGLE.replace("f 1 2 3", "f 1 2 3 1"),
        ] {
            let (id, assets) = fixture(&source, "newmtl paint\nKd 1 1 1\n");
            assert!(decode(&id, &assets).is_err());
        }
        let (id, assets) = fixture(TRIANGLE, "newmtl paint\nKd 2 1 1\n");
        assert!(decode(&id, &assets).is_err());
    }

    #[test]
    fn official_wheel_obj_preserves_its_native_robot_coordinates() {
        let id = AssetId::new("components/ddsm115/meshes/ddsm115.obj").expect("mesh id");
        let mtl = AssetId::new("components/ddsm115/meshes/motorized_wheel.mtl").expect("MTL id");
        let assets = BTreeMap::from([
            (
                id.clone(),
                include_bytes!("../../../../components/ddsm115/meshes/ddsm115.obj").to_vec(),
            ),
            (
                mtl,
                include_bytes!("../../../../components/ddsm115/meshes/motorized_wheel.mtl")
                    .to_vec(),
            ),
        ]);
        let mesh = decode(&id, &assets).expect("official wheel decodes");
        assert_eq!(mesh.primitives.len(), 3);
        let low_y = mesh
            .primitives
            .iter()
            .flat_map(|p| &p.positions)
            .map(|p| p[1])
            .fold(f64::INFINITY, f64::min);
        assert_eq!(low_y, -0.099);
    }
}
