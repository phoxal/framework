//! Modular authoring catalogue for the framework-owned wire contracts.

pub(crate) mod robot;

phoxal_macros::phoxal_api_tree! {
    output generated;
    source crate::api;

    fragments {
        robot;
    }
}
