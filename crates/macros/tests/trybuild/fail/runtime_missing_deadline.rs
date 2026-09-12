#[phoxal_macros::runtime(period_ms = 20, timeout_ms = 100)]
impl Runtime for Service {
    type Config = ();
    type State = ();
    type Inputs = ();
    type Outputs = ();
}

struct Service;

fn main() {}
