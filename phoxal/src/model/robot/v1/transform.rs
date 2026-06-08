//! 3D transform helper used by model config and TF payloads.

use nalgebra::{Quaternion, UnitQuaternion};
use serde::{Deserialize, Serialize};

/// 3D transform (translation + rotation).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Transform {
    /// Translation in meters [x, y, z].
    pub translation_xyz: [f64; 3],
    /// Rotation in radians [roll, pitch, yaw].
    pub rotation_rpy: [f64; 3],
}

impl Transform {
    pub const IDENTITY: Self = Self {
        translation_xyz: [0.0, 0.0, 0.0],
        rotation_rpy: [0.0, 0.0, 0.0],
    };

    #[must_use]
    pub const fn new(translation: [f64; 3], rotation: [f64; 3]) -> Self {
        Self {
            translation_xyz: translation,
            rotation_rpy: rotation,
        }
    }

    /// Convert RPY rotation to quaternion [x, y, z, w].
    #[must_use]
    pub fn rotation_quaternion(&self) -> [f64; 4] {
        let [roll, pitch, yaw] = self.rotation_rpy;
        let quaternion = UnitQuaternion::from_euler_angles(roll, pitch, yaw);
        [quaternion.i, quaternion.j, quaternion.k, quaternion.w]
    }

    /// Create transform from translation and quaternion [x, y, z, w].
    #[must_use]
    pub fn from_translation_quaternion(translation: [f64; 3], quaternion: [f64; 4]) -> Self {
        let [x, y, z, w] = quaternion;
        let unit = UnitQuaternion::new_normalize(Quaternion::new(w, x, y, z));
        let (roll, pitch, yaw) = unit.euler_angles();
        Self {
            translation_xyz: translation,
            rotation_rpy: [roll, pitch, yaw],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quaternion_identity() {
        let transform = Transform::IDENTITY;
        let quaternion = transform.rotation_quaternion();
        assert!((quaternion[0] - 0.0).abs() < 1e-10);
        assert!((quaternion[1] - 0.0).abs() < 1e-10);
        assert!((quaternion[2] - 0.0).abs() < 1e-10);
        assert!((quaternion[3] - 1.0).abs() < 1e-10);
    }
}
