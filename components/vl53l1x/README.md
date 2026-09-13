# VL53L1X component

The component mount uses +X forward, +Y left, and +Z up.
The native range site explicitly directs its +Z measurement axis along mount +X.
Robot-owned mount rotations select the sensor's horizontal or downward direction.
The simulator samples the authored finite field of view and range limits; it does not substitute an unrelated body frame.

The native component acceptance uses the actual MJCF artifact and a target at a known distance along mount +X.
Required ground and obstacle interpretation belongs to the configured Safety service, not the component or native provider.
