use phoxal_macros::phoxal_api;

struct Body;

phoxal_api! {
    latest version v0.1 {
        data {
            topic sample: measurement Body;
        }
    }
}

fn main() {}
