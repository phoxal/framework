use crate::capabilities::{
    accelerometer, battery, camera, depth, encoder, gnss, gyroscope, imu, led, lidar, magnetometer,
    microphone, mmwave, motor, range, speaker,
};
use anyhow::{Context, Result, anyhow, bail};
use phoxal::model::component::v1::CapabilityRef;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActuatorType {
    Velocity,
    Position,
    Torque,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Controller {
    Motor(motor::Config),
    Encoder(encoder::Config),
    Accelerometer(accelerometer::Config),
    Gyroscope(gyroscope::Config),
    Magnetometer(magnetometer::Config),
    Imu(imu::Config),
    Gnss(gnss::Config),
    Camera(camera::Config),
    Depth(depth::Config),
    Range(range::Config),
    Lidar(lidar::Config),
    Mmwave(mmwave::Config),
    Microphone(microphone::Config),
    Speaker(speaker::Config),
    Battery(battery::Config),
    Led(led::Config),
}

#[derive(Debug, Clone)]
pub struct Capability {
    pub capability: CapabilityRef,
    pub controller: Controller,
}

#[derive(Debug, Clone, Default)]
pub struct Component {
    pub component_id: String,
    pub proto_name: String,
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, Default)]
pub struct ControllerContract {
    pub components: Vec<Component>,
}

#[derive(Debug, Clone, Default)]
struct ComponentProto {
    capabilities: BTreeMap<String, Controller>,
}

#[derive(Debug)]
struct RobotProtoInstance {
    component_id: String,
    proto_name: String,
    capability_refs: BTreeMap<String, CapabilityRef>,
}

impl Controller {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Motor(_) => "motor",
            Self::Encoder(_) => "encoder",
            Self::Accelerometer(_) => "accelerometer",
            Self::Gyroscope(_) => "gyroscope",
            Self::Magnetometer(_) => "magnetometer",
            Self::Imu(_) => "imu",
            Self::Gnss(_) => "gnss",
            Self::Camera(_) => "camera",
            Self::Depth(_) => "depth",
            Self::Range(_) => "range",
            Self::Lidar(_) => "lidar",
            Self::Mmwave(_) => "mmwave",
            Self::Microphone(_) => "microphone",
            Self::Speaker(_) => "speaker",
            Self::Battery(_) => "battery",
            Self::Led(_) => "led",
        }
    }
}

impl ControllerContract {
    pub fn load() -> Result<Self> {
        let proto_root = staged_proto_root_from_current_exe()?;
        Self::load_from_paths(&proto_root)
    }
}

fn load_component_protos(proto_dir: &Path) -> Result<BTreeMap<String, ComponentProto>> {
    let mut component_protos = BTreeMap::new();

    let mut proto_paths = std::fs::read_dir(proto_dir)
        .with_context(|| {
            format!(
                "failed to list staged component protos in {}",
                proto_dir.display()
            )
        })?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("proto"))
        .collect::<Vec<_>>();
    proto_paths.sort();

    for proto_path in proto_paths {
        let proto_source = std::fs::read_to_string(&proto_path).with_context(|| {
            format!(
                "failed to read staged component PROTO {}",
                proto_path.display()
            )
        })?;
        let proto: webots_proto::Proto = proto_source.parse().with_context(|| {
            format!(
                "failed to parse staged component PROTO {}",
                proto_path.display()
            )
        })?;
        let proto_definition = proto.proto.as_ref().ok_or_else(|| {
            anyhow!(
                "staged component PROTO {} does not contain a PROTO definition",
                proto_path.display()
            )
        })?;
        let declared_capability_ids = proto_definition
            .fields
            .iter()
            .filter_map(|field| {
                field
                    .name
                    .strip_prefix("capability_name__")
                    .map(ToOwned::to_owned)
            })
            .collect::<Vec<_>>();

        let mut capabilities = BTreeMap::<String, Controller>::new();
        for line in proto_source.lines() {
            let line = line.trim();
            let Some(comment_body) = line.strip_prefix("# rf:") else {
                continue;
            };

            let mut capability_id = None;
            let mut controller = None;
            for token in comment_body.split_whitespace() {
                if let Some((key, value)) = token.split_once('=') {
                    match key {
                        "capability" => capability_id = Some(value.to_string()),
                        "controller" => controller = Some(value),
                        _ => {}
                    }
                }
            }

            let (Some(capability_id), Some(controller)) = (capability_id, controller) else {
                continue;
            };

            let controller = serde_json::from_str(controller).with_context(|| {
                format!(
                    "failed to parse controller config from {}",
                    proto_path.display()
                )
            })?;
            if capabilities
                .insert(capability_id.clone(), controller)
                .is_some()
            {
                return Err(anyhow!(
                    "staged component PROTO {} defines duplicate controller config for capability '{}'",
                    proto_path.display(),
                    capability_id
                ));
            }
        }

        for capability_id in &declared_capability_ids {
            if !capabilities.contains_key(capability_id)
                && !is_imu_device_name_field(capability_id, &capabilities)
            {
                return Err(anyhow!(
                    "staged component PROTO {} is missing controller config for capability '{}'",
                    proto_path.display(),
                    capability_id
                ));
            }
        }

        for capability_id in capabilities.keys() {
            if !declared_capability_ids
                .iter()
                .any(|declared| declared == capability_id)
            {
                return Err(anyhow!(
                    "staged component PROTO {} has controller config for undeclared capability '{}'",
                    proto_path.display(),
                    capability_id
                ));
            }
        }

        component_protos.insert(
            proto_definition.name.clone(),
            ComponentProto { capabilities },
        );
    }

    Ok(component_protos)
}

fn is_imu_device_name_field(
    capability_id: &str,
    capabilities: &BTreeMap<String, Controller>,
) -> bool {
    let Some(parent_id) = capability_id
        .strip_suffix("__accel")
        .or_else(|| capability_id.strip_suffix("__gyro"))
    else {
        return false;
    };

    matches!(capabilities.get(parent_id), Some(Controller::Imu(_)))
}

fn load_robot_proto_instances(proto_root: &Path) -> Result<Vec<RobotProtoInstance>> {
    let robot_proto_path = find_robot_proto_path(proto_root)?;
    let source = std::fs::read_to_string(&robot_proto_path).with_context(|| {
        format!(
            "failed to read staged robot proto {}",
            robot_proto_path.display()
        )
    })?;
    let document: webots_proto::Proto = source.parse().with_context(|| {
        format!(
            "failed to parse staged robot proto {}",
            robot_proto_path.display()
        )
    })?;
    let proto_definition = document.proto.as_ref().ok_or_else(|| {
        anyhow!(
            "staged robot proto {} does not contain a PROTO definition",
            robot_proto_path.display()
        )
    })?;

    let robot_node = proto_definition
        .body
        .iter()
        .find_map(|item| match item {
            webots_proto::ast::proto::ast::ProtoBodyItem::Node(node) => Some(node),
            webots_proto::ast::proto::ast::ProtoBodyItem::Template(_) => None,
        })
        .ok_or_else(|| {
            anyhow!(
                "staged robot proto '{}' has no body node",
                proto_definition.name
            )
        })?;

    let children = node_array_field(robot_node, "children").ok_or_else(|| {
        anyhow!(
            "staged robot proto '{}' root node is missing children field",
            proto_definition.name
        )
    })?;

    let mut instances = Vec::new();
    for element in &children.elements {
        let webots_proto::FieldValue::Node(node) = &element.value else {
            continue;
        };

        let webots_proto::ast::proto::ast::AstNodeKind::Node {
            type_name, fields, ..
        } = &node.kind
        else {
            continue;
        };

        let capability_refs = fields
            .iter()
            .filter_map(|element| match element {
                webots_proto::ast::proto::ast::NodeBodyElement::Field(field)
                    if field.name.starts_with("capability_name__") =>
                {
                    let local_capability_id =
                        field.name.strip_prefix("capability_name__")?.to_string();
                    let webots_proto::FieldValue::String(capability_id) = &field.value else {
                        return None;
                    };
                    capability_id
                        .parse::<CapabilityRef>()
                        .ok()
                        .map(|capability| (local_capability_id, capability))
                }
                _ => None,
            })
            .collect::<BTreeMap<_, _>>();
        if capability_refs.is_empty() {
            continue;
        }
        let component_id =
            component_id_from_capability_refs(&capability_refs).ok_or_else(|| {
                anyhow!(
                    "staged robot proto instance '{}' has invalid resolved capability names",
                    type_name
                )
            })?;

        instances.push(RobotProtoInstance {
            component_id,
            proto_name: type_name.clone(),
            capability_refs,
        });
    }

    Ok(instances)
}

fn find_robot_proto_path(proto_root: &Path) -> Result<PathBuf> {
    let mut robot_proto_paths = std::fs::read_dir(proto_root)
        .with_context(|| format!("failed to list staged proto root {}", proto_root.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.is_file()
                && path.extension().and_then(|extension| extension.to_str()) == Some("proto")
        })
        .collect::<Vec<_>>();
    robot_proto_paths.sort();

    match robot_proto_paths.as_slice() {
        [path] => Ok(path.clone()),
        [] => bail!("no staged robot proto found under {}", proto_root.display()),
        _ => bail!(
            "expected exactly one staged robot proto under {}, found {}",
            proto_root.display(),
            robot_proto_paths.len()
        ),
    }
}

fn node_array_field<'a>(
    node: &'a webots_proto::ast::proto::ast::AstNode,
    field_name: &str,
) -> Option<&'a webots_proto::ast::proto::ast::ArrayValue> {
    let webots_proto::ast::proto::ast::AstNodeKind::Node { fields, .. } = &node.kind else {
        return None;
    };
    fields.iter().find_map(|element| match element {
        webots_proto::ast::proto::ast::NodeBodyElement::Field(field)
            if field.name == field_name =>
        {
            match &field.value {
                webots_proto::FieldValue::Array(value) => Some(value),
                _ => None,
            }
        }
        _ => None,
    })
}

fn component_id_from_capability_refs(
    capability_refs: &BTreeMap<String, CapabilityRef>,
) -> Option<String> {
    let mut component_ids = capability_refs
        .values()
        .map(|capability| capability.component_id.as_str())
        .collect::<Vec<_>>();
    component_ids.sort_unstable();
    component_ids.dedup();

    match component_ids.as_slice() {
        [component_id] => Some((*component_id).to_string()),
        _ => None,
    }
}

pub fn staged_proto_root_from_current_exe() -> Result<PathBuf> {
    let current_exe =
        std::env::current_exe().context("failed to resolve controller executable path")?;
    let webots_root = current_exe
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| {
            anyhow!(
                "failed to derive staged Webots root from {}",
                current_exe.display()
            )
        })?;
    Ok(webots_root.join("protos"))
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use crate::capabilities::{encoder, lidar};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn reads_controller_configs_from_component_proto() -> Result<()> {
        let proto_dir = std::env::temp_dir().join(format!(
            "phoxal-simulator-webots-controller-component-proto-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&proto_dir)?;
        std::fs::write(
            proto_dir.join("Ddsm115.proto"),
            r#"#VRML_SIM R2025a utf8
# rf: capability=encoder controller={"kind":"encoder","publish_rate_hz":50.0,"sampling_period_hz":50.0,"gear_ratio":2.0,"encoder_type":"incremental","counts_per_revolution":1024}
# rf: capability=motor controller={"kind":"motor","gear_ratio":3.0,"actuator_type":"torque"}
PROTO Ddsm115 [
  hiddenField SFString capability_name__encoder "encoder"
  hiddenField SFString capability_name__motor "motor"
] {
  Group {
  }
}
"#,
        )?;

        let component_protos = load_component_protos(&proto_dir)?;
        let proto = component_protos.get("Ddsm115").expect("component proto");
        let encoder = proto.capabilities.get("encoder").expect("encoder config");
        let motor = proto.capabilities.get("motor").expect("motor config");

        match encoder {
            Controller::Encoder(config) => {
                assert_eq!(config.gear_ratio, 2.0);
                assert_eq!(config.counts_per_revolution, 1024);
                assert_eq!(config.encoder_type, encoder::EncoderType::Incremental);
            }
            controller => panic!("expected encoder config, got {controller:?}"),
        }

        match motor {
            Controller::Motor(config) => {
                assert_eq!(config.gear_ratio, 3.0);
                assert_eq!(config.actuator_type, ActuatorType::Torque);
            }
            controller => panic!("expected motor config, got {controller:?}"),
        }

        std::fs::remove_dir_all(&proto_dir)?;
        Ok(())
    }

    #[test]
    fn rejects_missing_controller_config_for_declared_capability() -> Result<()> {
        let proto_dir = std::env::temp_dir().join(format!(
            "phoxal-simulator-webots-controller-component-proto-missing-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&proto_dir)?;
        std::fs::write(
            proto_dir.join("Ddsm115.proto"),
            r#"#VRML_SIM R2025a utf8
# rf: capability=motor controller={"kind":"motor","gear_ratio":3.0,"actuator_type":"torque"}
PROTO Ddsm115 [
  hiddenField SFString capability_name__encoder "encoder"
  hiddenField SFString capability_name__motor "motor"
] {
  Group {
  }
}
"#,
        )?;

        let error = load_component_protos(&proto_dir).expect_err("should fail");
        assert!(
            error
                .to_string()
                .contains("missing controller config for capability 'encoder'")
        );

        std::fs::remove_dir_all(&proto_dir)?;
        Ok(())
    }

    #[test]
    fn imu_derived_device_name_fields_do_not_require_controller_configs() -> Result<()> {
        let proto_dir = std::env::temp_dir().join(format!(
            "phoxal-simulator-webots-controller-component-proto-imu-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&proto_dir)?;
        std::fs::write(
            proto_dir.join("ImuComponent.proto"),
            r#"#VRML_SIM R2025a utf8
# rf: capability=imu controller={"kind":"imu","publish_rate_hz":100.0,"sampling_period_hz":100.0}
PROTO ImuComponent [
  hiddenField SFString capability_name__imu "imu"
  hiddenField SFString capability_name__imu__accel "imu__accel"
  hiddenField SFString capability_name__imu__gyro "imu__gyro"
] {
  Group {
  }
}
"#,
        )?;

        let component_protos = load_component_protos(&proto_dir)?;
        let proto = component_protos
            .get("ImuComponent")
            .expect("component proto");
        assert!(matches!(
            proto.capabilities.get("imu"),
            Some(Controller::Imu(_))
        ));
        assert!(!proto.capabilities.contains_key("imu__accel"));
        assert!(!proto.capabilities.contains_key("imu__gyro"));

        std::fs::remove_dir_all(&proto_dir)?;
        Ok(())
    }

    #[test]
    fn composes_robot_and_component_proto_configs() -> Result<()> {
        let root = std::env::temp_dir().join(format!(
            "phoxal-simulator-webots-controller-contract-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after epoch")
                .as_nanos()
        ));
        let component_dir = root.join("components");
        std::fs::create_dir_all(&component_dir)?;

        std::fs::write(
            component_dir.join("Sensor.proto"),
            r#"#VRML_SIM R2025a utf8
# rf: capability=depth controller={"kind":"depth","publish_rate_hz":20.0,"sampling_period_hz":100.0}
# rf: capability=range controller={"kind":"range","publish_rate_hz":20.0,"sampling_period_hz":20.0}
# rf: capability=lidar controller={"kind":"lidar","publish_rate_hz":10.0,"sampling_period_hz":50.0,"output":"points"}
PROTO Sensor [
  hiddenField SFString capability_name__depth "depth"
  hiddenField SFString capability_name__range "range"
  hiddenField SFString capability_name__lidar "lidar"
] {
  Group {
  }
}
"#,
        )?;
        std::fs::write(
            root.join("RobotV1.proto"),
            r#"#VRML_SIM R2025a utf8
PROTO RobotV1 [
] {
  Robot {
    children [
      Sensor {
        capability_name__depth "front.depth"
        capability_name__range "front.range"
        capability_name__lidar "front.lidar"
      }
    ]
  }
}
"#,
        )?;

        let contract = ControllerContract::load_from_paths(&root)?;
        assert_eq!(contract.components.len(), 1);

        let component = &contract.components[0];
        assert_eq!(component.component_id, "front");
        assert_eq!(component.proto_name, "Sensor");

        let depth = component
            .capabilities
            .iter()
            .find(|capability| capability.capability == CapabilityRef::new("front", "depth"))
            .expect("depth capability");
        match &depth.controller {
            Controller::Depth(config) => {
                assert_eq!(config.publish_rate_hz, 20.0);
                assert_eq!(config.sampling_period_hz, 100.0);
            }
            controller => panic!("expected depth config, got {controller:?}"),
        }

        let lidar = component
            .capabilities
            .iter()
            .find(|capability| capability.capability == CapabilityRef::new("front", "lidar"))
            .expect("lidar capability");
        match &lidar.controller {
            Controller::Lidar(config) => {
                assert_eq!(config.output, lidar::ScanMode::Points);
            }
            controller => panic!("expected lidar config, got {controller:?}"),
        }

        let range = component
            .capabilities
            .iter()
            .find(|capability| capability.capability == CapabilityRef::new("front", "range"))
            .expect("range capability");
        match &range.controller {
            Controller::Range(config) => {
                assert_eq!(config.publish_rate_hz, 20.0);
                assert_eq!(config.sampling_period_hz, 20.0);
            }
            controller => panic!("expected range config, got {controller:?}"),
        }

        std::fs::remove_dir_all(&root)?;
        Ok(())
    }
}

impl ControllerContract {
    fn load_from_paths(proto_root: &Path) -> Result<Self> {
        let component_protos = load_component_protos(&proto_root.join("components"))?;
        let robot_instances = load_robot_proto_instances(proto_root)?;

        let mut components = Vec::with_capacity(robot_instances.len());
        for instance in robot_instances {
            let component_proto = component_protos.get(&instance.proto_name).ok_or_else(|| {
                anyhow!(
                    "missing staged component config for proto '{}'",
                    instance.proto_name
                )
            })?;
            let mut capabilities = Vec::with_capacity(component_proto.capabilities.len());

            for (local_capability_id, controller) in &component_proto.capabilities {
                let capability = instance
                    .capability_refs
                    .get(local_capability_id)
                    .ok_or_else(|| {
                        anyhow!(
                            "robot proto instance '{}' ({}) is missing resolved capability name for '{}'",
                            instance.component_id,
                            instance.proto_name,
                            local_capability_id
                        )
                    })?;
                capabilities.push(Capability {
                    capability: capability.clone(),
                    controller: controller.clone(),
                });
            }

            components.push(Component {
                component_id: instance.component_id,
                proto_name: instance.proto_name,
                capabilities,
            });
        }

        Ok(Self { components })
    }
}
