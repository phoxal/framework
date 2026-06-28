// The mandatory #[phoxal(api = …)] selector is missing (D59/D60).
use phoxal::api::y2026_1 as api;
use phoxal::prelude::*;

#[derive(phoxal::Runtime)]
#[phoxal(id = "no-api")]
struct NoApi {
    target: Publisher<api::drive::Target>,
}

fn main() {}
