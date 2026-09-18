#![cfg(feature = "rendering")]

use std::path::Path;

use phoxal_mujoco::{Model, Workspace};

#[test]
fn native_camera_renders_rgb_and_metric_depth_from_a_private_workspace() {
    let model = Model::from_file(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rendering/scene.xml"),
    )
    .expect("rendering fixture model");
    let camera = model
        .camera("camera")
        .expect("camera lookup")
        .expect("rendering fixture camera");
    let mut workspace = Workspace::new(&model).expect("private rendering workspace");
    let rendered = workspace
        .render_camera(camera)
        .expect("native camera render");

    assert_eq!(rendered.resolution(), [4, 3]);
    assert_eq!(rendered.rgb().len(), 4 * 3 * 3);
    assert_eq!(rendered.depth_m().len(), 4 * 3);
    assert!(rendered.depth_m().iter().all(|depth| depth.is_finite()));
    assert!(
        rendered
            .depth_m()
            .iter()
            .all(|depth| (*depth - 1.0).abs() < 0.001),
        "plane depth: {:?}",
        rendered.depth_m()
    );
    assert!(
        rendered
            .rgb()
            .chunks_exact(3)
            .all(|pixel| pixel[0] > pixel[1] && pixel[0] > pixel[2]),
        "red plane RGB: {:?}",
        rendered.rgb()
    );
    let mut second = Workspace::new(&model).expect("second independent renderer");
    let other = second.render_camera(camera).expect("second render");
    assert_eq!(rendered, other);
    drop(second);
    assert_eq!(
        rendered,
        workspace
            .render_camera(camera)
            .expect("first renderer survives second context destruction")
    );
}

#[test]
fn oversized_framebuffer_is_rejected_before_native_allocation() {
    let model = Model::from_xml(r#"<mujoco><visual><global offwidth="8192" offheight="8192"/></visual><worldbody><camera name="camera" resolution="4 3"/></worldbody></mujoco>"#).unwrap();
    let camera = model.camera("camera").unwrap().unwrap();
    let mut workspace = Workspace::new(&model).unwrap();
    assert!(
        workspace
            .render_camera(camera)
            .unwrap_err()
            .to_string()
            .contains("pixel budget")
    );
}

#[test]
fn oversized_capture_is_rejected_before_native_allocation() {
    let model = Model::from_xml(r#"<mujoco><worldbody/></mujoco>"#).unwrap();
    let mut workspace = Workspace::new(&model).unwrap();
    let view = workspace.default_view_camera();
    assert!(workspace.render_viewport(view, [usize::MAX, 2]).is_err());
    assert!(workspace.render_viewport(view, [0, 1]).is_err());
}

#[test]
fn no_hit_depth_is_invalid_instead_of_a_fabricated_far_plane_measurement() {
    let model = Model::from_xml(
        r#"<mujoco><worldbody><camera name="camera" resolution="4 3"/></worldbody></mujoco>"#,
    )
    .unwrap();
    let camera = model.camera("camera").unwrap().unwrap();
    let mut workspace = Workspace::new(&model).unwrap();
    let rendered = workspace.render_camera(camera).unwrap();
    assert!(
        rendered.depth_m().iter().all(|depth| *depth == 0.0),
        "{:?}",
        rendered.depth_m()
    );
}

#[test]
fn cameras_with_different_resolutions_share_a_workspace_without_stale_pixels() {
    let model = Model::from_xml(
        r#"<mujoco><worldbody>
        <geom type="plane" size="10 10 0.1" rgba="1 0 0 1"/>
        <camera name="large" pos="0 0 2" resolution="64 48"/>
        <camera name="small" pos="0 0 1" resolution="8 6"/>
        </worldbody></mujoco>"#,
    )
    .unwrap();
    let mut workspace = Workspace::new(&model).unwrap();
    for _ in 0..3 {
        for (name, resolution, expected_depth) in [("large", [64, 48], 2.0), ("small", [8, 6], 1.0)]
        {
            let image = workspace
                .render_camera(model.camera(name).unwrap().unwrap())
                .unwrap();
            assert_eq!(image.resolution(), resolution);
            assert!(
                image
                    .depth_m()
                    .iter()
                    .all(|depth| (*depth - expected_depth).abs() < 0.001)
            );
        }
    }
}
