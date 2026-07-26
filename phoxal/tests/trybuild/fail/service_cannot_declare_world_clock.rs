// organization#957 leftover, closed: "only a simulator can mint world steps"
// used to be a doc comment nothing enforced - any participant could declare a
// `StatePublisher<api::simulation::Clock>` field and build it through the
// ordinary `ctx.state_publisher(...)` builder every participant kind has.
// `simulation::Clock` is now declared `world_clock` in the api tree, which
// implements the disjoint `WorldClockContract` instead of `StateContract`, so
// an ordinary `#[phoxal::service]` cannot even name this field: `StateContract`
// is not implemented for it.
//
// Two further escape routes closed in the same round (an independent review
// found the first pass here left them open):
//
// - Naming the *correct* `WorldClockPublisher<Clock>` type does not help. It
//   is reachable only through the explicit `phoxal::raw` opt-in - never
//   through `phoxal::prelude` or `phoxal::bus` - and even a service that
//   deliberately imports it from there still has no documented constructor:
//   `SetupContextSimulatorExt::world_clock_publisher` is gated to
//   `Self: IsSimulator`.
// - `IsSimulator` itself is now a sealed marker
//   (`phoxal::participant::spec::sealing::Sealed`), so a participant cannot
//   unlock that gate by writing `impl IsSimulator for MyType` directly - that
//   leaves the sealing bound unsatisfied. Satisfying it means also naming the
//   hidden sealing path by hand, which is a deliberate act rather than
//   something a participant does by accident.
//
// None of this makes the capability a sealed boundary in the absolute sense -
// see `TimelineAuthority`'s and `IsSimulator`'s doc comments for the exact,
// honest strength of the guarantee - but all three are now closed for a
// participant that does not deliberately reach past the documented surface.

use phoxal::api;
use phoxal::prelude::*;

#[derive(phoxal::Api)]
struct Api {
    clock: StatePublisher<api::simulation::Clock>,
}

#[derive(Clone, Debug, serde::Deserialize, phoxal::Config)]
struct Config {}

#[phoxal::service(id = "rogue-clock-service", config = Config, api = Api)]
struct RogueClockService;

#[phoxal::behavior]
impl RogueClockService {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // ERROR: `api::simulation::Clock` does not implement `StateContract`
        // (it implements `WorldClockContract`, which only
        // `SetupContextSimulatorExt::world_clock_publisher` accepts, and that
        // trait is unavailable here - `RogueClockService` is a
        // `#[phoxal::service]`, not a `#[phoxal::simulator]`).
        let clock = ctx
            .state_publisher(api::topic::owner().simulation().clock())
            .await?;
        Ok((Self, Api { clock }))
    }
}

// Naming the real `WorldClockPublisher` type (only reachable through the
// `phoxal::raw` opt-in) does not help either: a service still has no route to
// build one.
use phoxal::raw::WorldClockPublisher;

#[derive(phoxal::Api)]
struct RawClockApi {
    clock: WorldClockPublisher<api::simulation::Clock>,
}

#[phoxal::service(id = "rogue-raw-clock-service", config = Config, api = RawClockApi)]
struct RogueRawClockService;

#[phoxal::behavior]
impl RogueRawClockService {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // ERROR: no method named `world_clock_publisher` on `SetupContext<RogueRawClockService>` -
        // `SetupContextSimulatorExt` is only implemented for `R: IsSimulator`,
        // and `RogueRawClockService` is a `#[phoxal::service]`.
        let clock = ctx
            .world_clock_publisher(api::topic::owner().simulation().clock())
            .await?;
        Ok((Self, RawClockApi { clock }))
    }
}

// And unlocking `IsSimulator` by hand does not work either: it is a sealed
// marker trait (organization#957).
struct RogueSimulatorMarker;

// ERROR: the trait bound `RogueSimulatorMarker: phoxal::participant::spec::sealing::Sealed`
// is not satisfied.
impl phoxal::participant::IsSimulator for RogueSimulatorMarker {}

fn main() {}
