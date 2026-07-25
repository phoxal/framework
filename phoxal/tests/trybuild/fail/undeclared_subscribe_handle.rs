// D44: `SetupContext` builders reject building a handle for a contract the
// `Api` struct never declared as a field. This `Api` declares only a
// `CommandPublisher<drive::Target>` (no `Subscriber<drive::State>`/`Latest<drive::State>`
// field), so `Self::Api: DeclaresSubscribe<drive::State>` does not hold and
// `ctx.latest(...)` for that contract must fail to compile - exactly the gap
// `Declares*<B>` gating closes (`ParticipantApi::CONTRACTS` is now a
// guaranteed-complete picture of the declared surface, not just a lower
// bound).
//
// The SOLE error is the unsatisfied `DeclaresSubscribe<drive::State>` bound.
use phoxal::api as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    target: CommandPublisher<api::drive::Target>,
}

#[phoxal::service(id = "undeclared-subscribe")]
struct UndeclaredSubscribe;

#[phoxal::behavior]
impl UndeclaredSubscribe {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // ERROR: `Api` never declared a subscribe handle for `drive::State`.
        let _state = ctx.latest(api::topic::new().drive().state()).await?;
        Ok((
            Self,
            Self::Api {
                target: ctx.command_publisher(api::topic::new().drive().target()).await?,
            },
        ))
    }
}

fn main() {}
