// Owning the transport is not a participant capability. A participant receives
// typed handles from its `SetupContext`; the session owner behind them, the
// inputs that open one, and the fabric they meet on are the framework's, and no
// consumer profile publishes them.
use phoxal::bus::{BusConfig, BusOwner, Router};

fn main() {}
