use super::Command;
use super::output::Publish;
use crate::capabilities::{
    accelerometer::Accelerometer,
    battery::{Battery, Config as BatteryConfig},
    camera::Camera,
    depth::{Config as DepthConfig, Depth},
    downsample::{RateDecimation, downsample_camera_frame, downsample_depth_frame},
    encoder::Encoder,
    gnss::Gnss,
    gyroscope::Gyroscope,
    imu::Imu,
    led::{Config as LedConfig, Led},
    lidar::{Config as LidarConfig, Lidar, ScanMode as LidarScanMode},
    magnetometer::Magnetometer,
    microphone::Microphone,
    mmwave::Mmwave,
    motor::{Config as MotorConfig, Motor},
    range::{Config as RangeConfig, Range},
    sample_period_ms,
};
use crate::webots::controller::{Capability, Controller, ControllerContract};
use anyhow::{Result, anyhow};
use phoxal::api::component::capability::profile::v1::{
    CameraProfileSpec, DepthProfileSpec, ParsedCameraProfileSpec, ParsedDepthProfileSpec, ProfileId,
};
use phoxal::api::simulation::clock::v1::Clock;
use phoxal::model::component::v1::CapabilityRef;
use std::collections::{BTreeMap, BTreeSet};
use tracing::{debug, info, warn};

pub struct Webots {
    webots: webots_rs::Webots,
    actuators: BTreeMap<CapabilityRef, Actuator>,
    sensors: BTreeMap<CapabilityRef, Sensor>,
    step_ms: i32,
    dt_ns: u64,
}

impl Webots {
    pub fn new(webots: webots_rs::Webots, contract: &ControllerContract) -> Result<Self> {
        let step_ms = webots
            .get_basic_time_step()
            .map_err(|error| anyhow!(error))? as i32;
        let dt_ns = std::time::Duration::from_millis(step_ms as u64).as_nanos() as u64;

        let mut actuators = BTreeMap::new();
        let mut sensors = BTreeMap::new();

        for component in &contract.components {
            for capability in &component.capabilities {
                debug!(
                    capability = %capability.capability,
                    component_id = %component.component_id,
                    proto_name = %component.proto_name,
                    controller_kind = capability.controller.kind(),
                    "loading capability from staged proto config"
                );

                match CapabilityRegistration::new(&webots, step_ms, capability)? {
                    Some(CapabilityRegistration::Actuator(actuator)) => {
                        actuators.insert(capability.capability.clone(), actuator);
                    }
                    Some(CapabilityRegistration::Sensor(sensor)) => {
                        sensors.insert(capability.capability.clone(), sensor);
                    }
                    None => {}
                }
            }
        }

        if actuators.is_empty() && sensors.is_empty() {
            warn!("No resolved capabilities found in bundled component config");
        } else {
            info!(
                actuator_count = actuators.len(),
                sensor_count = sensors.len(),
                "Configured Webots capabilities"
            );
        }

        Ok(Self {
            webots,
            actuators,
            sensors,
            step_ms,
            dt_ns,
        })
    }

    pub const fn dt_ns(&self) -> u64 {
        self.dt_ns
    }

    pub fn advance(
        &mut self,
        step: Clock,
        commands: Vec<Command>,
        demanded_capabilities: &BTreeSet<CapabilityRef>,
        requested_profiles: &BTreeMap<CapabilityRef, BTreeSet<ProfileId>>,
    ) -> Result<Option<Vec<Publish>>> {
        self.advance_one_step(
            self.step_ms,
            step,
            commands,
            demanded_capabilities,
            requested_profiles,
        )
    }

    fn apply_command(&mut self, command: Command) {
        let Some(actuator) = self.actuators.get_mut(command.capability()) else {
            return;
        };

        match (actuator, command) {
            (
                Actuator::Motor(motor),
                Command::Motor {
                    capability,
                    payload,
                },
            ) => {
                if let Err(error) = motor.apply(&payload) {
                    warn!(capability = %capability, error = %error, "failed to apply motor command");
                }
            }
            (
                Actuator::Led(led),
                Command::Led {
                    capability,
                    payload,
                },
            ) => {
                if let Err(error) = led.apply(&payload) {
                    warn!(capability = %capability, error = %error, "failed to apply led command");
                }
            }
            _ => {}
        }
    }

    fn advance_one_step(
        &mut self,
        step_ms: i32,
        step: Clock,
        commands: Vec<Command>,
        demanded_capabilities: &BTreeSet<CapabilityRef>,
        requested_profiles: &BTreeMap<CapabilityRef, BTreeSet<ProfileId>>,
    ) -> Result<Option<Vec<Publish>>> {
        for command in commands {
            self.apply_command(command);
        }

        if !self.webots.step(step_ms).map_err(|error| anyhow!(error))? {
            return Ok(None);
        }

        let mut publishes = Vec::new();

        for (capability, sensor) in &mut self.sensors {
            let empty_profiles = BTreeSet::new();
            let requested_for_capability = requested_profiles
                .get(capability)
                .unwrap_or(&empty_profiles);
            publishes.extend(sensor.read_outputs(
                capability,
                step.step(),
                step.time_ns(),
                demanded_capabilities,
                requested_for_capability,
                self.dt_ns,
            )?);
        }

        Ok(Some(publishes))
    }
}

enum CapabilityRegistration {
    Actuator(Actuator),
    Sensor(Sensor),
}

enum Actuator {
    Motor(Motor),
    Led(Led),
}

enum Sensor {
    Encoder(Encoder),
    Accelerometer(Accelerometer),
    Battery(Battery),
    Camera(Camera),
    Depth(Depth),
    Gnss(Gnss),
    Gyroscope(Gyroscope),
    Imu(Imu),
    Range(Range),
    Lidar(Lidar),
    Magnetometer(Magnetometer),
    Microphone(Microphone),
    Mmwave(Mmwave),
}

impl CapabilityRegistration {
    fn new(
        webots: &webots_rs::Webots,
        step_ms: i32,
        capability: &Capability,
    ) -> Result<Option<Self>> {
        let capability_id = capability.capability.to_string();
        let registration = match &capability.controller {
            Controller::Motor(config) => Self::Actuator(Actuator::Motor(Self::new_motor(
                webots,
                &capability_id,
                config,
            )?)),
            Controller::Encoder(config) => Self::Sensor(Sensor::Encoder(Self::sampled_sensor(
                webots,
                &capability_id,
                step_ms,
                config,
                |config| config.sampling_period_hz,
                |webots, capability_id| {
                    webots
                        .position_sensor(capability_id)
                        .map_err(|error| anyhow!(error))
                },
                Encoder::new,
            )?)),
            Controller::Accelerometer(config) => {
                Self::Sensor(Sensor::Accelerometer(Self::sampled_sensor(
                    webots,
                    &capability_id,
                    step_ms,
                    config,
                    |config| config.sampling_period_hz,
                    |webots, capability_id| {
                        webots
                            .accelerometer(capability_id)
                            .map_err(|error| anyhow!(error))
                    },
                    Accelerometer::new,
                )?))
            }
            Controller::Battery(config) => {
                Self::Sensor(Sensor::Battery(Self::new_battery(step_ms, config)?))
            }
            Controller::Camera(config) => Self::Sensor(Sensor::Camera(Self::sampled_sensor(
                webots,
                &capability_id,
                step_ms,
                config,
                |config| config.sampling_period_hz,
                |webots, capability_id| {
                    webots.camera(capability_id).map_err(|error| anyhow!(error))
                },
                Camera::new,
            )?)),
            Controller::Depth(config) => {
                Self::new_depth_sensor(webots, &capability_id, step_ms, config)?
            }
            Controller::Range(config) => {
                Self::new_range_sensor(webots, &capability_id, step_ms, config)?
            }
            Controller::Gnss(config) => Self::Sensor(Sensor::Gnss(Self::sampled_sensor(
                webots,
                &capability_id,
                step_ms,
                config,
                |config| config.sampling_period_hz,
                |webots, capability_id| webots.gps(capability_id).map_err(|error| anyhow!(error)),
                Gnss::new,
            )?)),
            Controller::Gyroscope(config) => Self::Sensor(Sensor::Gyroscope(Self::sampled_sensor(
                webots,
                &capability_id,
                step_ms,
                config,
                |config| config.sampling_period_hz,
                |webots, capability_id| webots.gyro(capability_id).map_err(|error| anyhow!(error)),
                Gyroscope::new,
            )?)),
            Controller::Imu(config) => Self::Sensor(Sensor::Imu(Self::new_imu(
                webots,
                &capability_id,
                step_ms,
                config,
            )?)),
            Controller::Lidar(config) => Self::Sensor(Sensor::Lidar(Self::new_lidar(
                webots,
                &capability_id,
                step_ms,
                config,
            )?)),
            Controller::Led(config) => Self::Actuator(Actuator::Led(Self::new_led(
                webots,
                &capability_id,
                config,
            )?)),
            Controller::Magnetometer(config) => {
                Self::Sensor(Sensor::Magnetometer(Self::sampled_sensor(
                    webots,
                    &capability_id,
                    step_ms,
                    config,
                    |config| config.sampling_period_hz,
                    |webots, capability_id| {
                        webots
                            .compass(capability_id)
                            .map_err(|error| anyhow!(error))
                    },
                    Magnetometer::new,
                )?))
            }
            Controller::Microphone(config) => {
                Self::Sensor(Sensor::Microphone(Self::sampled_sensor(
                    webots,
                    &capability_id,
                    step_ms,
                    config,
                    |config| config.sampling_period_hz,
                    |webots, capability_id| {
                        webots
                            .microphone(capability_id)
                            .map_err(|error| anyhow!(error))
                    },
                    Microphone::new,
                )?))
            }
            Controller::Mmwave(config) => Self::Sensor(Sensor::Mmwave(Self::sampled_sensor(
                webots,
                &capability_id,
                step_ms,
                config,
                |config| config.sampling_period_hz,
                |webots, capability_id| webots.radar(capability_id).map_err(|error| anyhow!(error)),
                Mmwave::new,
            )?)),
            Controller::Speaker(_) => {
                return Self::unsupported(&capability.capability, "speaker");
            }
        };

        Ok(Some(registration))
    }

    fn new_motor(
        webots: &webots_rs::Webots,
        capability_id: &str,
        config: &MotorConfig,
    ) -> Result<Motor> {
        let motor = webots
            .motor(capability_id)
            .map_err(|error| anyhow!(error))?;
        Motor::new(motor, config)
    }

    fn new_led(webots: &webots_rs::Webots, capability_id: &str, config: &LedConfig) -> Result<Led> {
        let led = webots.led(capability_id).map_err(|error| anyhow!(error))?;
        Ok(Led::new(led, config))
    }

    fn new_battery(step_ms: i32, config: &BatteryConfig) -> Result<Battery> {
        Battery::new(
            webots_rs::device::battery_sensor::BatterySensor::new(),
            step_ms,
            sample_period_ms(config.publish_rate_hz)?,
            config,
        )
    }

    fn new_lidar(
        webots: &webots_rs::Webots,
        capability_id: &str,
        step_ms: i32,
        config: &LidarConfig,
    ) -> Result<Lidar> {
        let lidar = webots
            .lidar(
                capability_id,
                webots_rs::device::lidar::LidarConfig::new()
                    .with_point_cloud(matches!(config.output, LidarScanMode::Points)),
            )
            .map_err(|error| anyhow!(error))?;
        Lidar::new(
            lidar,
            step_ms,
            sample_period_ms(config.sampling_period_hz)?,
            config,
        )
    }

    fn new_depth_sensor(
        webots: &webots_rs::Webots,
        capability_id: &str,
        step_ms: i32,
        config: &DepthConfig,
    ) -> Result<Self> {
        Ok(Self::Sensor(Sensor::Depth(Self::sampled_sensor(
            webots,
            capability_id,
            step_ms,
            config,
            |config| config.sampling_period_hz,
            |webots, capability_id| {
                webots
                    .range_finder(capability_id)
                    .map_err(|error| anyhow!(error))
            },
            Depth::new,
        )?)))
    }

    fn new_range_sensor(
        webots: &webots_rs::Webots,
        capability_id: &str,
        step_ms: i32,
        config: &RangeConfig,
    ) -> Result<Self> {
        Ok(Self::Sensor(Sensor::Range(Self::sampled_sensor(
            webots,
            capability_id,
            step_ms,
            config,
            |config| config.sampling_period_hz,
            |webots, capability_id| {
                webots
                    .distance_sensor(capability_id)
                    .map_err(|error| anyhow!(error))
            },
            Range::new,
        )?)))
    }

    fn new_imu(
        webots: &webots_rs::Webots,
        capability_id: &str,
        step_ms: i32,
        config: &crate::capabilities::imu::Config,
    ) -> Result<Imu> {
        let sample_period_ms = sample_period_ms(config.sampling_period_hz)?;
        let accelerometer_id = imu_accelerometer_device_id(capability_id);
        let gyro_id = imu_gyroscope_device_id(capability_id);
        let inertial_unit = webots
            .inertial_unit(capability_id)
            .map_err(|error| anyhow!(error))?;
        let accelerometer = webots
            .accelerometer(accelerometer_id)
            .map_err(|error| anyhow!(error))?;
        let gyro = webots.gyro(gyro_id).map_err(|error| anyhow!(error))?;
        Imu::new(
            inertial_unit,
            accelerometer,
            gyro,
            step_ms,
            sample_period_ms,
            config,
        )
    }

    fn sampled_sensor<Handle, Config, Device, FRate, FGet, FBuild>(
        webots: &webots_rs::Webots,
        capability_id: &str,
        step_ms: i32,
        config: &Config,
        sampling_period_hz: FRate,
        get_handle: FGet,
        build: FBuild,
    ) -> Result<Device>
    where
        FRate: FnOnce(&Config) -> f64,
        FGet: FnOnce(&webots_rs::Webots, &str) -> Result<Handle>,
        FBuild: FnOnce(Handle, i32, i32, &Config) -> Result<Device>,
    {
        let handle = get_handle(webots, capability_id)?;
        build(
            handle,
            step_ms,
            sample_period_ms(sampling_period_hz(config))?,
            config,
        )
    }

    fn unsupported(capability: &CapabilityRef, capability_kind: &str) -> Result<Option<Self>> {
        warn!(
            capability = %capability,
            "{capability_kind} is not wired into the rewritten bridge yet"
        );
        Ok(None)
    }
}

fn imu_accelerometer_device_id(capability_id: &str) -> String {
    format!("{capability_id}__accel")
}

fn imu_gyroscope_device_id(capability_id: &str) -> String {
    format!("{capability_id}__gyro")
}

impl Sensor {
    fn read_outputs(
        &mut self,
        capability: &CapabilityRef,
        step_count: u64,
        time_ns: u64,
        demanded_capabilities: &BTreeSet<CapabilityRef>,
        requested_profiles: &BTreeSet<ProfileId>,
        dt_ns: u64,
    ) -> Result<Vec<Publish>> {
        if !should_capture(
            self.kind(),
            capability,
            demanded_capabilities,
            requested_profiles,
        ) {
            return Ok(Vec::new());
        }

        let default_profile = ProfileId::default_profile();
        let publish = match self {
            Self::Encoder(sensor) => {
                sensor
                    .read_if_due(step_count, time_ns)?
                    .map(|payload| Publish::Encoder {
                        capability: capability.clone(),
                        profile_id: default_profile.clone(),
                        at_ns: time_ns,
                        payload,
                    })
            }
            Self::Accelerometer(sensor) => {
                sensor
                    .read_if_due(step_count, time_ns)?
                    .map(|payload| Publish::Accelerometer {
                        capability: capability.clone(),
                        profile_id: default_profile.clone(),
                        at_ns: time_ns,
                        payload,
                    })
            }
            Self::Battery(sensor) => {
                sensor
                    .read_if_due(step_count, time_ns)?
                    .map(|payload| Publish::Battery {
                        capability: capability.clone(),
                        profile_id: default_profile.clone(),
                        at_ns: time_ns,
                        payload,
                    })
            }
            Self::Camera(sensor) => {
                sensor
                    .read_if_due(step_count, time_ns)?
                    .map(|payload| Publish::Camera {
                        capability: capability.clone(),
                        profile_id: default_profile.clone(),
                        at_ns: time_ns,
                        payload,
                    })
            }
            Self::Depth(sensor) => {
                sensor
                    .read_if_due(step_count, time_ns)?
                    .map(|payload| Publish::Depth {
                        capability: capability.clone(),
                        profile_id: default_profile.clone(),
                        at_ns: time_ns,
                        payload,
                    })
            }
            Self::Range(sensor) => {
                sensor
                    .read_if_due(step_count, time_ns)?
                    .map(|payload| Publish::Range {
                        capability: capability.clone(),
                        profile_id: default_profile.clone(),
                        at_ns: time_ns,
                        payload,
                    })
            }
            Self::Gnss(sensor) => {
                sensor
                    .read_if_due(step_count, time_ns)?
                    .map(|payload| Publish::Gnss {
                        capability: capability.clone(),
                        profile_id: default_profile.clone(),
                        at_ns: time_ns,
                        payload,
                    })
            }
            Self::Gyroscope(sensor) => {
                sensor
                    .read_if_due(step_count, time_ns)?
                    .map(|payload| Publish::Gyroscope {
                        capability: capability.clone(),
                        profile_id: default_profile.clone(),
                        at_ns: time_ns,
                        payload,
                    })
            }
            Self::Imu(sensor) => {
                sensor
                    .read_if_due(step_count, time_ns)?
                    .map(|payload| Publish::Imu {
                        capability: capability.clone(),
                        profile_id: default_profile.clone(),
                        at_ns: time_ns,
                        payload,
                    })
            }
            Self::Lidar(sensor) => {
                sensor
                    .read_if_due(step_count, time_ns)?
                    .map(|payload| Publish::Lidar {
                        capability: capability.clone(),
                        profile_id: default_profile.clone(),
                        at_ns: time_ns,
                        payload,
                    })
            }
            Self::Magnetometer(sensor) => {
                sensor
                    .read_if_due(step_count, time_ns)?
                    .map(|payload| Publish::Magnetometer {
                        capability: capability.clone(),
                        profile_id: default_profile.clone(),
                        at_ns: time_ns,
                        payload,
                    })
            }
            Self::Microphone(sensor) => {
                sensor
                    .read_if_due(step_count, time_ns)?
                    .map(|payload| Publish::Microphone {
                        capability: capability.clone(),
                        profile_id: default_profile.clone(),
                        at_ns: time_ns,
                        payload,
                    })
            }
            Self::Mmwave(sensor) => {
                sensor
                    .read_if_due(step_count, time_ns)?
                    .map(|payload| Publish::Mmwave {
                        capability: capability.clone(),
                        profile_id: default_profile.clone(),
                        at_ns: time_ns,
                        payload,
                    })
            }
        };
        let Some(native) = publish else {
            return Ok(Vec::new());
        };

        let mut publishes = Vec::new();
        if should_publish_default(self.kind(), capability, demanded_capabilities) {
            publishes.push(native.clone());
        }
        publishes.extend(self.derive_requested_profiles(
            capability,
            &native,
            step_count,
            requested_profiles,
            dt_ns,
        ));
        Ok(publishes)
    }

    const fn kind(&self) -> &'static str {
        match self {
            Self::Encoder(_) => phoxal::api::component::capability::encoder::v1::KIND,
            Self::Accelerometer(_) => phoxal::api::component::capability::accelerometer::v1::KIND,
            Self::Battery(_) => phoxal::api::component::capability::battery::v1::KIND,
            Self::Camera(_) => phoxal::api::component::capability::camera::v1::KIND,
            Self::Depth(_) => phoxal::api::component::capability::depth::v1::KIND,
            Self::Range(_) => phoxal::api::component::capability::range::v1::KIND,
            Self::Gnss(_) => phoxal::api::component::capability::gnss::v1::KIND,
            Self::Gyroscope(_) => phoxal::api::component::capability::gyroscope::v1::KIND,
            Self::Imu(_) => phoxal::api::component::capability::imu::v1::KIND,
            Self::Lidar(_) => phoxal::api::component::capability::lidar::v1::KIND,
            Self::Magnetometer(_) => phoxal::api::component::capability::magnetometer::v1::KIND,
            Self::Microphone(_) => phoxal::api::component::capability::microphone::v1::KIND,
            Self::Mmwave(_) => phoxal::api::component::capability::mmwave::v1::KIND,
        }
    }

    fn derive_requested_profiles(
        &self,
        capability: &CapabilityRef,
        native: &Publish,
        step_count: u64,
        requested_profiles: &BTreeSet<ProfileId>,
        dt_ns: u64,
    ) -> Vec<Publish> {
        let mut publishes = Vec::new();
        for profile_id in requested_profiles {
            match (self, native) {
                (Self::Camera(sensor), Publish::Camera { at_ns, payload, .. }) => {
                    if !profile_rate_due(
                        profile_id,
                        sensor.publish_every_steps(),
                        step_count,
                        dt_ns,
                        camera_profile_rate_hz,
                    ) {
                        continue;
                    }
                    let profile = match camera_profile(profile_id) {
                        Ok(profile) => profile,
                        Err(error) => {
                            warn!(capability = %capability, profile_id = %profile_id, error = %error, "ignored invalid camera profile request");
                            continue;
                        }
                    };
                    match downsample_camera_frame(payload, &profile, None) {
                        Ok(frame) => publishes.push(Publish::Camera {
                            capability: capability.clone(),
                            profile_id: profile_id.clone(),
                            at_ns: *at_ns,
                            payload: frame,
                        }),
                        Err(error) => warn!(
                            capability = %capability,
                            profile_id = %profile_id,
                            error = %error,
                            "ignored camera profile request outside native envelope"
                        ),
                    }
                }
                (Self::Depth(sensor), Publish::Depth { at_ns, payload, .. }) => {
                    if !profile_rate_due(
                        profile_id,
                        sensor.publish_every_steps(),
                        step_count,
                        dt_ns,
                        depth_profile_rate_hz,
                    ) {
                        continue;
                    }
                    let profile = match depth_profile(profile_id) {
                        Ok(profile) => profile,
                        Err(error) => {
                            warn!(capability = %capability, profile_id = %profile_id, error = %error, "ignored invalid depth profile request");
                            continue;
                        }
                    };
                    match downsample_depth_frame(
                        payload,
                        sensor.width(),
                        sensor.height(),
                        &profile,
                        None,
                    ) {
                        Ok(depth) => publishes.push(Publish::Depth {
                            capability: capability.clone(),
                            profile_id: profile_id.clone(),
                            at_ns: *at_ns,
                            payload: depth.with_resolution(profile.width_px, profile.height_px),
                        }),
                        Err(error) => warn!(
                            capability = %capability,
                            profile_id = %profile_id,
                            error = %error,
                            "ignored depth profile request outside native envelope"
                        ),
                    }
                }
                _ => {}
            }
        }
        publishes
    }
}

fn should_capture(
    kind: &'static str,
    capability: &CapabilityRef,
    demanded_capabilities: &BTreeSet<CapabilityRef>,
    requested_profiles: &BTreeSet<ProfileId>,
) -> bool {
    if kind == phoxal::api::component::capability::camera::v1::KIND
        || kind == phoxal::api::component::capability::depth::v1::KIND
    {
        demanded_capabilities.contains(capability) || !requested_profiles.is_empty()
    } else {
        true
    }
}

fn should_publish_default(
    kind: &'static str,
    capability: &CapabilityRef,
    demanded_capabilities: &BTreeSet<CapabilityRef>,
) -> bool {
    if kind == phoxal::api::component::capability::camera::v1::KIND
        || kind == phoxal::api::component::capability::depth::v1::KIND
    {
        demanded_capabilities.contains(capability)
    } else {
        true
    }
}

fn camera_profile(profile_id: &ProfileId) -> Result<CameraProfileSpec> {
    match CameraProfileSpec::from_profile_id(profile_id)? {
        ParsedCameraProfileSpec::Spec(profile) => Ok(profile),
        ParsedCameraProfileSpec::Native => anyhow::bail!("default profile is not downsampled"),
    }
}

fn depth_profile(profile_id: &ProfileId) -> Result<DepthProfileSpec> {
    match DepthProfileSpec::from_profile_id(profile_id)? {
        ParsedDepthProfileSpec::Spec(profile) => Ok(profile),
        ParsedDepthProfileSpec::Native => anyhow::bail!("default profile is not downsampled"),
    }
}

fn camera_profile_rate_hz(profile_id: &ProfileId) -> Result<f64> {
    Ok(camera_profile(profile_id)?.publish_rate_hz)
}

fn depth_profile_rate_hz(profile_id: &ProfileId) -> Result<f64> {
    Ok(depth_profile(profile_id)?.publish_rate_hz)
}

fn profile_rate_due(
    profile_id: &ProfileId,
    publish_every_steps: u64,
    step_count: u64,
    dt_ns: u64,
    target_rate: impl FnOnce(&ProfileId) -> Result<f64>,
) -> bool {
    let native_rate_hz = 1_000_000_000.0 / (dt_ns as f64 * publish_every_steps as f64);
    let target_rate_hz = match target_rate(profile_id) {
        Ok(rate_hz) => rate_hz,
        Err(error) => {
            warn!(profile_id = %profile_id, error = %error, "ignored invalid requested profile rate");
            return false;
        }
    };
    let decimation = match RateDecimation::new(native_rate_hz, target_rate_hz) {
        Ok(decimation) => decimation,
        Err(error) => {
            warn!(
                profile_id = %profile_id,
                error = %error,
                "ignored requested profile rate above native envelope"
            );
            return false;
        }
    };
    decimation.should_emit(native_frame_index(step_count, publish_every_steps))
}

fn native_frame_index(step_count: u64, publish_every_steps: u64) -> u64 {
    step_count
        .checked_div(publish_every_steps)
        .unwrap_or_default()
        .saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use super::should_capture;
    use phoxal::api::component::capability::profile::v1::ProfileId;
    use phoxal::model::component::v1::CapabilityRef;
    use std::collections::BTreeSet;

    #[test]
    fn camera_capture_requires_matching_demand() {
        let capability = CapabilityRef::new("front_camera", "rgb");
        let mut demanded = BTreeSet::new();

        assert!(!should_capture(
            phoxal::api::component::capability::camera::v1::KIND,
            &capability,
            &demanded,
            &BTreeSet::new()
        ));

        demanded.insert(capability.clone());

        assert!(should_capture(
            phoxal::api::component::capability::camera::v1::KIND,
            &capability,
            &demanded,
            &BTreeSet::new()
        ));
    }

    #[test]
    fn camera_capture_accepts_requested_profile_demand() {
        let capability = CapabilityRef::new("front_camera", "rgb");
        let demanded = BTreeSet::new();
        let mut requested = BTreeSet::new();
        requested.insert(ProfileId::new("r320x240_h5_rgb8").unwrap());

        assert!(should_capture(
            phoxal::api::component::capability::camera::v1::KIND,
            &capability,
            &demanded,
            &requested
        ));
    }

    #[test]
    fn depth_capture_requires_matching_demand() {
        let capability = CapabilityRef::new("front_camera", "depth");
        let mut demanded = BTreeSet::new();

        assert!(!should_capture(
            phoxal::api::component::capability::depth::v1::KIND,
            &capability,
            &demanded,
            &BTreeSet::new()
        ));

        demanded.insert(capability.clone());

        assert!(should_capture(
            phoxal::api::component::capability::depth::v1::KIND,
            &capability,
            &demanded,
            &BTreeSet::new()
        ));
    }

    #[test]
    fn low_bandwidth_capture_ignores_matching_demand() {
        let capability = CapabilityRef::new("imu", "imu");
        let demanded = BTreeSet::new();

        assert!(should_capture(
            phoxal::api::component::capability::imu::v1::KIND,
            &capability,
            &demanded,
            &BTreeSet::new()
        ));
    }
}
