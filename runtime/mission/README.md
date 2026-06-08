# phoxal-runtime-mission

`phoxal-runtime-mission` is the MVP implementation of the only behavior
decider.

It owns explicit mission command lifecycle, the active autonomous goal, pause /
resume / cancel / manual-handover state, exploration-session ownership, and
mission state publication. `NavigateTo` and `Explore` are accepted only while
localization is `Tracking`; other localization modes refuse new autonomous
mission commands.

Primary contract: `runtime/mission/command`, `runtime/mission/state`,
`runtime/mission/goal`, and `runtime/mission/debug/decision_trace`.
`GoalTolerance` is geometric only: position radius plus optional normalized
heading error. `NavigateTo.max_duration_ns` is a logical-time execution budget;
expiry is published as mission failure, not treated as goal completion.

Exploration contract: `phoxal-runtime-explore` proposes scored candidates on
`runtime/explore/goal_candidates`; mission remains the only goal decider. During
an active exploration session, mission selects the highest-scored candidate,
publishes it as `GoalSource::Explore`, and returns to `Exploring` after the
goal is reached so the next candidate batch can be promoted.

MVP limitations: `Explore` is open-ended and ignores area/completion details,
there is no perception candidate selection, and there is no `DeadReckoning`
continuation budget. While navigating, mission republishes the active goal every
runtime step so `plan` can keep deriving fresh paths from the latest pose.

Not in scope: low-level motion execution, direct actuator authority, planner
search, feedback from plan/follow/safety, or bypassing safety/motion authority.
