//! Modular authoring catalogue for the pre-v1 Robot API.

pub(crate) mod v0_1;

phoxal_macros::phoxal_api_tree! {
    output generated;
    source crate::api;

    versions {
        latest version v0_1;
    }

    fragments {
        v0_1;
    }
}
