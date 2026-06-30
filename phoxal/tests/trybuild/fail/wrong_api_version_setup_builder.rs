// A wrong-API body cannot be smuggled in through SetupContext builders. The
// load-bearing guard is the type bound `B: ContractBody<Api = R::Api>` on
// `ctx.publisher` (see `phoxal/src/runtime/context.rs`): even if an author forges
// a `Topic` for a foreign body and satisfies the publish-declaration marker, a
// body whose `Api` is not the runtime's selected API version fails to compile.
//
// To isolate THAT bound, we hand-declare `DeclaresPublish<ForeignBody>` below so
// the other `ctx.publisher` bound (`R: DeclaresPublish<B>`) is already satisfied;
// otherwise the failure would surface on the `DeclaresPublish` inference path
// instead of the wrong-API guard we are testing. The raw `Topic` constructors are
// `#[doc(hidden)]` (off the author surface) only because the `phoxal_api_tree!`
// macro must call them across the `phoxal` / `phoxal-bus` crate boundary; the
// guarantee proven here is the type bound, not constructor privacy.
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

// Satisfy the publish-declaration gate for `ForeignBody` by hand, so the only
// remaining unsatisfied bound on `ctx.publisher` is the wrong-API guard
// `B: ContractBody<Api = R::Api>`. This makes the compile error isolate that
// bound rather than the `R: DeclaresPublish<B>` inference path.
impl phoxal::runtime::DeclaresPublish<ForeignBody> for WrongApiSetup {}

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
