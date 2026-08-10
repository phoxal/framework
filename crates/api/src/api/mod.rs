//! Modular authoring catalogue for the framework-owned wire contracts.

pub(crate) mod robot;
pub(crate) mod runtime;
pub(crate) mod supervisor;

phoxal_macros::phoxal_api_tree! {
    output generated;
    source crate::api;

    fragments {
        robot;
        runtime;
        supervisor;
    }
}
