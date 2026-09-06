//! Bounded private coordination shared by the Webots host and native controllers.
//!
//! This is not a public simulation API and it never leaves the local host.
//! Each controller publishes observations through a bounded nonblocking queue.
//! A socket worker performs the potentially blocking local I/O so Webots never waits for the host
//! or a robot participant while it owns a native transition.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use phoxal::api;
use phoxal::bus::RobotInstant;
use phoxal::identity::{ExecutionId, ProducerId};
use phoxal::model::identity::CapabilityRef;
use phoxal::model::world::WorldProgress;
use phoxal::version::FrameworkVersion;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::plan::RobotSimulationPlan;

mod framing;
mod link;
mod records;

pub use framing::{read_frame, write_frame};
pub use link::ControllerLink;
pub use records::{
    ActuationDecision, ActuationEvidence, ActuationSelection, AppliedActuation, ControllerEvent,
    ControllerFault, ControllerRole, HostDirective, HostRequest, HostResponse, LinkError,
    NativeMotion, NativeMutation, NativeProgressObservation, NoActuationReason, ObservedNativeMode,
    OfferedActuation, validate_robot_import,
};
use records::{EVENT_QUEUE_CAPACITY, IO_TIMEOUT, MAX_ROBOT_SOURCE_BYTES};

#[cfg(test)]
mod protocol_boundary_tests;
