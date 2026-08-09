use phoxal_macros::phoxal_api;

phoxal_api! {
    latest version v0.1 {
        data {
            struct Body { value: u8 }
            topic sample: Sample<Body>;
        }
    }
}

fn main() {}
