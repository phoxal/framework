//! A compiled, input-free Runtime used by the supervisor process-boundary test.
//!
//! The binary intentionally has no generated ports.  That makes it the
//! smallest complete runtime that can prove bundle admission, exact child
//! launch, Zenoh execution attachment, Ready liveliness, scheduling, and
//! orderly termination without pretending that typed port codecs already
//! exist.

use std::path::PathBuf;

use clap::Parser;
use phoxal::runtime::{InitContext, Runtime, StepContext};

#[derive(Debug, Parser)]
struct Cli {
    #[arg(long)]
    bundle_root: PathBuf,
}

struct ReferenceRuntime {
    marker: PathBuf,
}

#[phoxal::runtime(period_ms = 20, timeout_ms = 100, init_timeout_ms = 1_000)]
impl Runtime for ReferenceRuntime {
    type Config = ();
    type State = u64;
    type Inputs = ();
    type Outputs = ();

    fn init(&self, _ctx: &InitContext, _config: Self::Config) -> phoxal::Result<Self::State> {
        std::fs::write(&self.marker, b"initialized")?;
        Ok(0)
    }

    fn step(
        &self,
        _ctx: &StepContext,
        state: Self::State,
        _inputs: &Self::Inputs,
    ) -> phoxal::Result<(Self::State, Self::Outputs)> {
        if state == 0 {
            std::fs::write(&self.marker, b"stepped")?;
        }
        Ok((state.saturating_add(1), ()))
    }
}

#[phoxal::runtime::outputs]
impl ReferenceRuntime {}

fn main() -> phoxal::Result<()> {
    let bundle_root = Cli::parse().bundle_root;
    phoxal::runtime::run(ReferenceRuntime {
        marker: bundle_root.join("reference-runtime.marker"),
    })
}
