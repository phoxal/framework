use phoxal_macros::phoxal_api_tree;

phoxal_api_tree! {
    output generated;
    source crate;
    versions {
        latest version v1_1 extends v1_0;
    }
    fragments { missing; }
}

fn main() {}
