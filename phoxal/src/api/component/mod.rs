//! The robot family's component-scoped capabilities.
//!
//! Every child below is a capability kind, addressed under the component
//! instance that mounts it and the capability id within that component - so a
//! key reads `robot/component/{instance}/<kind>/{capability}/<leaf>`. The two
//! dynamic segments are bound in order by walking this node.

use crate::model::identity::CapabilityId;

crate::nodes! {
    accelerometer(capability: CapabilityId);
    battery(capability: CapabilityId);
    camera(capability: CapabilityId);
    depth(capability: CapabilityId);
    emergency_stop(capability: CapabilityId);
    encoder(capability: CapabilityId);
    gnss(capability: CapabilityId);
    gyroscope(capability: CapabilityId);
    imu(capability: CapabilityId);
    led(capability: CapabilityId);
    lidar(capability: CapabilityId);
    magnetometer(capability: CapabilityId);
    microphone(capability: CapabilityId);
    mmwave(capability: CapabilityId);
    motor(capability: CapabilityId);
    range(capability: CapabilityId);
    speaker(capability: CapabilityId);
}
