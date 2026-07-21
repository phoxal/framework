// L2 (plan #00): the owner builder `Root` is `#[non_exhaustive]`, so downstream
// code cannot construct it by a direct `api::topic::internal::Root` literal. That
// path would otherwise bypass the `internal::new(cap)` owner-capability gate (a
// bare `Root` reaches `.node().leaf()` with no `OwnerCap`). The ONLY entry to the
// owner builder is `api::topic::internal::new(cap)`.
use phoxal::api as api;

fn main() {
    // ERROR: `Root` is `#[non_exhaustive]` and cannot be constructed outside its
    // defining crate; use `api::topic::internal::new(cap)`.
    let _root = api::topic::internal::Root;
}
