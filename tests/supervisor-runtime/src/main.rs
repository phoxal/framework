//! The supervisor process-boundary runtime binary.

mod contract;
#[cfg(test)]
mod path_probes;
mod reference_runtime;

use crate::reference_runtime::ReferenceRuntime;
use phoxal::runtime::RuntimeLaunch;

fn main() -> phoxal::Result<()> {
    let bundle_root = RuntimeLaunch::parse()?.bundle_root;
    phoxal::runtime::run(ReferenceRuntime {
        marker: bundle_root.join("reference-runtime.marker"),
    })
}
