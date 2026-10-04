//! Compiler-contract suite: the checks whose subject is compilation
//! itself.
//!
//! `contract_cases` pins the macro compile-fail guarantees; the pass
//! side of authored usage is proven deterministically by the `unit`
//! suite. `consumer_profiles` compiles every supported consumer feature
//! profile of the SDK as published.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Phoxal supports Linux and macOS only");

mod consumer_profiles;
mod contract_cases;
