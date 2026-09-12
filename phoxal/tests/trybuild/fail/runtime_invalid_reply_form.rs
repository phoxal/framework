#[phoxal::runtime::outputs]
struct Outputs {
    #[phoxal::runtime::outputs::reply(commands, max_items = 2, max_bytes = 64)]
    replies: Vec<u32>,
}

fn main() {}
