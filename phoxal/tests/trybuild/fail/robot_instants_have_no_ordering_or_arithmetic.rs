// The four time types are non-interchangeable and carry no cross-type ordering
// or arithmetic (#952 section C). Comparison and age across timelines are
// *checked* operations, so the compiler must reject every shortcut that would
// silently produce a wrong number.

use phoxal::bus::{LocalInstant, RobotInstant, TimelineId, WallTimestamp};

fn main() {
    let timeline = TimelineId::mint();
    let earlier = RobotInstant::new(timeline, 100);
    let later = RobotInstant::new(timeline, 200);
    let local = LocalInstant::now();
    let wall = WallTimestamp::now();

    // No `Ord`: ordering robot instants must go through the checked comparison
    // that can fail across timelines.
    let _ = earlier < later;

    // No `Sub`: age must go through the checked `duration_since`.
    let _ = later - earlier;

    // No ordering across types: a host instant and a robot instant are
    // different physical facts.
    let _ = local < earlier;

    // A calendar timestamp implements no ordering and no freshness interface at
    // all; it exists for diagnostics only.
    let _ = wall < WallTimestamp::now();
}
