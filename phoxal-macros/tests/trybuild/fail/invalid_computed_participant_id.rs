// An omitted `id = "…"` derives the participant id from the struct name via
// `to_kebab_case()`, which must satisfy the same identity-token grammar as an
// explicit id (see `invalid_participant_id.rs`). `to_kebab_case` only
// transforms ASCII case boundaries and passes every other character through
// unchanged, so it is not itself a proof of the grammar: a struct name of
// only underscores kebab-cases to the empty string.

use phoxal_macros::service;

#[service]
struct __;

fn main() {}
