# phoxal-installation

`phoxal-installation` owns artifact-only installation and activation of completed Phoxal deployment releases.

It does not read `robot.yaml`, invoke Cargo, resolve packages, or decide which services run.
The source compiler supplies a complete release containing `phoxal-supervisor` and `bundle/`.
This crate creates or verifies its deterministic archive, installs it under its content digest, and atomically switches the active release symlink.

Activation and rollback select an explicit immutable release identifier.
The crate also validates deployment identity and atomically generates the installed Linux systemd unit outside immutable releases.
That unit is the single persisted source of `scope` and `supervisor_id`: first installation supplies both, regeneration can retain both, and a deliberate identity change replaces both together.
Host process control is represented by `ServiceControl`, and `SystemdService` invokes native systemd primitives so an installer can stop services before activation, regenerate the unit, start it, test process and domain readiness separately, and switch back after a failed activation.

## Supported host procedure

The supported procedure is implemented by `DeploymentOperations` with a `SystemdService` controller and consumes only a completed `.build.phoxal` archive, its SHA-256 sidecar, an installation root, and the host's native `systemctl` and `journalctl` primitives.
The robot host does not need `robot.yaml`, source files, a Cargo workspace, package resolution, or a source compiler.

1. Open an installation root with `Installation::open` and bind it to `SystemdService::new(SERVICE_UNIT_FILE)`.
2. Call `DeploymentOperations::install(archive, checksum, IdentityUpdate::Set(identity))` for the first install, or use `IdentityUpdate::Retain` when regenerating an existing unit.
3. The install operation verifies the checksum, extracts only the fixed supervisor and bundle layout under the content digest, persists the identity outside immutable releases, links the generated absolute unit path with `systemctl link`, and runs `systemctl daemon-reload` without activating or starting the release.
4. Call `DeploymentOperations::activate_and_start(release_id)` to stop the current service, select the exact immutable release, start the service, and verify native process readiness.
5. Call `DeploymentOperations::status()` for the active release and process state, and call `DeploymentOperations::logs(LogQuery::default())` for bounded native journal output.
6. Treat native process readiness as a separate gate from domain readiness: query the public supervisor session until its status is `Ready`, and treat `Failed` as a failed activation even when systemd still reports the supervisor process as active.
7. A failed process activation returns `OperationsError::ActivationFailed` with the attempted and previous release identifiers; inspect status and logs, then call `DeploymentOperations::rollback_and_start(previous_release)` for deliberate recovery when a previous release exists, or `DeploymentOperations::deactivate()` when this was the first activation.
8. After any failed runtime execution, stop the service, start a fresh execution through the public session, and require explicit motion re-arm; this procedure never restores old commands, goals, setpoints, or authority automatically.

The native equivalent of the controller operations is `systemctl link <absolute-unit-path>`, `systemctl daemon-reload`, `systemctl start phoxal-supervisor.service`, `systemctl stop phoxal-supervisor.service`, `systemctl show --property=ActiveState --property=SubState --property=Result --value phoxal-supervisor.service`, and `journalctl --unit phoxal-supervisor.service --no-pager --output=cat --lines 200`.
No operational frontend release is required for this procedure.
