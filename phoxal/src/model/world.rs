//! Canonical compiled worlds, identities, progress, and runtime provenance.
//!
//! Authored paths end at the world compiler.
//! A runtime adapter receives one `WorldBundle`
//! containing a canonical expanded world and every reachable asset byte.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bundle::WorldBundleError;
use crate::model::asset::AssetId;
use crate::model::geometry::Geometry;
use crate::model::identity::{EntityDeclarationId, SpawnId, WorldId};
use crate::model::structure::Pose;
use crate::version::FrameworkVersion;

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
    pub(crate) fn of(bytes: &[u8]) -> Self {
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

    pub(crate) fn referenced_assets(&self) -> BTreeSet<AssetId> {
        self.entities
            .iter()
            .flat_map(|entity| [entity.geometry.asset_id(), entity.collision.asset_id()])
            .flatten()
            .cloned()
            .collect()
    }

    pub(crate) fn validate_intrinsic(&self) -> Result<(), WorldBundleError> {
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
