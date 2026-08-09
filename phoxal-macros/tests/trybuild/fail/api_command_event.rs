use phoxal_macros::phoxal_api;

struct Payload;

phoxal_api! {
    latest version v0.1 {
        data {
            command cmd_event: Event<Payload>;
        }
    }
}

fn main() {}
