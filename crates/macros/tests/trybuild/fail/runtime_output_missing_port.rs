#[phoxal_macros::outputs]
struct Outputs {
    #[phoxal_macros::event(max_items = 4, max_bytes = 64)]
    events: Vec<u32>,
}

fn main() {}
