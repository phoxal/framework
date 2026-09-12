# phoxal-installation

`phoxal-installation` owns artifact-only installation and activation of completed Phoxal deployment releases.

It does not read `robot.yaml`, invoke Cargo, resolve packages, or decide which services run.
The source compiler supplies a complete release containing `phoxal-supervisor` and `bundle/`.
This crate creates or verifies its deterministic archive, installs it under its content digest, and atomically switches the active release symlink.

Activation and rollback select an explicit immutable release identifier.
Process control and host service configuration are deliberately outside this library so an installer can stop services before activation, regenerate units, start them, test readiness, and switch back after a failed activation without coupling the artifact store to systemd.
