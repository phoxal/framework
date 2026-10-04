#[phoxal::messages(package = "example.handler.v0")]
mod contract {
    pub struct Request {
        #[phoxal(tag = 1)]
        pub value: u32,
    }
    pub struct Response {
        #[phoxal(tag = 1)]
        pub value: u32,
    }
    #[phoxal::endpoints]
    pub struct Api {
        #[phoxal::operation]
        update: phoxal::contracts::RequestReply<Request, Response>,
    }
}

struct Service;

#[phoxal::runtime(contract = contract::Api, period_ms = 20)]
impl Service {
    #[init]
    fn new(_: ()) -> phoxal::Result<Self> { Ok(Self) }

    #[handle(update)]
    fn update(&mut self, _: &mut phoxal::runtime::Context<'_, Self>, _: contract::Request) -> phoxal::Result<u32> {
        Ok(1)
    }
}

fn main() {}
