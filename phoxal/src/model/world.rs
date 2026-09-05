//! Canonical compiled worlds, identities, progress, and runtime provenance.
//!
//! Authored paths end at the world compiler.
//! A runtime adapter receives one [`WorldBundle`] containing a canonical expanded world and every reachable asset byte.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::asset::AssetId;
use crate::model::geometry::Geometry;
use crate::model::identity::{EntityDeclarationId, SpawnId, WorldId};
use crate::model::structure::Pose;
use crate::version::FrameworkVersion;

const WORLD_FILE: &str = "world.json";
const ASSETS_DIRECTORY: &str = "assets";
const ARCHIVE_MAGIC: &[u8] = b"phoxal-world-bundle-v0\0";
const SESSION_ID_BYTES: usize = 16;
const SESSION_ID_HEX_LEN: usize = SESSION_ID_BYTES * 2;
const CANONICAL_TOP_NIBBLE: u128 = 1 << 124;

/// One live world-session identity minted by its session host.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct WorldInstanceId(u128);

impl WorldInstanceId {
    /// The exact rendered width of a world-session identity.
    pub const LEN: usize = SESSION_ID_HEX_LEN;

    /// Mint one random canonical identity.
    #[must_use]
    pub fn mint() -> Self {
        loop {
            let mut bytes = [0_u8; SESSION_ID_BYTES];
            #[expect(
                clippy::expect_used,
                reason = "a world session cannot start safely without a unique identity"
            )]
            getrandom::fill(&mut bytes).expect("the host must provide randomness");
            let value = u128::from_be_bytes(bytes);
            if value >= CANONICAL_TOP_NIBBLE {
                return Self(value);
            }
        }
    }

    /// Parse the exact lowercase hexadecimal representation.
    ///
    /// # Errors
    ///
    /// Returns [`WorldIdentityError`] when the spelling is not canonical.
    pub fn parse(value: &str) -> Result<Self, WorldIdentityError> {
        if value.len() != SESSION_ID_HEX_LEN
            || value.starts_with('0')
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(WorldIdentityError(value.to_owned()));
        }
        u128::from_str_radix(value, 16)
            .map(Self)
            .map_err(|_| WorldIdentityError(value.to_owned()))
    }
}

impl fmt::Display for WorldInstanceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:032x}", self.0)
    }
}

impl fmt::Debug for WorldInstanceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "WorldInstanceId({self})")
    }
}

impl std::str::FromStr for WorldInstanceId {
    type Err = WorldIdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for WorldInstanceId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WorldInstanceId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl crate::__compat::wire::DescribeWire for WorldInstanceId {
    fn wire_schema() -> crate::__compat::wire::WireSchema {
        crate::__compat::wire::WireSchema::opaque(
            "WorldInstanceId",
            crate::__compat::wire::WireSchema::String,
        )
    }
}

/// A world-session identity that is not in canonical form.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error(
    "world instance id must be exactly 32 lowercase hexadecimal characters with a nonzero leading nibble, got '{0}'"
)]
pub struct WorldIdentityError(String);

/// Authoritative absolute physics progress in one world session.
#[derive(phoxal_macros::DescribeWire, Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorldProgress {
    completed_step: u64,
    elapsed_ns: u64,
}

impl WorldProgress {
    /// Construct progress from its completed step and declared physics quantum.
    ///
    /// # Errors
    ///
    /// Returns [`WorldProgressError`] when the multiplication overflows.
    pub fn at(completed_step: u64, time_step_ns: u64) -> Result<Self, WorldProgressError> {
        if time_step_ns == 0 {
            return Err(WorldProgressError::ZeroQuantum);
        }
        let elapsed_ns =
            completed_step
                .checked_mul(time_step_ns)
                .ok_or(WorldProgressError::Overflow {
                    completed_step,
                    time_step_ns,
                })?;
        Ok(Self {
            completed_step,
            elapsed_ns,
        })
    }

    /// Zero progress before the first native transition.
    pub fn zero(time_step_ns: u64) -> Result<Self, WorldProgressError> {
        Self::at(0, time_step_ns)
    }

    /// The world-absolute number of completed native transitions.
    #[must_use]
    pub const fn completed_step(self) -> u64 {
        self.completed_step
    }

    /// The exact simulated duration represented by the completed transitions.
    #[must_use]
    pub const fn elapsed_ns(self) -> u64 {
        self.elapsed_ns
    }

    /// Validate this progress against a world's declared quantum.
    ///
    /// # Errors
    ///
    /// Returns [`WorldProgressError`] when the fields disagree.
    pub fn validate(self, time_step_ns: u64) -> Result<(), WorldProgressError> {
        let expected = Self::at(self.completed_step, time_step_ns)?;
        if expected == self {
            Ok(())
        } else {
            Err(WorldProgressError::Inconsistent {
                completed_step: self.completed_step,
                elapsed_ns: self.elapsed_ns,
                expected_ns: expected.elapsed_ns,
            })
        }
    }

    /// Validate that the two wire fields can describe one fixed positive
    /// physics quantum, even when that quantum is not known yet.
    ///
    /// Zero progress is the one boundary that carries no intrinsic quantum:
    /// it is valid only with zero elapsed time. Every later boundary must be
    /// an exact positive multiple of its completed-step count.
    ///
    /// # Errors
    ///
    /// Returns [`WorldProgressError::InvalidRatio`] when no positive integral
    /// quantum can produce both fields.
    pub fn validate_intrinsic(self) -> Result<(), WorldProgressError> {
        let valid = if self.completed_step == 0 {
            self.elapsed_ns == 0
        } else {
            self.elapsed_ns > 0 && self.elapsed_ns.is_multiple_of(self.completed_step)
        };
        if valid {
            Ok(())
        } else {
            Err(WorldProgressError::InvalidRatio {
                completed_step: self.completed_step,
                elapsed_ns: self.elapsed_ns,
            })
        }
    }
}

impl<'de> Deserialize<'de> for WorldProgress {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            completed_step: u64,
            elapsed_ns: u64,
        }

        let raw = Raw::deserialize(deserializer)?;
        let progress = Self {
            completed_step: raw.completed_step,
            elapsed_ns: raw.elapsed_ns,
        };
        progress
            .validate_intrinsic()
            .map_err(serde::de::Error::custom)?;
        Ok(progress)
    }
}

/// An invalid world-progress value.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WorldProgressError {
    #[error("world time step must be positive")]
    ZeroQuantum,
    #[error("world progress overflows for step {completed_step} at {time_step_ns} ns")]
    Overflow {
        completed_step: u64,
        time_step_ns: u64,
    },
    #[error(
        "world progress step {completed_step} and {elapsed_ns} ns do not imply a positive integral physics quantum"
    )]
    InvalidRatio {
        completed_step: u64,
        elapsed_ns: u64,
    },
    #[error(
        "world progress step {completed_step} carries {elapsed_ns} ns, expected {expected_ns} ns"
    )]
    Inconsistent {
        completed_step: u64,
        elapsed_ns: u64,
        expected_ns: u64,
    },
}

/// The immutable correlation recorded when a monotonic execution joins a world.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct LiveAttachmentBoundary {
    /// The completed native-world boundary observed during attachment.
    pub world: WorldProgress,
    /// The execution's unchanged monotonic instant at that same boundary.
    pub execution: crate::bus::RobotInstant,
}

/// The SHA-256 identity of a complete canonical world archive.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorldDigest([u8; 32]);

impl WorldDigest {
    fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    /// Parse the exact lowercase hexadecimal representation.
    ///
    /// # Errors
    ///
    /// Returns [`WorldDigestError`] when the spelling is not 64 lowercase hexadecimal characters.
    pub fn parse(value: &str) -> Result<Self, WorldDigestError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(WorldDigestError(value.to_owned()));
        }
        let mut bytes = [0_u8; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| WorldDigestError(value.to_owned()))?;
        }
        Ok(Self(bytes))
    }

    /// The digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for WorldDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for WorldDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "WorldDigest({self})")
    }
}

impl Serialize for WorldDigest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WorldDigest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl crate::__compat::wire::DescribeWire for WorldDigest {
    fn wire_schema() -> crate::__compat::wire::WireSchema {
        crate::__compat::wire::WireSchema::opaque(
            "WorldDigest",
            crate::__compat::wire::WireSchema::String,
        )
    }
}

/// A digest spelling that is not canonical SHA-256 hexadecimal.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("world digest must be exactly 64 lowercase hexadecimal characters, got '{0}'")]
pub struct WorldDigestError(String);

/// One expanded static entity in a compiled world.
#[derive(phoxal_macros::DescribeWire, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldEntity {
    declaration: EntityDeclarationId,
    instance: u32,
    pose: Pose,
    geometry: Geometry,
    collision: Geometry,
}

impl WorldEntity {
    /// The declaration this anonymous instance was expanded from.
    #[must_use]
    pub const fn declaration(&self) -> &EntityDeclarationId {
        &self.declaration
    }

    /// The zero-based instance order inside the declaration.
    #[must_use]
    pub const fn instance(&self) -> u32 {
        self.instance
    }

    /// The canonical world pose.
    #[must_use]
    pub const fn pose(&self) -> Pose {
        self.pose
    }

    /// The visible geometry.
    #[must_use]
    pub const fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    /// The exact physics geometry after collision defaults were expanded.
    #[must_use]
    pub const fn collision(&self) -> &Geometry {
        &self.collision
    }
}

/// One canonical expanded world.
#[derive(phoxal_macros::DescribeWire, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct World {
    id: WorldId,
    time_step_ns: u64,
    gravity_mps2: [f64; 3],
    spawn_points: BTreeMap<SpawnId, Pose>,
    entities: Vec<WorldEntity>,
}

impl World {
    /// The stable authored world identity.
    #[must_use]
    pub const fn id(&self) -> &WorldId {
        &self.id
    }

    /// The exact duration represented by one completed native transition.
    #[must_use]
    pub const fn time_step_ns(&self) -> u64 {
        self.time_step_ns
    }

    /// Gravity in SI metres per second squared in the canonical Z-up frame.
    #[must_use]
    pub const fn gravity_mps2(&self) -> [f64; 3] {
        self.gravity_mps2
    }

    /// The authored spawn points in deterministic name order.
    pub fn spawn_points(&self) -> impl ExactSizeIterator<Item = (&SpawnId, Pose)> {
        self.spawn_points.iter().map(|(id, pose)| (id, *pose))
    }

    /// Anonymous expanded entities in declaration-name and instance order.
    pub fn entities(&self) -> impl ExactSizeIterator<Item = &WorldEntity> {
        self.entities.iter()
    }

    fn referenced_assets(&self) -> BTreeSet<AssetId> {
        self.entities
            .iter()
            .flat_map(|entity| [entity.geometry.asset_id(), entity.collision.asset_id()])
            .flatten()
            .cloned()
            .collect()
    }

    fn validate(&self, assets: &BTreeMap<AssetId, Vec<u8>>) -> Result<(), WorldBundleError> {
        if self.time_step_ns == 0 || !self.time_step_ns.is_multiple_of(1_000_000) {
            return Err(WorldBundleError::Invalid(
                "time_step_ns must be a positive whole number of milliseconds".to_owned(),
            ));
        }
        if !self.gravity_mps2.into_iter().all(canonical_float) {
            return Err(WorldBundleError::Invalid(
                "gravity_mps2 must contain finite values without negative zero".to_owned(),
            ));
        }
        for (id, pose) in &self.spawn_points {
            if !pose
                .xyz()
                .into_iter()
                .chain(pose.rpy())
                .all(canonical_float)
            {
                return Err(WorldBundleError::Invalid(format!(
                    "spawn point '{id}' contains a non-canonical pose"
                )));
            }
        }
        let mut previous_declaration: Option<&EntityDeclarationId> = None;
        let mut previous_instance = 0_u32;
        for entity in &self.entities {
            if !entity
                .pose
                .xyz()
                .into_iter()
                .chain(entity.pose.rpy())
                .all(canonical_float)
            {
                return Err(WorldBundleError::Invalid(format!(
                    "entity '{}[{}]' contains a non-canonical pose",
                    entity.declaration, entity.instance
                )));
            }
            if !entity.geometry.has_valid_dimensions()
                || !entity.collision.has_valid_dimensions()
                || !geometry_is_canonical(&entity.geometry)
                || !geometry_is_canonical(&entity.collision)
            {
                return Err(WorldBundleError::Invalid(format!(
                    "entity '{}[{}]' contains non-canonical geometry",
                    entity.declaration, entity.instance
                )));
            }
            match previous_declaration {
                None if entity.instance == 0 => {}
                Some(previous) if previous == &entity.declaration => {
                    let expected = previous_instance.checked_add(1).ok_or_else(|| {
                        WorldBundleError::Invalid(
                            "world entity instance index exhausted".to_owned(),
                        )
                    })?;
                    if entity.instance != expected {
                        return Err(WorldBundleError::Invalid(
                            "world entity instances must be contiguous from zero".to_owned(),
                        ));
                    }
                }
                Some(previous) if previous < &entity.declaration && entity.instance == 0 => {}
                None | Some(_) => {
                    return Err(WorldBundleError::Invalid(
                        "world entities must be ordered by declaration and contiguous instance"
                            .to_owned(),
                    ));
                }
            }
            previous_declaration = Some(&entity.declaration);
            previous_instance = entity.instance;
        }
        let referenced = self.referenced_assets();
        let present = assets.keys().cloned().collect::<BTreeSet<_>>();
        if referenced != present {
            return Err(WorldBundleError::AssetClosure {
                referenced: referenced
                    .into_iter()
                    .map(|id| id.as_str().to_owned())
                    .collect(),
                present: present
                    .into_iter()
                    .map(|id| id.as_str().to_owned())
                    .collect(),
            });
        }
        for (id, bytes) in assets {
            let expected = format!("sha256/{:x}.glb", Sha256::digest(bytes));
            if id.as_str() != expected {
                return Err(WorldBundleError::AssetDigestMismatch {
                    asset: id.as_str().to_owned(),
                    expected,
                });
            }
            validate_closed_glb(bytes).map_err(|source| WorldBundleError::InvalidAsset {
                asset: id.as_str().to_owned(),
                detail: source.to_string(),
            })?;
        }
        WorldProgress::zero(self.time_step_ns)?;
        Ok(())
    }
}

/// Immutable facts required to qualify one world run.
#[derive(phoxal_macros::DescribeWire, Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorldProvenance {
    /// The stable authored world identity.
    pub world: WorldId,
    /// The complete canonical bundle digest.
    pub digest: WorldDigest,
    /// The effective simulator seed.
    pub random_seed: u64,
    /// The framework train used by the world-side processes.
    pub framework: FrameworkVersion,
    /// The concrete adapter name.
    pub adapter: String,
    /// The exact adapter package version.
    pub adapter_version: String,
    /// The observed native simulator version.
    pub simulator_version: String,
    /// The platform qualification string.
    pub platform: String,
    /// The exact physics quantum in nanoseconds.
    pub time_step_ns: u64,
}

/// A canonical expanded world plus every asset byte it can reach.
#[derive(Clone, Debug)]
pub struct WorldBundle {
    world: World,
    assets: BTreeMap<AssetId, Vec<u8>>,
    canonical_archive: Vec<u8>,
    digest: WorldDigest,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
enum WorldDocument {
    #[serde(rename = "phoxal/world-bundle/v0")]
    V0(World),
}

impl WorldBundle {
    pub(crate) fn from_compiler(
        world: World,
        assets: BTreeMap<AssetId, Vec<u8>>,
    ) -> Result<Self, WorldBundleError> {
        world.validate(&assets)?;
        let canonical_archive = canonical_archive(&world, &assets)?;
        let digest = WorldDigest::of(&canonical_archive);
        Ok(Self {
            world,
            assets,
            canonical_archive,
            digest,
        })
    }

    /// Open and validate one inspectable `world.json` plus `assets/` bundle.
    ///
    /// # Errors
    ///
    /// Returns [`WorldBundleError`] for I/O, document, path, or closure failures.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, WorldBundleError> {
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(|source| WorldBundleError::Io {
                path: root.as_ref().to_path_buf(),
                source,
            })?;
        validate_root_layout(&root)?;
        let document_path = root.join(WORLD_FILE);
        let document = std::fs::read(&document_path).map_err(|source| WorldBundleError::Io {
            path: document_path.clone(),
            source,
        })?;
        let WorldDocument::V0(world) =
            serde_json::from_slice(&document).map_err(|source| WorldBundleError::Document {
                path: document_path,
                source,
            })?;
        let assets = read_assets(&root.join(ASSETS_DIRECTORY))?;
        Self::from_compiler(world, assets)
    }

    /// Atomically write this bundle into a new target directory.
    ///
    /// # Errors
    ///
    /// Returns [`WorldBundleError::TargetExists`] when `root` already exists or an I/O error while staging.
    pub fn write(&self, root: impl AsRef<Path>) -> Result<(), WorldBundleError> {
        let root = root.as_ref();
        if root.exists() {
            return Err(WorldBundleError::TargetExists(root.to_path_buf()));
        }
        let parent = root.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(|source| WorldBundleError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
        let staging = tempfile::Builder::new()
            .prefix(".phoxal-world-")
            .tempdir_in(parent)
            .map_err(|source| WorldBundleError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        let assets_root = staging.path().join(ASSETS_DIRECTORY);
        std::fs::create_dir(&assets_root).map_err(|source| WorldBundleError::Io {
            path: assets_root.clone(),
            source,
        })?;
        let document = serde_json::to_vec_pretty(&WorldDocument::V0(self.world.clone()))?;
        let document_path = staging.path().join(WORLD_FILE);
        std::fs::write(&document_path, document).map_err(|source| WorldBundleError::Io {
            path: document_path,
            source,
        })?;
        for (id, bytes) in &self.assets {
            let path = asset_path(&assets_root, id)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| WorldBundleError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            std::fs::write(&path, bytes).map_err(|source| WorldBundleError::Io { path, source })?;
        }
        std::fs::rename(staging.path(), root).map_err(|source| WorldBundleError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        Ok(())
    }

    #[must_use]
    pub const fn world(&self) -> &World {
        &self.world
    }

    #[must_use]
    pub const fn digest(&self) -> WorldDigest {
        self.digest
    }

    pub fn assets(&self) -> impl ExactSizeIterator<Item = (&AssetId, &[u8])> {
        self.assets.iter().map(|(id, bytes)| (id, bytes.as_slice()))
    }

    #[must_use]
    pub fn asset(&self, id: &AssetId) -> Option<&[u8]> {
        self.assets.get(id).map(Vec::as_slice)
    }

    /// Deterministic bytes over which [`WorldDigest`] is computed.
    #[must_use]
    pub fn canonical_archive(&self) -> &[u8] {
        &self.canonical_archive
    }
}

fn canonical_float(value: f64) -> bool {
    value.is_finite() && (value != 0.0 || value.is_sign_positive())
}

fn geometry_is_canonical(geometry: &Geometry) -> bool {
    match geometry {
        Geometry::Box { size } => size.iter().copied().all(canonical_float),
        Geometry::Cylinder { radius, length } | Geometry::Capsule { radius, length } => {
            [*radius, *length].into_iter().all(canonical_float)
        }
        Geometry::Sphere { radius } => canonical_float(*radius),
        Geometry::Mesh { scale, .. } => scale
            .as_ref()
            .is_none_or(|values| values.iter().copied().all(canonical_float)),
    }
}

fn validate_root_layout(root: &Path) -> Result<(), WorldBundleError> {
    let mut world = false;
    let mut assets = false;
    let entries = std::fs::read_dir(root)
        .and_then(Iterator::collect::<std::io::Result<Vec<_>>>)
        .map_err(|source| WorldBundleError::Io {
            path: root.to_path_buf(),
            source,
        })?;
    for entry in entries {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|source| WorldBundleError::Io {
            path: path.clone(),
            source,
        })?;
        match entry.file_name().to_str() {
            Some(WORLD_FILE) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                world = true;
            }
            Some(ASSETS_DIRECTORY) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                assets = true;
            }
            _ => {
                return Err(WorldBundleError::Invalid(format!(
                    "world bundle contains an unsupported root entry: {}",
                    path.display()
                )));
            }
        }
    }
    if !world || !assets {
        return Err(WorldBundleError::Invalid(
            "world bundle must contain exactly world.json and assets/".to_owned(),
        ));
    }
    Ok(())
}

fn canonical_archive(
    world: &World,
    assets: &BTreeMap<AssetId, Vec<u8>>,
) -> Result<Vec<u8>, WorldBundleError> {
    let document = serde_json::to_vec(&WorldDocument::V0(world.clone()))?;
    let mut archive = Vec::new();
    archive.extend_from_slice(ARCHIVE_MAGIC);
    append_archive_entry(&mut archive, WORLD_FILE.as_bytes(), &document)?;
    for (id, bytes) in assets {
        let name = format!("{ASSETS_DIRECTORY}/{}", id.as_str());
        append_archive_entry(&mut archive, name.as_bytes(), bytes)?;
    }
    Ok(archive)
}

fn append_archive_entry(
    archive: &mut Vec<u8>,
    name: &[u8],
    bytes: &[u8],
) -> Result<(), WorldBundleError> {
    let name_len = u64::try_from(name.len()).map_err(|_| WorldBundleError::ArchiveTooLarge)?;
    let bytes_len = u64::try_from(bytes.len()).map_err(|_| WorldBundleError::ArchiveTooLarge)?;
    archive.extend_from_slice(&name_len.to_be_bytes());
    archive.extend_from_slice(name);
    archive.extend_from_slice(&bytes_len.to_be_bytes());
    archive.extend_from_slice(bytes);
    Ok(())
}

fn asset_path(root: &Path, id: &AssetId) -> Result<PathBuf, WorldBundleError> {
    let path = root.join(id.as_str());
    if !path.starts_with(root) {
        return Err(WorldBundleError::InvalidAssetPath(id.as_str().to_owned()));
    }
    Ok(path)
}

fn read_assets(root: &Path) -> Result<BTreeMap<AssetId, Vec<u8>>, WorldBundleError> {
    if !root.is_dir() {
        return Err(WorldBundleError::Invalid(format!(
            "world bundle is missing {}",
            root.display()
        )));
    }
    let mut assets = BTreeMap::new();
    read_asset_directory(root, root, &mut assets)?;
    Ok(assets)
}

fn read_asset_directory(
    root: &Path,
    current: &Path,
    assets: &mut BTreeMap<AssetId, Vec<u8>>,
) -> Result<(), WorldBundleError> {
    let mut entries = std::fs::read_dir(current)
        .and_then(Iterator::collect::<std::io::Result<Vec<_>>>)
        .map_err(|source| WorldBundleError::Io {
            path: current.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|source| WorldBundleError::Io {
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(WorldBundleError::Invalid(format!(
                "world bundle asset is a forbidden symlink: {}",
                path.display()
            )));
        }
        if metadata.is_dir() {
            read_asset_directory(root, &path, assets)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(WorldBundleError::Invalid(format!(
                "world bundle contains an unsupported asset entry: {}",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| WorldBundleError::InvalidAssetPath(path.display().to_string()))?;
        let relative = relative
            .to_str()
            .ok_or_else(|| WorldBundleError::InvalidAssetPath(path.display().to_string()))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let id = AssetId::new(relative)
            .map_err(|_| WorldBundleError::InvalidAssetPath(path.display().to_string()))?;
        let bytes = std::fs::read(&path).map_err(|source| WorldBundleError::Io {
            path: path.clone(),
            source,
        })?;
        assets.insert(id, bytes);
    }
    Ok(())
}

/// Validate the one closed binary glTF form accepted by world compilation and reopening.
pub(crate) fn validate_closed_glb(bytes: &[u8]) -> Result<(), ClosedGlbError> {
    const JSON_CHUNK: u32 = 0x4E4F_534A;
    const BIN_CHUNK: u32 = 0x004E_4942;

    if bytes.len() < 20 || bytes.get(0..4) != Some(b"glTF") {
        return Err(ClosedGlbError("missing the GLB header".to_owned()));
    }
    let version = glb_u32(bytes, 4, "truncated GLB version")?;
    let declared = glb_u32(bytes, 8, "truncated GLB length")?;
    if version != 2 || usize::try_from(declared).ok() != Some(bytes.len()) {
        return Err(ClosedGlbError(format!(
            "expected GLB version 2 with declared length {}, found version {version} and length {declared}",
            bytes.len()
        )));
    }

    let mut offset = 12_usize;
    let mut json = None;
    let mut binary = None;
    while offset < bytes.len() {
        let header_end = offset
            .checked_add(8)
            .ok_or_else(|| ClosedGlbError("chunk header offset overflows".to_owned()))?;
        let header = bytes
            .get(offset..header_end)
            .ok_or_else(|| ClosedGlbError("truncated GLB chunk header".to_owned()))?;
        let length = glb_u32(header, 0, "invalid GLB chunk length")? as usize;
        let kind = glb_u32(header, 4, "invalid GLB chunk type")?;
        if !length.is_multiple_of(4) {
            return Err(ClosedGlbError(
                "GLB chunk length is not four-byte aligned".to_owned(),
            ));
        }
        let end = header_end
            .checked_add(length)
            .ok_or_else(|| ClosedGlbError("GLB chunk length overflows".to_owned()))?;
        let chunk = bytes
            .get(header_end..end)
            .ok_or_else(|| ClosedGlbError("truncated GLB chunk".to_owned()))?;
        match kind {
            JSON_CHUNK if offset == 12 && json.is_none() => json = Some(chunk),
            JSON_CHUNK => {
                return Err(ClosedGlbError(
                    "GLB JSON must be the first and only JSON chunk".to_owned(),
                ));
            }
            BIN_CHUNK if json.is_some() && binary.is_none() => binary = Some(chunk),
            BIN_CHUNK => {
                return Err(ClosedGlbError(
                    "GLB may contain at most one binary chunk after JSON".to_owned(),
                ));
            }
            _ => {
                return Err(ClosedGlbError(format!(
                    "unsupported GLB chunk type {kind:#010x}"
                )));
            }
        }
        offset = end;
    }

    let json = json.ok_or_else(|| ClosedGlbError("GLB has no JSON chunk".to_owned()))?;
    let json = std::str::from_utf8(json)
        .map_err(|source| ClosedGlbError(format!("JSON chunk is not UTF-8: {source}")))?;
    let json = json
        .trim_end_matches(|character: char| character == '\0' || character.is_ascii_whitespace());
    let document: serde_json::Value = serde_json::from_str(json)
        .map_err(|source| ClosedGlbError(format!("JSON chunk is invalid: {source}")))?;
    if document
        .get("asset")
        .and_then(|asset| asset.get("version"))
        .and_then(serde_json::Value::as_str)
        != Some("2.0")
    {
        return Err(ClosedGlbError(
            "JSON asset.version must be exactly '2.0'".to_owned(),
        ));
    }
    let buffers = match document.get("buffers") {
        Some(serde_json::Value::Array(buffers)) => buffers.as_slice(),
        Some(_) => {
            return Err(ClosedGlbError("JSON buffers must be an array".to_owned()));
        }
        None => &[],
    };
    let mut embedded_buffer_length = None;
    for (index, buffer) in buffers.iter().enumerate() {
        let buffer = buffer
            .as_object()
            .ok_or_else(|| ClosedGlbError(format!("buffers[{index}] must be an object")))?;
        let byte_length = buffer
            .get("byteLength")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                ClosedGlbError(format!(
                    "buffers[{index}].byteLength must be a non-negative integer"
                ))
            })?;
        let byte_length = usize::try_from(byte_length).map_err(|_| {
            ClosedGlbError(format!(
                "buffers[{index}].byteLength exceeds the supported size"
            ))
        })?;
        match buffer.get("uri") {
            Some(serde_json::Value::String(uri)) if uri.starts_with("data:") => {}
            Some(serde_json::Value::String(uri)) => {
                return Err(ClosedGlbError(format!(
                    "buffers contains external URI '{uri}'"
                )));
            }
            Some(_) => {
                return Err(ClosedGlbError(format!(
                    "buffers[{index}].uri must be a string"
                )));
            }
            None if index == 0 => embedded_buffer_length = Some(byte_length),
            None => {
                return Err(ClosedGlbError(
                    "only buffers[0] may omit uri and use the GLB binary chunk".to_owned(),
                ));
            }
        }
    }
    match (embedded_buffer_length, binary) {
        (None, None) => {}
        (None, Some(_)) => {
            return Err(ClosedGlbError(
                "GLB binary chunk has no matching buffers[0] without uri".to_owned(),
            ));
        }
        (Some(_), None) => {
            return Err(ClosedGlbError(
                "buffers[0] omits uri but the GLB binary chunk is missing".to_owned(),
            ));
        }
        (Some(byte_length), Some(binary)) => {
            let maximum = byte_length
                .checked_add(3)
                .ok_or_else(|| ClosedGlbError("buffer byte length overflows".to_owned()))?;
            if binary.len() < byte_length || binary.len() > maximum {
                return Err(ClosedGlbError(format!(
                    "GLB binary chunk length {} does not cover buffers[0].byteLength {byte_length} with at most three padding bytes",
                    binary.len()
                )));
            }
            if binary[byte_length..].iter().any(|byte| *byte != 0) {
                return Err(ClosedGlbError(
                    "GLB binary chunk padding bytes must be zero".to_owned(),
                ));
            }
        }
    }
    if let Some(images) = document.get("images") {
        let images = images
            .as_array()
            .ok_or_else(|| ClosedGlbError("JSON images must be an array".to_owned()))?;
        for (index, image) in images.iter().enumerate() {
            let image = image
                .as_object()
                .ok_or_else(|| ClosedGlbError(format!("images[{index}] must be an object")))?;
            if let Some(uri) = image.get("uri") {
                let uri = uri.as_str().ok_or_else(|| {
                    ClosedGlbError(format!("images[{index}].uri must be a string"))
                })?;
                if !uri.starts_with("data:") {
                    return Err(ClosedGlbError(format!(
                        "images contains external URI '{uri}'"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn glb_u32(bytes: &[u8], offset: usize, detail: &'static str) -> Result<u32, ClosedGlbError> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| ClosedGlbError(detail.to_owned()))?;
    let value = bytes
        .get(offset..end)
        .ok_or_else(|| ClosedGlbError(detail.to_owned()))?;
    Ok(u32::from_le_bytes(
        value
            .try_into()
            .map_err(|_| ClosedGlbError(detail.to_owned()))?,
    ))
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct ClosedGlbError(String);

/// A compiled world bundle that is not closed and canonical.
#[derive(Debug, thiserror::Error)]
pub enum WorldBundleError {
    #[error("world bundle target already exists: {}", .0.display())]
    TargetExists(PathBuf),
    #[error("world bundle I/O failed at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("world bundle document {} is invalid: {source}", path.display())]
    Document {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("world bundle JSON could not be encoded: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid world bundle: {0}")]
    Invalid(String),
    #[error("invalid world bundle asset path '{0}'")]
    InvalidAssetPath(String),
    #[error("world bundle asset '{asset}' is not a closed GLB v2 file: {detail}")]
    InvalidAsset { asset: String, detail: String },
    #[error("world bundle asset closure differs: referenced {referenced:?}, present {present:?}")]
    AssetClosure {
        referenced: Vec<String>,
        present: Vec<String>,
    },
    #[error("world bundle asset '{asset}' does not match its byte digest; expected '{expected}'")]
    AssetDigestMismatch { asset: String, expected: String },
    #[error(transparent)]
    Progress(#[from] WorldProgressError),
    #[error("world bundle canonical archive exceeds supported length")]
    ArchiveTooLarge,
}

#[allow(
    dead_code,
    reason = "the authoring profile constructs canonical worlds; runtime profiles only read them"
)]
pub(crate) fn compiled_world(
    id: WorldId,
    time_step_ns: u64,
    gravity_mps2: [f64; 3],
    spawn_points: BTreeMap<SpawnId, Pose>,
    entities: Vec<WorldEntity>,
) -> World {
    World {
        id,
        time_step_ns,
        gravity_mps2,
        spawn_points,
        entities,
    }
}

#[allow(
    dead_code,
    reason = "the authoring profile expands entities; runtime profiles only read them"
)]
pub(crate) fn compiled_entity(
    declaration: EntityDeclarationId,
    instance: u32,
    pose: Pose,
    geometry: Geometry,
    collision: Geometry,
) -> WorldEntity {
    WorldEntity {
        declaration,
        instance,
        pose,
        geometry,
        collision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn world_instance_identity_is_full_width_and_strict() {
        let id = WorldInstanceId::mint();
        let text = id.to_string();
        assert_eq!(text.len(), WorldInstanceId::LEN);
        assert_eq!(WorldInstanceId::parse(&text), Ok(id));
        for invalid in [
            "0123456789abcdef0123456789abcdef",
            "ABCDEF0123456789ABCDEF0123456789",
            "1234",
        ] {
            assert!(WorldInstanceId::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn progress_is_one_checked_value() {
        assert_eq!(WorldProgress::at(4, 12).unwrap().elapsed_ns(), 48);
        assert!(WorldProgress::at(u64::MAX, 2).is_err());
        assert!(WorldProgress::at(1, 0).is_err());
        let inconsistent = WorldProgress {
            completed_step: 2,
            elapsed_ns: 25,
        };
        assert!(inconsistent.validate(12).is_err());
        for malformed in [
            serde_json::json!({"completed_step": 0, "elapsed_ns": 1}),
            serde_json::json!({"completed_step": 2, "elapsed_ns": 25}),
        ] {
            assert!(
                serde_json::from_value::<WorldProgress>(malformed).is_err(),
                "wire decoding must preserve the progress invariant"
            );
        }
    }

    #[test]
    fn digest_spelling_is_strict() {
        let digest = WorldDigest::of(b"world");
        let text = digest.to_string();
        assert_eq!(text.len(), 64);
        assert_eq!(WorldDigest::parse(&text), Ok(digest));
        assert!(WorldDigest::parse(&text.to_uppercase()).is_err());
    }
}
