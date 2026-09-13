//! Immutable compiled models and model-scoped read-only handles.

use std::ffi::CString;
use std::fmt;
use std::sync::Arc;

use mujoco_rs::prelude::{MjModel, MjtJoint, MjtObj};
use mujoco_rs::wrappers::MjVfs;
use phoxal_port::{PortDescriptor, PortKind, PortSignature};

use crate::artifact::ClosedModel;
use crate::error::ModelError;
use crate::scene::StateSnapshot;

/// An immutable compiled MuJoCo model and the closed artifact that produced it.
///
/// MuJoCo's model is never exposed mutably through this type.
/// An application that needs to edit or recompile a model must create a new
/// [`ClosedModel`] and a new [`Model`], which keeps a running scene's model
/// identity stable.
#[derive(Clone, Debug)]
pub struct Model {
    inner: Arc<MjModel>,
    artifact: Arc<ClosedModel>,
    identity: ModelIdentity,
}

impl Model {
    /// Compiles a closed MJCF/resource artifact with the pinned native binding.
    ///
    /// All resources are inserted into MuJoCo's VFS before parsing.
    /// The native parser therefore cannot fall back to an arbitrary filesystem
    /// path while resolving the supplied closure.
    ///
    /// # Errors
    ///
    /// Returns [`ModelError`] when artifact admission, native parsing, or native
    /// compilation fails.
    pub fn from_closed(artifact: ClosedModel) -> Result<Self, ModelError> {
        let mut vfs = MjVfs::new();
        for resource in artifact.resources() {
            vfs.add_from_buffer(resource.name(), resource.bytes())
                .map_err(|error| native_error("resource admission", error))?;
        }

        let model = MjModel::from_xml_vfs(artifact.entry(), &vfs)
            .map_err(|error| native_error("MJCF compile", error))?;
        let timestep = model.opt().timestep;
        if !timestep.is_finite() || timestep <= 0.0 {
            return Err(ModelError::InvalidTimestep(timestep));
        }

        Ok(Self {
            inner: Arc::new(model),
            identity: ModelIdentity(artifact.digest()),
            artifact: Arc::new(artifact),
        })
    }

    /// Builds a model from one in-memory MJCF document.
    pub fn from_xml(xml: impl AsRef<[u8]>) -> Result<Self, ModelError> {
        Self::from_closed(ClosedModel::from_xml(xml)?)
    }

    /// Reads an explicit model directory and compiles its closed resource set.
    pub fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self, ModelError> {
        Self::from_closed(ClosedModel::from_file(path)?)
    }

    /// Returns the immutable source/resource closure used to compile this model.
    #[must_use]
    pub fn artifact(&self) -> &ClosedModel {
        &self.artifact
    }

    /// Returns the deterministic identity of the source/resource closure.
    #[must_use]
    pub const fn identity(&self) -> ModelIdentity {
        self.identity
    }

    /// Returns the MuJoCo version linked by the pinned binding.
    #[must_use]
    pub fn native_version() -> &'static str {
        mujoco_rs::mujoco_version()
    }

    /// Returns the source-authored native physics timestep in seconds.
    #[must_use]
    pub fn timestep(&self) -> f64 {
        self.inner.opt().timestep
    }

    /// Returns the compiled model table sizes.
    #[must_use]
    pub fn counts(&self) -> ModelCounts {
        ModelCounts {
            qpos: self.inner.nq() as usize,
            qvel: self.inner.nv() as usize,
            controls: self.inner.nu() as usize,
            actuators: self.inner.nactuator() as usize,
            bodies: self.inner.nbody() as usize,
            joints: self.inner.njnt() as usize,
            sites: self.inner.nsite() as usize,
            cameras: self.inner.ncam() as usize,
            sensors: self.inner.nsensor() as usize,
            sensor_values: self.inner.nsensordata() as usize,
        }
    }

    /// Looks up a body by its model-local name.
    pub fn body(&self, name: &str) -> Result<Option<BodyHandle>, ModelError> {
        Ok(self.find(name, ObjectKind::Body)?.map(BodyHandle))
    }

    /// Looks up a joint by its model-local name.
    pub fn joint(&self, name: &str) -> Result<Option<JointHandle>, ModelError> {
        Ok(self.find(name, ObjectKind::Joint)?.map(JointHandle))
    }

    /// Looks up a site by its model-local name.
    pub fn site(&self, name: &str) -> Result<Option<SiteHandle>, ModelError> {
        Ok(self.find(name, ObjectKind::Site)?.map(SiteHandle))
    }

    /// Looks up an actuator by its model-local name.
    pub fn actuator(&self, name: &str) -> Result<Option<ActuatorHandle>, ModelError> {
        Ok(self.find(name, ObjectKind::Actuator)?.map(ActuatorHandle))
    }

    /// Looks up a sensor by its model-local name.
    pub fn sensor(&self, name: &str) -> Result<Option<SensorHandle>, ModelError> {
        Ok(self.find(name, ObjectKind::Sensor)?.map(SensorHandle))
    }

    /// Binds a generated sample port to one model-authored native sensor.
    ///
    /// The public descriptor remains the contract identity while the returned
    /// handle and static range identify the model-local native source. The
    /// caller chooses the explicit native name from the component model; no
    /// name or sensor semantics are inferred from the port string.
    pub fn bind_sensor<P: PortDescriptor>(
        &self,
        port: P,
        native_name: &str,
    ) -> Result<SensorBinding, ModelError> {
        let signature = binding_signature(port, "sensor", PortKind::Sample)?;
        let native = self
            .sensor(native_name)?
            .ok_or_else(|| missing_binding(signature, "sensor", native_name))?;
        Ok(SensorBinding {
            port: signature,
            native,
            info: self.sensor_info(native)?,
        })
    }

    /// Binds a generated sample port to one model-authored native camera.
    pub fn bind_camera<P: PortDescriptor>(
        &self,
        port: P,
        native_name: &str,
    ) -> Result<CameraBinding, ModelError> {
        let signature = binding_signature(port, "camera", PortKind::Sample)?;
        let native = self
            .camera(native_name)?
            .ok_or_else(|| missing_binding(signature, "camera", native_name))?;
        Ok(CameraBinding {
            port: signature,
            native,
            info: self.camera_info(native)?,
        })
    }

    /// Binds a generated sample port to one model-authored native site.
    ///
    /// Sites are used for capabilities whose native output is derived from a
    /// physical frame, such as GNSS antenna position or a finite-FOV range
    /// query, rather than a direct MuJoCo sensor table.
    pub fn bind_site<P: PortDescriptor>(
        &self,
        port: P,
        native_name: &str,
    ) -> Result<SiteBinding, ModelError> {
        let signature = binding_signature(port, "site", PortKind::Sample)?;
        let native = self
            .site(native_name)?
            .ok_or_else(|| missing_binding(signature, "site", native_name))?;
        Ok(SiteBinding {
            port: signature,
            native,
            info: self.site_info(native)?,
        })
    }

    /// Binds a consuming setpoint port to one model-authored native actuator.
    ///
    /// A consuming input does not advertise a public port from the component
    /// runtime. The compiled graph supplies its generated producer descriptor
    /// and this explicit binding only associates that descriptor with native
    /// actuation.
    pub fn bind_actuator<P: PortDescriptor>(
        &self,
        port: P,
        native_name: &str,
    ) -> Result<ActuatorBinding, ModelError> {
        let signature = binding_signature(port, "actuator", PortKind::Setpoint)?;
        let native = self
            .actuator(native_name)?
            .ok_or_else(|| missing_binding(signature, "actuator", native_name))?;
        Ok(ActuatorBinding {
            port: signature,
            native,
            info: self.actuator_info(native)?,
        })
    }

    /// Looks up a model-authored camera by its model-local name.
    pub fn camera(&self, name: &str) -> Result<Option<CameraHandle>, ModelError> {
        Ok(self.find(name, ObjectKind::Camera)?.map(CameraHandle))
    }

    /// Returns static model data for a body.
    pub fn body_info(&self, handle: BodyHandle) -> Result<BodyInfo, ModelError> {
        let index = self.check_handle(handle.0, ObjectKind::Body, self.inner.nbody() as usize)?;
        Ok(BodyInfo {
            handle,
            position: self.inner.body_pos()[index],
            orientation: self.inner.body_quat()[index],
            mass: self.inner.body_mass()[index],
        })
    }

    /// Returns static model data for a joint.
    pub fn joint_info(&self, handle: JointHandle) -> Result<JointInfo, ModelError> {
        let index = self.check_handle(handle.0, ObjectKind::Joint, self.inner.njnt() as usize)?;
        let limits = self.inner.jnt_limited()[index].then(|| self.inner.jnt_range()[index]);
        Ok(JointInfo {
            handle,
            kind: JointKind::from_native(self.inner.jnt_type()[index]),
            qpos_offset: self.inner.jnt_qposadr()[index] as usize,
            dof_offset: self.inner.jnt_dofadr()[index] as usize,
            axis: self.inner.jnt_axis()[index],
            limits,
        })
    }

    /// Returns static model data for a site.
    pub fn site_info(&self, handle: SiteHandle) -> Result<SiteInfo, ModelError> {
        let index = self.check_handle(handle.0, ObjectKind::Site, self.inner.nsite() as usize)?;
        Ok(SiteInfo {
            handle,
            body_index: self.inner.site_bodyid()[index] as usize,
            position: self.inner.site_pos()[index],
            orientation: self.inner.site_quat()[index],
        })
    }

    /// Returns static model data for an actuator.
    pub fn actuator_info(&self, handle: ActuatorHandle) -> Result<ActuatorInfo, ModelError> {
        let index = self.check_handle(
            handle.0,
            ObjectKind::Actuator,
            self.inner.nactuator() as usize,
        )?;
        let control_index = self.inner.actuator_ctrladr()[index] as usize;
        let control_range = if control_index < self.inner.actuator_ctrllimited().len()
            && self.inner.actuator_ctrllimited()[control_index]
        {
            Some(self.inner.actuator_ctrlrange()[control_index])
        } else {
            None
        };
        Ok(ActuatorInfo {
            handle,
            control_index,
            control_range,
        })
    }

    /// Returns static model data for a sensor's contiguous output range.
    pub fn sensor_info(&self, handle: SensorHandle) -> Result<SensorInfo, ModelError> {
        let index =
            self.check_handle(handle.0, ObjectKind::Sensor, self.inner.nsensor() as usize)?;
        let offset = self.inner.sensor_adr()[index] as usize;
        let dimension = self.inner.sensor_dim()[index] as usize;
        Ok(SensorInfo {
            handle,
            data_offset: offset,
            dimension,
        })
    }

    /// Returns static model data for a camera.
    pub fn camera_info(&self, handle: CameraHandle) -> Result<CameraInfo, ModelError> {
        let index = self.check_handle(handle.0, ObjectKind::Camera, self.inner.ncam() as usize)?;
        let [width, height] = self.inner.cam_resolution()[index];
        Ok(CameraInfo {
            handle,
            body_index: self.inner.cam_bodyid()[index] as usize,
            position: self.inner.cam_pos()[index],
            orientation: self.inner.cam_quat()[index],
            resolution: [width as usize, height as usize],
            fovy_degrees: self.inner.cam_fovy()[index],
        })
    }

    /// Returns the model control range for a scalar control, if it is limited.
    pub fn control_range(&self, index: usize) -> Result<Option<[f64; 2]>, ModelError> {
        let controls = self.inner.nu() as usize;
        if index >= controls {
            return Err(ModelError::InvalidHandleIndex {
                kind: "control",
                index,
                length: controls,
            });
        }
        Ok(
            self.inner.actuator_ctrllimited()[index]
                .then(|| self.inner.actuator_ctrlrange()[index]),
        )
    }

    pub(crate) fn inner_arc(&self) -> Arc<MjModel> {
        Arc::clone(&self.inner)
    }

    fn find(&self, name: &str, kind: ObjectKind) -> Result<Option<ModelHandle>, ModelError> {
        if CString::new(name).is_err() {
            return Err(ModelError::NameContainsNul);
        }
        Ok(self
            .inner
            .name_to_id(kind.native(), name)
            .map(|index| ModelHandle {
                identity: self.identity,
                kind,
                index,
            }))
    }

    fn check_handle(
        &self,
        handle: ModelHandle,
        expected_kind: ObjectKind,
        length: usize,
    ) -> Result<usize, ModelError> {
        if handle.identity != self.identity {
            return Err(ModelError::ForeignHandle {
                found: handle.identity,
                expected: self.identity,
            });
        }
        if handle.kind != expected_kind {
            return Err(ModelError::InvalidHandleIndex {
                kind: expected_kind.as_str(),
                index: handle.index,
                length,
            });
        }
        if handle.index >= length {
            return Err(ModelError::InvalidHandleIndex {
                kind: expected_kind.as_str(),
                index: handle.index,
                length,
            });
        }
        Ok(handle.index)
    }
}

fn native_error(operation: &'static str, error: impl fmt::Display) -> ModelError {
    ModelError::Native {
        operation,
        message: error.to_string(),
    }
}

fn binding_signature<P: PortDescriptor>(
    port: P,
    native_kind: &'static str,
    expected: PortKind,
) -> Result<PortSignature, ModelError> {
    if P::KIND != expected {
        return Err(ModelError::InvalidBindingKind {
            port: port.name(),
            actual: P::KIND,
            native_kind,
            expected,
        });
    }
    Ok(port.signature())
}

fn missing_binding(
    signature: PortSignature,
    native_kind: &'static str,
    native_name: &str,
) -> ModelError {
    ModelError::MissingBinding {
        port: signature.name,
        native_kind,
        native_name: native_name.to_owned(),
    }
}

/// Deterministic compiled model table sizes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelCounts {
    /// Number of generalized positions.
    pub qpos: usize,
    /// Number of generalized velocities.
    pub qvel: usize,
    /// Number of scalar controls.
    pub controls: usize,
    /// Number of actuators.
    pub actuators: usize,
    /// Number of bodies, including the world body.
    pub bodies: usize,
    /// Number of joints.
    pub joints: usize,
    /// Number of sites.
    pub sites: usize,
    /// Number of model-authored cameras.
    pub cameras: usize,
    /// Number of sensors.
    pub sensors: usize,
    /// Number of scalar sensor outputs.
    pub sensor_values: usize,
}

/// Stable identity for one closed source/resource model.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ModelIdentity(pub(crate) [u8; 32]);

impl ModelIdentity {
    /// Returns the identity bytes.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Returns the identity as lowercase hexadecimal.
    #[must_use]
    pub fn to_hex(self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

impl fmt::Display for ModelIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

/// Kind of one model-local native object.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ObjectKind {
    /// A body.
    Body,
    /// A joint.
    Joint,
    /// A site.
    Site,
    /// A camera.
    Camera,
    /// An actuator.
    Actuator,
    /// A sensor.
    Sensor,
}

impl ObjectKind {
    fn native(self) -> MjtObj {
        match self {
            Self::Body => MjtObj::mjOBJ_BODY,
            Self::Joint => MjtObj::mjOBJ_JOINT,
            Self::Site => MjtObj::mjOBJ_SITE,
            Self::Camera => MjtObj::mjOBJ_CAMERA,
            Self::Actuator => MjtObj::mjOBJ_ACTUATOR,
            Self::Sensor => MjtObj::mjOBJ_SENSOR,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::Joint => "joint",
            Self::Site => "site",
            Self::Camera => "camera",
            Self::Actuator => "actuator",
            Self::Sensor => "sensor",
        }
    }
}

/// A model-scoped native object handle.
///
/// The native index is only meaningful with the exact [`ModelIdentity`] carried
/// by this handle.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ModelHandle {
    identity: ModelIdentity,
    kind: ObjectKind,
    index: usize,
}

impl ModelHandle {
    /// Returns the model identity that owns this handle.
    #[must_use]
    pub const fn model_identity(self) -> ModelIdentity {
        self.identity
    }

    /// Returns the local native object kind.
    #[must_use]
    pub const fn kind(self) -> ObjectKind {
        self.kind
    }

    /// Returns the local native index.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }
}

macro_rules! typed_handle {
    ($name:ident, $kind:ident) => {
        #[doc = concat!("A model-scoped ", stringify!($kind), " handle.")]
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub struct $name(ModelHandle);

        impl $name {
            /// Returns the model identity that owns this handle.
            #[must_use]
            pub const fn model_identity(self) -> ModelIdentity {
                self.0.identity
            }

            /// Returns the model-local native index.
            #[must_use]
            pub const fn index(self) -> usize {
                self.0.index
            }

            /// Returns the generic model handle.
            #[must_use]
            pub const fn as_model_handle(self) -> ModelHandle {
                self.0
            }
        }
    };
}

typed_handle!(BodyHandle, body);
typed_handle!(JointHandle, joint);
typed_handle!(SiteHandle, site);
typed_handle!(CameraHandle, camera);
typed_handle!(ActuatorHandle, actuator);
typed_handle!(SensorHandle, sensor);

/// Supported native joint kinds represented without exposing native pointers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JointKind {
    /// A free six-degree-of-freedom joint.
    Free,
    /// A three-degree-of-freedom ball joint.
    Ball,
    /// A one-degree-of-freedom slider joint.
    Slide,
    /// A one-degree-of-freedom hinge joint.
    Hinge,
}

impl JointKind {
    fn from_native(kind: MjtJoint) -> Self {
        match kind {
            MjtJoint::mjJNT_FREE => Self::Free,
            MjtJoint::mjJNT_BALL => Self::Ball,
            MjtJoint::mjJNT_SLIDE => Self::Slide,
            MjtJoint::mjJNT_HINGE => Self::Hinge,
        }
    }
}

/// Read-only static body facts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyInfo {
    /// Handle for the body.
    pub handle: BodyHandle,
    /// Position relative to the parent body.
    pub position: [f64; 3],
    /// Quaternion relative to the parent body in MuJoCo order `[w, x, y, z]`.
    pub orientation: [f64; 4],
    /// Body mass in native mass units.
    pub mass: f64,
}

/// Read-only static joint facts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointInfo {
    /// Handle for the joint.
    pub handle: JointHandle,
    /// Native joint kind.
    pub kind: JointKind,
    /// Offset into `qpos`.
    pub qpos_offset: usize,
    /// Offset into `qvel`.
    pub dof_offset: usize,
    /// Joint axis in the parent body frame.
    pub axis: [f64; 3],
    /// Optional native joint position range.
    pub limits: Option<[f64; 2]>,
}

/// Read-only static site facts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SiteInfo {
    /// Handle for the site.
    pub handle: SiteHandle,
    /// Owning body index in this model.
    pub body_index: usize,
    /// Position relative to the owning body.
    pub position: [f64; 3],
    /// Quaternion relative to the owning body in MuJoCo order `[w, x, y, z]`.
    pub orientation: [f64; 4],
}

/// Read-only static actuator facts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActuatorInfo {
    /// Handle for the actuator.
    pub handle: ActuatorHandle,
    /// Index into the scalar `ctrl` array.
    pub control_index: usize,
    /// Optional finite control range.
    pub control_range: Option<[f64; 2]>,
}

/// Read-only static sensor facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SensorInfo {
    /// Handle for the sensor.
    pub handle: SensorHandle,
    /// Start index in the scalar sensor-data array.
    pub data_offset: usize,
    /// Number of scalar values emitted by this sensor.
    pub dimension: usize,
}

/// A generated sample port bound to a native sensor table range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SensorBinding {
    /// Public generated port identity.
    pub port: PortSignature,
    /// Model-local sensor handle.
    pub native: SensorHandle,
    /// Native data range emitted by this sensor.
    pub info: SensorInfo,
}

impl SensorBinding {
    /// Reads this binding's native values from a snapshot owned by the same
    /// compiled model.
    pub fn values<'a>(&self, snapshot: &'a StateSnapshot) -> Result<&'a [f64], ModelError> {
        ensure_snapshot_model(snapshot, self.native.model_identity())?;
        let end = self
            .info
            .data_offset
            .checked_add(self.info.dimension)
            .ok_or(ModelError::InvalidHandleIndex {
                kind: "sensor data",
                index: self.info.data_offset,
                length: snapshot.sensor_data().len(),
            })?;
        snapshot
            .sensor_data()
            .get(self.info.data_offset..end)
            .ok_or(ModelError::InvalidHandleIndex {
                kind: "sensor data",
                index: self.info.data_offset,
                length: snapshot.sensor_data().len(),
            })
    }
}

/// A generated sample port bound to a native camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraBinding {
    /// Public generated port identity.
    pub port: PortSignature,
    /// Model-local camera handle.
    pub native: CameraHandle,
    /// Static camera facts.
    pub info: CameraInfo,
}

/// A generated sample port bound to a native model site.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SiteBinding {
    /// Public generated port identity.
    pub port: PortSignature,
    /// Model-local site handle.
    pub native: SiteHandle,
    /// Static site facts.
    pub info: SiteInfo,
}

impl SiteBinding {
    /// Reads this binding's post-forward Cartesian position from a snapshot
    /// owned by the same compiled model.
    pub fn position(&self, snapshot: &StateSnapshot) -> Result<[f64; 3], ModelError> {
        ensure_snapshot_model(snapshot, self.native.model_identity())?;
        snapshot
            .site_positions()
            .get(self.native.index())
            .copied()
            .ok_or(ModelError::InvalidHandleIndex {
                kind: "site position",
                index: self.native.index(),
                length: snapshot.site_positions().len(),
            })
    }
}

/// A generated consuming setpoint bound to a native actuator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActuatorBinding {
    /// Generated producer/setpoint identity supplied by the compiled graph.
    pub port: PortSignature,
    /// Model-local actuator handle.
    pub native: ActuatorHandle,
    /// Static actuator facts.
    pub info: ActuatorInfo,
}

impl ActuatorBinding {
    /// Reads the selected scalar control from a snapshot owned by the same
    /// compiled model.
    pub fn control(&self, snapshot: &StateSnapshot) -> Result<f64, ModelError> {
        ensure_snapshot_model(snapshot, self.native.model_identity())?;
        snapshot
            .controls()
            .get(self.info.control_index)
            .copied()
            .ok_or(ModelError::InvalidHandleIndex {
                kind: "control",
                index: self.info.control_index,
                length: snapshot.controls().len(),
            })
    }
}

/// Read-only static camera facts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraInfo {
    /// Handle for the camera.
    pub handle: CameraHandle,
    /// Owning body index in this model.
    pub body_index: usize,
    /// Position relative to the owning body.
    pub position: [f64; 3],
    /// Quaternion relative to the owning body in MuJoCo order `[w, x, y, z]`.
    pub orientation: [f64; 4],
    /// Render resolution as `[width, height]` pixels.
    pub resolution: [usize; 2],
    /// Vertical field of view in degrees for a perspective camera.
    pub fovy_degrees: f64,
}

fn ensure_snapshot_model(
    snapshot: &StateSnapshot,
    expected: ModelIdentity,
) -> Result<(), ModelError> {
    if snapshot.model_identity() != expected {
        return Err(ModelError::ForeignHandle {
            found: snapshot.model_identity(),
            expected,
        });
    }
    Ok(())
}
