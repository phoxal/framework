// A wrong-API body cannot be smuggled in through SetupContext builders: there is
// no author-facing way to build a `Topic` for an arbitrary body. The only public
// path is the api-local builder (`api::topic::new()....()`); the raw `Topic`
// constructors are sealed (`pub(crate)`), so this attempt to forge one for a
// foreign body fails to even construct the topic.
use phoxal::bus::{PubSub, Topic};
use phoxal::prelude::*;

enum ForeignApi {}

impl phoxal::api::ApiVersion for ForeignApi {
    const ID: &'static str = "foreign";
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ForeignBody;

impl phoxal::api::ContractBody for ForeignBody {
    type Api = ForeignApi;
    const FAMILY: &'static str = "foreign::Body";
    const TOPIC: &'static str = "foreign/body";
}

#[derive(phoxal::Runtime)]
#[phoxal(id = "wrong-api-setup", api = y2026_1)]
struct WrongApiSetup {
    target: Publisher<phoxal::api::y2026_1::drive::Target>,
}

#[phoxal::runtime]
impl WrongApiSetup {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let topic = Topic::<PubSub<ForeignBody>>::new_static("foreign/body");
        let _foreign = ctx.publisher(topic).await?;
        Ok(Self {
            target: ctx
                .publisher(phoxal::api::y2026_1::topic::new().drive().target())
                .await?,
        })
    }
}

fn main() {}
