# phoxal-installation

`phoxal-installation` owns artifact-only installation and activation of completed Phoxal deployment releases.

It does not read `robot.yaml`, invoke Cargo, resolve packages, or decide which services run.
The source compiler supplies a complete release containing `phoxal-supervisor` and `bundle/`.
This crate creates or verifies its deterministic archive, installs it under its content digest, and atomically switches the active release symlink.

Activation and rollback select an explicit immutable release identifier.
The crate also validates deployment identity and atomically generates the installed Linux systemd unit outside immutable releases.
That unit is the single persisted source of `scope` and `supervisor_id`: first installation supplies both, regeneration can retain both, and a deliberate identity change replaces both together.
Process control remains outside this library so an installer can stop services before activation, regenerate the unit, start it, test process and domain readiness separately, and switch back after a failed activation.
