use anyhow::{Result, bail};
use phoxal::model::structure::{Geometry, Structure};

/// The robot's bounding radius in meters, derived from the base_link's first
/// collision geometry. Used by the traversability inflation step.
///
/// MVP: only Box, Cylinder, and Sphere are supported. Mesh-only and
/// capsule-only collisions fail explicitly. We do not load mesh files, and the
/// repository forbids tests that validate mesh bytes or mesh-file existence.
pub fn body_radius_from_structure(structure: &Structure, base_link_id: &str) -> Result<f64> {
    let Some(link) = structure.link(base_link_id) else {
        bail!("structure has no link named '{base_link_id}'");
    };
    let Some(first_collision) = link.collision.first() else {
        bail!("link '{base_link_id}' has no collision geometry");
    };

    match &first_collision.geometry {
        Geometry::Box { size } => {
            // Half-diagonal of the XY footprint. The vertical dimension does
            // not contribute to a 2D footprint radius.
            Ok(((size[0] / 2.0).powi(2) + (size[1] / 2.0).powi(2)).sqrt())
        }
        Geometry::Cylinder { radius, .. } | Geometry::Sphere { radius } => Ok(*radius),
        Geometry::Capsule { .. } | Geometry::Mesh { .. } => {
            bail!(
                "link '{base_link_id}' uses mesh or capsule collision; MVP requires Box, Cylinder, or Sphere"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use phoxal::model::structure::Structure;

    use super::*;

    #[test]
    fn body_radius_from_box_collision() -> Result<()> {
        let structure = structure_from_geometry(r#"<box size="0.50 0.30 0.10"/>"#)?;

        let body_radius_m = body_radius_from_structure(&structure, "base_link")?;

        assert!((body_radius_m - 0.291_547_594_742_265).abs() < 1e-9);
        Ok(())
    }

    #[test]
    fn body_radius_from_cylinder_collision() -> Result<()> {
        let structure = structure_from_geometry(r#"<cylinder radius="0.20" length="0.10"/>"#)?;

        let body_radius_m = body_radius_from_structure(&structure, "base_link")?;

        assert!((body_radius_m - 0.20).abs() < 1e-9);
        Ok(())
    }

    #[test]
    fn body_radius_from_sphere_collision() -> Result<()> {
        let structure = structure_from_geometry(r#"<sphere radius="0.15"/>"#)?;

        let body_radius_m = body_radius_from_structure(&structure, "base_link")?;

        assert!((body_radius_m - 0.15).abs() < 1e-9);
        Ok(())
    }

    #[test]
    fn body_radius_from_mesh_collision_errors() -> Result<()> {
        let structure = structure_from_geometry(r#"<mesh filename="package://meshes/foo.obj"/>"#)?;

        let result = body_radius_from_structure(&structure, "base_link");

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn body_radius_missing_link_errors() -> Result<()> {
        let structure = structure_from_geometry(r#"<sphere radius="0.15"/>"#)?;

        let result = body_radius_from_structure(&structure, "missing_link");

        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn body_radius_missing_collision_errors() -> Result<()> {
        let structure = Structure::from_urdf_str(
            r#"
            <robot name="test_robot">
              <link name="base_link">
                <visual>
                  <geometry>
                    <sphere radius="0.15"/>
                  </geometry>
                </visual>
              </link>
            </robot>
            "#,
        )?;

        let result = body_radius_from_structure(&structure, "base_link");

        assert!(result.is_err());
        Ok(())
    }

    fn structure_from_geometry(geometry: &str) -> Result<Structure> {
        Structure::from_urdf_str(&format!(
            r#"
            <robot name="test_robot">
              <link name="base_link">
                <collision>
                  <geometry>
                    {geometry}
                  </geometry>
                </collision>
              </link>
            </robot>
            "#
        ))
    }
}
