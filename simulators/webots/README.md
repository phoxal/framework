# Webots adapter

This directory contains the official Webots adapter packages for the Phoxal framework train.
The adapter supports exactly Webots R2025a.
It discovers Webots through `WEBOTS_HOME` or the platform default and refuses every other observed version.

The Webots dependency and native scene behavior stay in these packages.
They do not enter the universal `phoxal` library or the backend-neutral CLI.

## Process roles

- `phoxal-simulator-webots-host` owns one long-lived world session, the generated native project, the Webots process tree, local registration, retained evidence, and serialized robot attachment.
- `phoxal-simulator-webots-world-controller` is the single Webots supervisor controller for the shared native world.
- `phoxal-simulator-webots-robot-controller` is the one controller for all simulated devices of one attached robot execution.

The host accepts one canonical compiled `WorldBundle` through `--world-bundle <PATH>`.
The CLI supplies owner-only registry and evidence directories plus a bounded combined log budget through `PHOXAL_SIMULATION_REGISTRY_DIR`, `PHOXAL_SIMULATION_EVIDENCE_DIR`, and `PHOXAL_SIMULATION_LOG_BYTE_LIMIT`.

The two Webots controllers are staged beside the generated world and use fixed arguments.
The world controller accepts only `--host-connect <LOCAL_ENDPOINT>`.
The robot controller accepts only `--connect <SUPERVISOR_ENDPOINT> --host-connect <LOCAL_ENDPOINT>`.
The host-controller connection is a private loopback coordination channel for native mutation, readiness, progress, and shutdown.
It is not a public simulation protocol or a world-step barrier.

## Native lifecycle

The generated world uses synchronized controllers and a deterministic seed of `0`.
The world controller enters `PAUSE` before the first native step and reports readiness to the host.
Running requests native Webots `REAL_TIME` mode.
Observed `RUN` or `FAST` modes are unsupported and fail the whole world session.
R2025a starts dynamically imported controllers only from its running event loop.
During attachment bootstrap, the world controller temporarily enables `REAL_TIME` using only zero-duration control exchanges, waits for native controller readiness, and restores `PAUSE` before completing the import transaction.
No positive native step is issued in that phase; the world controller verifies that physics time is unchanged throughout and fails the world if it advances.

Each attached robot keeps its independent supervisor, execution bus, monotonic execution time, and timeline.
The per-robot controller snapshots admissible commands at a completed native boundary, advances with the shared world, publishes typed simulator outputs, and publishes `StepEvent` last with the same monotonic capture instant.
Neither service scheduling nor native world progress waits for a participant acknowledgement.

## Geometry and collision

The host consumes only the compiled bundle and never reopens authoring paths.
Static primitives and self-contained GLB assets are rendered into an R2025a world.
Robot structure and capability facts are compiled into one authoritative native plan before any scene mutation.
Existing URDF robot meshes also accept triangle OBJ files with finite coordinates and normals, plus bundled MTL files containing diffuse `Kd` colors.
OBJ material references must stay beneath the mesh directory; missing materials, textures, other material inputs, non-triangle geometry, and unsupported statements fail before mutation.
Both formats become native indexed geometry, preserving their authored coordinates without native resource loading.
Generated Robot source is limited to 16 MiB and checked before the host pauses or mutates the world.

Collision GLB validation is intentionally conservative and fail-closed.
The accepted subset contains plain triangle primitives with `POSITION` and optional `NORMAL` or `TEXCOORD_0` attributes.
Morph targets, active material extensions, skins, animations, sparse accessors, non-triangle primitives, coincident collision vertices, and collision scenes above 100,000 total triangles are refused.
Only semantically inactive `KHR_materials_clearcoat` is accepted as an extension.
Scene nesting is limited to 256 nodes, and composed transforms and scaled collision vertices must remain finite.
Visuals support base color, metallic and roughness factors, emissive factors, double-sided triangles, and embedded PNG or JPEG base-color textures with supported samplers.
Unsupported material inputs fail before scene mutation instead of being silently discarded.
A detailed visual outside this subset needs an explicit supported primitive or low-complexity GLB collision override in the authored model.

## Development

Webots controller packages dynamically link the R2025a controller SDK.
They are unsupported on musl and Linux aarch64 in this release.
Run the workspace checks with the SDK available, for example:

```sh
WEBOTS_HOME=/Applications/Webots.app cargo test --workspace --all-targets
```
