// A participant id is spliced directly between JSON quotes in the embedded
// linker-section metadata (no escaping) and used as a literal Zenoh key
// segment, so it is restricted to the framework's identity-token grammar:
// non-empty, lowercase ASCII letters, digits, `_`, or `-` only.

use phoxal_macros::service;

#[service(id = "Not Valid!")]
struct Invalid;

fn main() {}
