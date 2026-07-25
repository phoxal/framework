# Simulation

Phoxal's framework ships one Webots artifact:

```text
bin/phoxal-simulator-webots-controller
```

The CLI stages a complete single-robot Webots project. The world declares this
binary as the robot controller before Webots opens it. There is no supervisor
artifact, runtime robot spawning, simulation control protocol, pose/contact
feed, or dynamic scene mutation.

## Controller ownership

Webots starts and stops the controller. A world reload, reset, or explicit
controller restart creates a new process. The controller bootstraps its own
framework runner, Bus, Liveliness, logs, and random nonzero process
incarnation. The CLI supplies only stable project/routing/model inputs and does
not own, observe, restart, or assign an incarnation to the controller.

The controller has no framework `#[step]`. Its only cadence is the external
Webots loop:

1. apply the newest actuator commands;
2. call the Webots step API;
3. sample devices;
4. publish component/sensor outputs at `LogicalTime(epoch, now_ns)`;
5. publish `simulation::Clock { epoch, now_ns, step }` last at that same
   logical time.

The blocking Webots step and device access run on Tokio's blocking pool rather
than a participant async worker. The metadata-only stub is paced at its declared
step duration. On loop failure or graceful shutdown, the controller applies a
final `Stop` to every bound motor before the bus closes.

The process mints one collision-resistant nonzero `epoch` on startup. Pause and
resume retain it because the process stays alive. A replacement process mints
a different opaque identity. An unexpected controller exit is reported by
Webots; recovery is a user reset/reload rather than a CLI retry.

## Multi-robot status

Multi-robot Webots authority remains deliberately deferred. The current model
is one Webots-owned controller and clock per robot bus; it does not define a
world-scoped clock/session authority spanning several robot buses. That product
decision belongs to a later multi-robot design and is not inferred here.

## Epoch and reset boundary

Epoch values are equality-only identities. Numeric ordering between different
epochs has no meaning. Epoch `0` is reserved for the framework's
not-yet-initialized sentinel and is rejected at clock ingress. Within one
nonzero epoch, time is monotonic; duplicate or backward clock samples are
ignored. Recently replaced epochs are remembered in a bounded shared history,
so an in-flight clock from a retired controller cannot reactivate old state.
Clock silence means the world is not advancing.

Clocked services subscribe to `simulation/clock`. The first valid clock selects
the execution without invoking reset. Any later different epoch:

- gates scheduled work;
- retains only inbound simulation samples for the new epoch, including samples
  that arrived before its clock;
- rejects late samples from other epochs;
- resets runner cadence, step index, and timing history;
- serially invokes the optional `#[reset]` hook before the first new-epoch
  `#[step]`.

`Subscriber` and `Latest` keep active-epoch storage independent from a bounded
quarantine for possible replacement epochs (at most four epoch identities,
each using the handle's ordinary depth; `Latest` keeps one candidate per
identity). This gives active data priority while allowing output-before-clock
publication. Activation promotes only the matching quarantine, purges the
others, and reports discarded samples in the runtime row's `epoch_filtered`
counter.

`#[reset]` receives `ResetContext { previous_epoch, new_epoch }`. Standard
services clear state derived from the prior simulated world while preserving
immutable configuration, process identity, and explicitly epoch-agnostic
host/operator inputs. A reset error faults the participant through the ordinary
process failure policy.

This boundary correlates outputs by `(epoch, now_ns)` but is not an atomic
cross-topic frame transaction. The bounded nonblocking bus may still drop or
deliver topics independently; deterministic frame commit/replay is future work.

## Runtime proof

Automated tests cover clock wire shape, payload/envelope agreement, opaque
epoch replacement in both numeric directions, reset ordering/failure, inbound
epoch filtering, controller-owned epoch generation, and output-before-clock
ordering. Live Webots proof remains a separate GUI/runtime gate and must be
reported separately when unavailable.
