// The positive counterpart to fail/external_on_publish_field.rs and
// fail/external_on_server_field.rs: #[phoxal(external)] is valid on a
// subscribe (and, symmetrically, an ask) field - the consumer-side edge the
// coherence gate marker exists to excuse (coherence-gate design §1). This is
// the coherence-gate design doc's own worked example: a teleop-only robot
// where the operator app, off-robot, is the only publisher of drive targets,
// so the `drive` owner's `target` subscription is marked external.
use phoxal::api as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {}

#[derive(phoxal::Api)]
struct Api {
    // On-robot counterparts: checked normally.
    state: Publisher<api::drive::State>,

    // The publisher of drive::Target is the operator app (off-robot); in a
    // teleop-only robot.yaml no pinned participant publishes it. `external`
    // excuses this one edge from the coherence check.
    #[phoxal(external)]
    target: Latest<api::drive::Target>,
}

#[phoxal::service(id = "external-on-subscribe")]
struct ExternalOnSubscribe;

#[phoxal::behavior]
impl ExternalOnSubscribe {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        // Owner opt-in (plan #00 L2): the runner-minted capability the owner
        // (`internal`) builder requires.
        let cap = ctx.owner_capability();
        Ok((
            Self,
            Self::Api {
                state: ctx
                    .publisher(api::topic::internal::new(cap).drive().state())
                    .await?,
                target: ctx
                    .latest(api::topic::internal::new(cap).drive().target())
                    .await?,
            },
        ))
    }
}

fn main() {}
