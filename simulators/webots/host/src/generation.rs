//! Deterministic, self-contained Webots project generation from one compiled world bundle.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use phoxal::model::asset::AssetId;
use phoxal::model::geometry::Geometry;
use phoxal::model::structure::Pose;
use phoxal::model::world::WorldBundle;

use crate::glb::DecodedMesh;
use crate::{ROBOT_CONTROLLER_PACKAGE, WORLD_CONTROLLER_PACKAGE};

/// Exact native executables copied into a generated Webots project.
#[derive(Clone, Debug)]
pub struct ControllerExecutables {
    pub world: PathBuf,
    pub robot: PathBuf,
}

/// Paths of one fully staged Webots project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedProject {
    root: PathBuf,
    world: PathBuf,
}

impl GeneratedProject {
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn world(&self) -> &Path {
        &self.world
    }
}

struct DecodedWorldAssets {
    assets: BTreeMap<AssetId, DecodedMesh>,
}

impl DecodedWorldAssets {
    fn decode(bundle: &WorldBundle) -> Result<Self> {
        Self::decode_with(bundle, |_, bytes| DecodedMesh::decode(bytes))
    }

    fn decode_with(
        bundle: &WorldBundle,
        mut decode: impl FnMut(&AssetId, &[u8]) -> Result<DecodedMesh>,
    ) -> Result<Self> {
        let collision_assets = bundle
            .world()
            .entities()
            .filter_map(|entity| entity.collision().asset_id())
            .collect::<BTreeSet<_>>();
        let mut assets = BTreeMap::new();
        for (id, bytes) in bundle.assets() {
            ensure!(
                Path::new(id.as_str())
                    .extension()
                    .and_then(std::ffi::OsStr::to_str)
                    == Some("glb"),
                "Webots world mesh '{}' must be an embedded GLB asset",
                id
            );
            let decoded =
                decode(id, bytes).with_context(|| format!("invalid embedded GLB asset '{id}'"))?;
            if collision_assets.contains(id) {
                decoded.validate_collision().with_context(|| {
                    format!("GLB collision asset '{id}' exceeds the accepted subset")
                })?;
            }
            let previous = assets.insert(id.clone(), decoded);
            debug_assert!(previous.is_none(), "WorldBundle asset ids are distinct");
        }
        Ok(Self { assets })
    }

    fn get(&self, id: &AssetId) -> Result<&DecodedMesh> {
        self.assets
            .get(id)
            .with_context(|| format!("world mesh asset {id} is absent from the decoded cache"))
    }
}

/// Stage one deterministic project into a new, empty directory.
///
/// Generated projects contain no `EXTERNPROTO` declarations and never resolve network content.
pub fn stage_project(
    bundle: &WorldBundle,
    root: impl AsRef<Path>,
    host_connect: &str,
    controllers: &ControllerExecutables,
) -> Result<GeneratedProject> {
    let root = root.as_ref();
    ensure!(
        !root.exists(),
        "generated Webots project already exists at {}",
        root.display()
    );
    validate_endpoint(host_connect)?;
    let decoded_assets = DecodedWorldAssets::decode(bundle)?;

    let worlds = root.join("worlds");
    let assets = root.join("assets");
    let textures = root.join(".phoxal").join("textures").join("world");
    std::fs::create_dir_all(&worlds)
        .with_context(|| format!("failed to create {}", worlds.display()))?;
    std::fs::create_dir_all(&assets)
        .with_context(|| format!("failed to create {}", assets.display()))?;

    for (id, bytes) in bundle.assets() {
        let path = assets.join(id.as_str());
        ensure!(
            path.starts_with(&assets),
            "asset '{}' escapes the generated project",
            id
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&path, bytes)
            .with_context(|| format!("failed to stage world asset {}", path.display()))?;
        let decoded = decoded_assets.get(id)?;
        stage_decoded_images(&textures, id.as_str(), decoded)?;
    }

    stage_controller(&controllers.world, root, WORLD_CONTROLLER_PACKAGE)?;
    stage_controller(&controllers.robot, root, ROBOT_CONTROLLER_PACKAGE)?;

    let source = render_world_with_decoded(bundle, host_connect, &decoded_assets)?;
    let _: webots_proto_ast::Proto = source
        .parse()
        .context("generated Webots world did not parse as R2025a VRML")?;
    let world = worlds.join("world.wbt");
    std::fs::write(&world, source.as_bytes())
        .with_context(|| format!("failed to write generated world {}", world.display()))?;
    Ok(GeneratedProject {
        root: root.to_path_buf(),
        world,
    })
}

/// Render stable R2025a source without touching the filesystem.
pub fn render_world(bundle: &WorldBundle, host_connect: &str) -> Result<String> {
    validate_endpoint(host_connect)?;
    let decoded_assets = DecodedWorldAssets::decode(bundle)?;
    render_world_with_decoded(bundle, host_connect, &decoded_assets)
}

fn render_world_with_decoded(
    bundle: &WorldBundle,
    host_connect: &str,
    decoded_assets: &DecodedWorldAssets,
) -> Result<String> {
    let world = bundle.world();
    let quantum_ns = world.time_step_ns();
    ensure!(
        quantum_ns.is_multiple_of(1_000_000),
        "Webots requires time_step_ns to be an exact whole number of milliseconds"
    );
    let quantum_ms = quantum_ns / 1_000_000;
    ensure!(
        i32::try_from(quantum_ms).is_ok() && quantum_ms > 0,
        "Webots basicTimeStep does not fit its positive millisecond range"
    );
    let [gx, gy, gz] = world.gravity_mps2();
    ensure!(
        gx == 0.0 && gy == 0.0 && gz.is_sign_negative(),
        "Webots R2025a ENU generation requires gravity_mps2 [0, 0, negative]"
    );

    let mut out = String::new();
    writeln!(out, "#VRML_SIM R2025a utf8")?;
    writeln!(out)?;
    render_world_info(&mut out, quantum_ms, -gz)?;
    // R2025a Viewpoint uses FLU, unlike the OpenGL -Z camera convention.
    let forward = nalgebra::Vector3::new(-6.0, 6.0, -5.0).normalize();
    let left = nalgebra::Vector3::z().cross(&forward).normalize();
    let up = forward.cross(&left);
    let view = nalgebra::UnitQuaternion::from_matrix(&nalgebra::Matrix3::from_columns(&[
        forward, left, up,
    ]));
    let (axis, angle) = view
        .axis_angle()
        .context("default camera orientation has no axis")?;
    writeln!(out, "Viewpoint {{")?;
    writeln!(
        out,
        "  orientation {} {} {} {}",
        number(axis.x),
        number(axis.y),
        number(axis.z),
        number(angle)
    )?;
    writeln!(out, "  position 6 -6 5")?;
    writeln!(out, "}}")?;
    writeln!(out, "Background {{ skyColor [ 0.12 0.15 0.20 ] }}")?;
    writeln!(
        out,
        "DirectionalLight {{ direction -0.3 -0.5 -1 intensity 1 ambientIntensity 0.4 }}"
    )?;
    writeln!(out, "Robot {{")?;
    writeln!(out, "  name \"__phoxal_world_controller\"")?;
    writeln!(out, "  controller \"{WORLD_CONTROLLER_PACKAGE}\"")?;
    writeln!(
        out,
        "  controllerArgs [\"--host-connect\", \"{}\"]",
        quoted(host_connect)
    )?;
    writeln!(out, "  supervisor TRUE")?;
    writeln!(out, "  synchronization TRUE")?;
    writeln!(out, "}}")?;

    for entity in world.entities() {
        let pose = entity.pose();
        let [x, y, z] = pose.xyz();
        let [ax, ay, az, angle] = axis_angle(pose);
        writeln!(
            out,
            "DEF PHOXAL_{}_{} Solid {{",
            entity
                .declaration()
                .as_str()
                .to_ascii_uppercase()
                .replace('-', "_"),
            entity.instance()
        )?;
        writeln!(
            out,
            "  translation {} {} {}",
            number(x),
            number(y),
            number(z)
        )?;
        writeln!(
            out,
            "  rotation {} {} {} {}",
            number(ax),
            number(ay),
            number(az),
            number(angle)
        )?;
        writeln!(
            out,
            "  name \"{}[{}]\"",
            entity.declaration(),
            entity.instance()
        )?;
        writeln!(out, "  children [")?;
        render_visual(&mut out, decoded_assets, entity.geometry(), 4)?;
        writeln!(out, "  ]")?;
        writeln!(out, "  boundingObject")?;
        render_collision(&mut out, decoded_assets, entity.collision(), 2)?;
        writeln!(out, "  locked TRUE")?;
        writeln!(out, "}}")?;
    }
    ensure!(
        !out.contains("<extern>"),
        "generated source contains the forbidden external controller"
    );
    ensure!(
        !out.contains("EXTERNPROTO"),
        "generated source contains an external PROTO dependency"
    );
    Ok(out)
}

fn render_world_info(out: &mut String, quantum_ms: u64, gravity: f64) -> Result<()> {
    writeln!(out, "WorldInfo {{")?;
    writeln!(out, "  basicTimeStep {quantum_ms}")?;
    writeln!(out, "  coordinateSystem \"ENU\"")?;
    // The authoring schema intentionally has no global georeference yet. Use a deterministic
    // WGS84 origin so every admitted geographic GNSS publishes latitude/longitude/altitude rather
    // than silently relabeling local metres as degrees.
    writeln!(out, "  gpsCoordinateSystem \"WGS84\"")?;
    writeln!(out, "  gpsReference 0 0 0")?;
    writeln!(out, "  gravity {}", number(gravity))?;
    writeln!(out, "  randomSeed 0")?;
    writeln!(out, "}}")?;
    Ok(())
}

fn render_visual(
    out: &mut String,
    decoded_assets: &DecodedWorldAssets,
    geometry: &Geometry,
    indent: usize,
) -> Result<()> {
    match geometry {
        Geometry::Mesh { asset, scale } => {
            let decoded = decoded_assets.get(asset)?;
            if let Some(scale) = scale {
                writeln!(out, "{:indent$}Transform {{", "")?;
                writeln!(
                    out,
                    "{:width$}scale {} {} {}",
                    "",
                    number(scale[0]),
                    number(scale[1]),
                    number(scale[2]),
                    width = indent + 2
                )?;
                writeln!(out, "{:width$}children [", "", width = indent + 2)?;
                render_decoded_visual(out, decoded, asset, indent + 4)?;
                writeln!(out, "{:width$}]", "", width = indent + 2)?;
                writeln!(out, "{:indent$}}}", "")?;
            } else {
                render_decoded_visual(out, decoded, asset, indent)?;
            }
        }
        primitive => {
            writeln!(out, "{:indent$}Shape {{", "")?;
            writeln!(
                out,
                "{:width$}appearance PBRAppearance {{",
                "",
                width = indent + 2
            )?;
            writeln!(
                out,
                "{:width$}baseColor 0.6 0.6 0.6",
                "",
                width = indent + 4
            )?;
            writeln!(out, "{:width$}roughness 0.7", "", width = indent + 4)?;
            writeln!(out, "{:width$}}}", "", width = indent + 2)?;
            writeln!(out, "{:width$}geometry", "", width = indent + 2)?;
            render_primitive(out, primitive, indent + 4)?;
            writeln!(out, "{:indent$}}}", "")?;
        }
    }
    Ok(())
}

fn render_collision(
    out: &mut String,
    decoded_assets: &DecodedWorldAssets,
    geometry: &Geometry,
    indent: usize,
) -> Result<()> {
    match geometry {
        Geometry::Mesh { asset, scale } => {
            let decoded = decoded_assets.get(asset)?;
            if let Some(scale) = scale {
                decoded.render_collision_scaled(out, indent, *scale)?;
            } else {
                decoded.render_collision(out, indent)?;
            }
        }
        primitive => render_primitive(out, primitive, indent)?,
    }
    Ok(())
}

fn render_decoded_visual(
    out: &mut String,
    decoded: &DecodedMesh,
    asset: &AssetId,
    indent: usize,
) -> Result<()> {
    decoded.render_visual(out, indent, |primitive| {
        Ok(format!(
            "../.phoxal/textures/world/{}",
            extracted_image_path(asset.as_str(), primitive)
        ))
    })
}

pub(crate) fn stage_decoded_images(root: &Path, asset: &str, decoded: &DecodedMesh) -> Result<()> {
    for index in 0..decoded.primitives.len() {
        let Some(bytes) = decoded.staged_texture(index)? else {
            continue;
        };
        let relative = extracted_image_path(asset, index);
        let path = root.join(&relative);
        ensure!(
            path.starts_with(root),
            "decoded texture path escapes asset staging"
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&path, bytes)
            .with_context(|| format!("failed to extract GLB texture {}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn extracted_image_path(asset: &str, primitive: usize) -> String {
    format!("{asset}.images/{primitive}.png")
}

fn render_primitive(out: &mut String, geometry: &Geometry, indent: usize) -> Result<()> {
    match geometry {
        Geometry::Box { size } => writeln!(
            out,
            "{:indent$}Box {{ size {} {} {} }}",
            "",
            number(size[0]),
            number(size[1]),
            number(size[2])
        )?,
        Geometry::Cylinder { radius, length } => writeln!(
            out,
            "{:indent$}Cylinder {{ radius {} height {} }}",
            "",
            number(*radius),
            number(*length)
        )?,
        Geometry::Capsule { radius, length } => writeln!(
            out,
            "{:indent$}Capsule {{ radius {} height {} }}",
            "",
            number(*radius),
            number(*length)
        )?,
        Geometry::Sphere { radius } => {
            writeln!(out, "{:indent$}Sphere {{ radius {} }}", "", number(*radius))?
        }
        Geometry::Mesh { .. } => bail!("mesh geometry must use its dedicated renderer"),
    }
    Ok(())
}

fn validate_endpoint(endpoint: &str) -> Result<()> {
    let address = endpoint
        .strip_prefix("tcp://127.0.0.1:")
        .context("private Webots host endpoint must be tcp://127.0.0.1:<port>")?;
    let port: u16 = address
        .parse()
        .context("private Webots host port is invalid")?;
    ensure!(port != 0, "private Webots host port must be nonzero");
    Ok(())
}

fn stage_controller(source: &Path, root: &Path, name: &str) -> Result<()> {
    ensure!(
        source.is_file(),
        "native controller is missing at {}",
        source.display()
    );
    let directory = root.join("controllers").join(name);
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    let target = directory.join(name);
    std::fs::copy(source, &target).with_context(|| {
        format!(
            "failed to stage controller {} as {}",
            source.display(),
            target.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&target)?.permissions();
        permissions.set_mode(permissions.mode() | 0o500);
        std::fs::set_permissions(&target, permissions)?;
    }
    Ok(())
}

pub(crate) fn axis_angle(pose: Pose) -> [f64; 4] {
    let [roll, pitch, yaw] = pose.rpy();
    let (sr, cr) = (roll * 0.5).sin_cos();
    let (sp, cp) = (pitch * 0.5).sin_cos();
    let (sy, cy) = (yaw * 0.5).sin_cos();
    let x = sr * cp * cy - cr * sp * sy;
    let y = cr * sp * cy + sr * cp * sy;
    let z = cr * cp * sy - sr * sp * cy;
    let w = (cr * cp * cy + sr * sp * sy).clamp(-1.0, 1.0);
    let angle = 2.0 * w.acos();
    let length = (x * x + y * y + z * z).sqrt();
    if length <= f64::EPSILON || angle.abs() <= f64::EPSILON {
        [0.0, 0.0, 1.0, 0.0]
    } else {
        [x / length, y / length, z / length, angle]
    }
}

pub(crate) fn number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let mut rendered = format!("{value:.17}");
    while rendered.ends_with('0') {
        rendered.pop();
    }
    if rendered.ends_with('.') {
        rendered.pop();
    }
    rendered
}

pub(crate) fn quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
pub(crate) mod tests {
    use sha2::Digest as _;

    use super::*;

    fn triangle_glb() -> Vec<u8> {
        let mut binary = Vec::new();
        for value in [0.0_f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
            binary.extend_from_slice(&value.to_le_bytes());
        }
        for index in [0_u16, 1, 2] {
            binary.extend_from_slice(&index.to_le_bytes());
        }
        let document = serde_json::json!({
            "asset": { "version": "2.0" },
            "scene": 0,
            "scenes": [{ "nodes": [0] }],
            "nodes": [{ "mesh": 0 }],
            "meshes": [{ "primitives": [{
                "attributes": { "POSITION": 0 },
                "indices": 1
            }] }],
            "buffers": [{ "byteLength": binary.len() }],
            "bufferViews": [
                { "buffer": 0, "byteOffset": 0, "byteLength": 36 },
                { "buffer": 0, "byteOffset": 36, "byteLength": 6 }
            ],
            "accessors": [
                { "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3" },
                { "bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR" }
            ]
        });
        let mut json = serde_json::to_vec(&document).expect("fixture JSON");
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let padded_binary = binary.len().div_ceil(4) * 4;
        let total = 12 + 8 + json.len() + 8 + padded_binary;
        let mut glb = Vec::new();
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2_u32.to_le_bytes());
        glb.extend_from_slice(&u32::try_from(total).expect("fixture length").to_le_bytes());
        glb.extend_from_slice(
            &u32::try_from(json.len())
                .expect("fixture JSON length")
                .to_le_bytes(),
        );
        glb.extend_from_slice(&0x4e4f_534a_u32.to_le_bytes());
        glb.extend_from_slice(&json);
        glb.extend_from_slice(
            &u32::try_from(padded_binary)
                .expect("fixture BIN length")
                .to_le_bytes(),
        );
        glb.extend_from_slice(&0x004e_4942_u32.to_le_bytes());
        glb.extend_from_slice(&binary);
        glb.resize(total, 0);
        glb
    }

    pub(crate) fn compile_mesh_world(visual: &[u8], collision: Option<&[u8]>) -> WorldBundle {
        mesh_world_instances(visual, collision, 1)
    }

    fn mesh_world_instances(visual: &[u8], collision: Option<&[u8]>, count: u32) -> WorldBundle {
        let root = tempfile::tempdir().expect("fixture root");
        let assets = root.path().join("assets");
        std::fs::create_dir_all(assets.join("sha256")).expect("fixture assets");
        let visual_id = format!("sha256/{:x}.glb", sha2::Sha256::digest(visual));
        std::fs::write(assets.join(&visual_id), visual).expect("fixture visual");
        let geometry = serde_json::json!({
            "kind": "mesh", "filename": visual_id, "scale": [0.5, 0.75, 1.25]
        });
        let mut collision_geometry = geometry.clone();
        if let Some(collision) = collision {
            let id = format!("sha256/{:x}.glb", sha2::Sha256::digest(collision));
            std::fs::write(assets.join(&id), collision).expect("fixture collision");
            collision_geometry = serde_json::json!({
                "kind": "mesh", "filename": id, "scale": null
            });
        }
        let entities = (0..count)
            .map(|instance| {
                serde_json::json!({
                    "declaration": "exhibit", "instance": instance,
                    "pose": { "xyz": [f64::from(instance), 0.0, 0.0], "rpy": [0.0, 0.0, 0.0] },
                    "geometry": geometry, "collision": collision_geometry
                })
            })
            .collect::<Vec<_>>();
        let source = serde_json::json!({
            "schema": "phoxal/world-bundle/v0", "id": "webots-glb",
            "time_step_ns": 12_000_000, "gravity_mps2": [0.0, 0.0, -9.81],
            "spawn_points": {}, "entities": entities
        });
        std::fs::write(
            root.path().join("world.json"),
            serde_json::to_vec(&source).expect("JSON"),
        )
        .expect("fixture document");
        WorldBundle::open(root.path()).expect("canonical fixture opens")
    }

    #[test]
    fn repeated_instances_decode_each_distinct_asset_once() {
        let triangle = triangle_glb();
        let bundle = mesh_world_instances(&triangle, None, 1_000);
        let mut calls = 0;
        let decoded = DecodedWorldAssets::decode_with(&bundle, |_, bytes| {
            calls += 1;
            DecodedMesh::decode(bytes)
        })
        .expect("distinct assets decode");
        assert_eq!(calls, 1);
        let source = render_world_with_decoded(&bundle, "tcp://127.0.0.1:7000", &decoded)
            .expect("large world renders");
        assert_eq!(source.matches("IndexedFaceSet").count(), 2_000);
        assert_eq!(calls, 1);
        assert_eq!(
            source,
            render_world(&bundle, "tcp://127.0.0.1:7000").expect("public renderer")
        );
    }

    #[test]
    fn axis_angle_has_a_canonical_identity() {
        let pose: Pose = serde_json::from_value(serde_json::json!({
            "xyz": [0.0, 0.0, 0.0],
            "rpy": [0.0, 0.0, 0.0]
        }))
        .expect("pose decodes");
        assert_eq!(axis_angle(pose), [0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn generated_number_spelling_is_stable() {
        assert_eq!(number(-0.0), "0");
        assert_eq!(number(12.0), "12");
        assert_eq!(number(0.5), "0.5");
    }

    #[test]
    fn world_info_selects_deterministic_wgs84_gps() {
        let mut source = String::new();
        render_world_info(&mut source, 12, 9.81).expect("WorldInfo renders");
        assert!(source.contains("gpsCoordinateSystem \"WGS84\""));
        assert!(source.contains("gpsReference 0 0 0"));
        assert!(!source.contains("GPS {"));
    }

    #[test]
    fn authored_implicit_glb_collision_renders_native_triangles() {
        let triangle = triangle_glb();
        let bundle = compile_mesh_world(&triangle, None);
        let source = render_world(&bundle, "tcp://127.0.0.1:7000").expect("world renders");
        assert_eq!(source.matches("IndexedFaceSet").count(), 2);
        assert!(source.contains("scale 0.5 0.75 1.25"));
        assert!(!source.contains("CadShape"));
        assert!(!source.contains("url [\"../assets/"));
        let _: webots_proto_ast::Proto = source.parse().expect("native world parses");
    }

    #[test]
    fn detailed_visual_uses_explicit_bounded_glb_collision_override() {
        let visual =
            include_bytes!("../../../../fixture/components/drive_motor/meshes/drive_motor.glb");
        let collision = triangle_glb();
        let bundle = compile_mesh_world(visual, Some(&collision));
        let entity = bundle.world().entities().next().expect("fixture entity");
        assert_ne!(entity.geometry().asset_id(), entity.collision().asset_id());
        let source = render_world(&bundle, "tcp://127.0.0.1:7000").expect("world renders");
        assert!(source.matches("IndexedFaceSet").count() >= 4);
        assert!(!source.contains("CadShape"));
        assert!(!source.contains("url [\"../assets/"));
        let _: webots_proto_ast::Proto = source.parse().expect("native world parses");
    }
}
