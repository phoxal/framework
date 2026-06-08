# phoxal-runtime-drive

`phoxal-runtime-drive` is currently a BLUEPRINT skeleton.

It loads the staged source-shaped bundle through `phoxal::runtime`, calls the
shared deterministic resolver in `phoxal::model::robot`, and then stops with an
explicit `unimplemented!("BLUEPRINT skeleton")`.

Target scope: final actuator authority, command watchdog, dynamic limiting,
kinematic inversion, actuator command generation, motor command publication, and
final stop enforcement.

Inputs: `runtime/drive/target`, `runtime/safety/authorization`, and
`runtime/localize/state`. Outputs: `runtime/drive/state` and component motor
commands.

Not in scope: high-level safety perception, mapping, planning, mission behavior,
or motion-source arbitration.
