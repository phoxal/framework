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
framework runner, Bus, Liveliness, logs, and its own `ProducerId`. The CLI
supplies stable project/routing/model inputs plus the `ExecutionId` of the
supervised run - which the controller must join, not mint, or its traffic would
land outside the run's bus root - and does not own, observe, or restart the
controller.

The controller has no framework `#[step]`. Its only cadence is the external
Webots loop:

1. apply the newest actuator commands;
2. call the Webots step API;
3. sample devices;
4. publish component/sensor outputs stamped at the world step it just
   completed;
5. publish `simulation::Clock { step }` last, at that same instant. The
   timeline and instant ride the envelope; the body carries only the world's
   own step counter.

The blocking Webots step and device access run on Tokio's blocking pool rather
than a participant async worker. The metadata-only stub is paced at its declared
step duration. On loop failure or graceful shutdown, the controller applies a
final `Stop` to every bound motor before the bus closes.

The process holds one `TimelineAuthority` - the narrowly scoped right to say
what time it is in a world nobody schedules - and mints one `TimelineId` on
startup. Pause and resume retain it because the process stays alive and no new
step is published. A replacement process mints a different opaque timeline. A
second authority is rejected twice over: at mint inside the process, and by the
coherence checker, which rejects a graph where two participants publish the
clock contract. An unexpected controller exit is reported by Webots; recovery is
a user reset/reload rather than a CLI retry.

## Multi-robot status

Multi-robot Webots authority remains deliberately deferred. The current model
is one Webots-owned controller and clock per robot bus; it does not define a
world-scoped clock/session authority spanning several robot buses. That product
decision belongs to a later multi-robot design and is not inferred here.

## Timeline and reset boundary

A `TimelineId` is an equality-only identity: a different one means the world
was replaced, and numeric ordering between two of them means nothing. There is
no zero - absence is `None`, never a sentinel. Within one timeline, time is
monotonic; duplicate or backward clock samples are ignored. Replaced timelines are
remembered for the life of the process, so an in-flight clock from a retired
controller cannot reactivate old state - and cannot be forgotten later either,
which a bounded history would eventually do. Clock silence means the world
is not advancing.

Clocked services subscribe to `simulation/clock`. The first valid clock selects
the world history without invoking reset. Any later different timeline:

- gates scheduled work;
- retains only inbound simulation samples for the new timeline, including
  samples that arrived before its clock;
- rejects late samples from other timelines;
- resets runner cadence, step index, and timing history;
- serially invokes the optional `#[reset]` hook before the first `#[step]` on
  the new timeline.

`Subscriber` and `Latest` keep active-timeline storage independent from a
bounded quarantine for possible replacement timelines (at most four identities,
each using the handle's ordinary depth; `Latest` keeps one candidate per
identity). This gives active data priority while allowing output-before-clock
publication. Activation promotes only the matching quarantine, purges the
others, and reports the discarded samples.

`#[reset]` receives `ResetContext { previous_timeline, new_timeline }`.
Standard services clear state derived from the prior simulated world while
preserving immutable configuration and process identity. Clockless operator
input needs no marker to survive: a command carries no production instant, so it
belongs to no timeline - what bounds it instead is its lease, which keeps ageing
on the host clock across the boundary and re-anchors its logical horizon on the
first step of the new world. A reset error faults the participant through the
ordinary process failure policy.

This boundary correlates outputs by their stamped instant but is not an atomic
cross-topic frame transaction. The bounded nonblocking bus may still drop or
deliver topics independently; deterministic frame commit/replay is future work.

## Runtime proof

Automated tests cover clock wire shape, payload/envelope agreement, opaque
timeline replacement in both numeric directions, reset ordering/failure, inbound
timeline filtering, controller-owned timeline generation, and
output-before-clock ordering. Live Webots proof remains a separate GUI/runtime gate and must be
reported separately when unavailable.
