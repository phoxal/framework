pub const BASE_FOOTPRINT_LINK: &str = "base_footprint";
pub const BASE_LINK: &str = "base_link";

pub const COMPONENT_FILE: &str = "component.yaml";
pub const COMPONENTS_DIR: &str = "components";
pub const DEFAULT_ROBOT_NAMESPACE: &str = "dev";
pub const MESHES_DIR: &str = "meshes";
pub const ROBOT_FILE: &str = "robot.yaml";
pub const SIMULATION_FILE: &str = "simulation.yaml";
pub const STRUCTURE_FILE: &str = "structure.urdf";

pub const MODEL_URI_PREFIX: &str = "model://";
pub const PACKAGE_URI_PREFIX: &str = "package://";

pub const PHOXAL_COMPONENT_PACKAGE_PREFIX: &str = "phoxal-component-";
pub const PHOXAL_RUNTIME_PACKAGE_PREFIX: &str = "phoxal-runtime-";

pub fn component_package_name(component_type: &str) -> String {
    format!("{PHOXAL_COMPONENT_PACKAGE_PREFIX}{component_type}")
}

pub fn runtime_package_name(runtime_name: &str) -> String {
    format!("{PHOXAL_RUNTIME_PACKAGE_PREFIX}{runtime_name}")
}
