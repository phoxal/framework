// The root brain's identity is fixed to `brain` and its Config is always `()`:
// neither is an authoring choice, so both arguments are compile errors rather
// than silently ignored keys.
#[phoxal::brain(id = "policy")]
struct RenamedBrain;

#[derive(Default)]
struct BrainConfig;

#[phoxal::brain(config = BrainConfig)]
struct ConfiguredBrain;

fn main() {}
