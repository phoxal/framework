//! The generated editor schemas are a published contract.
//!
//! An authored document that validates today must still validate tomorrow, so
//! the exact schema for every document kind is checked in beside this test.
//! Any change to a source DTO that moves the schema fails here and has to be
//! reblessed deliberately with `PHOXAL_BLESS_SCHEMAS=1`.
//!
//! `description` and `title` carry documentation, not constraints, so a
//! rebless that only moves those changes no document's validity. A rebless that
//! moves anything else - a type, a `required` list, an `enum`, a `const`,
//! `additionalProperties` - changes which documents an editor accepts, and is
//! the case this test exists to make visible.

use std::path::{Path, PathBuf};

use phoxal_manifest::source::DocumentKind;

fn golden_path(kind: DocumentKind) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(kind.schema_file_name())
}

#[test]
fn the_generated_schemas_match_their_golden_documents() {
    let bless = std::env::var_os("PHOXAL_BLESS_SCHEMAS").is_some();
    for kind in DocumentKind::ALL {
        let generated = serde_json::to_string_pretty(&kind.generate())
            .expect("a generated schema serializes")
            + "\n";
        let path = golden_path(kind);
        if bless {
            std::fs::create_dir_all(path.parent().expect("the golden directory has a parent"))
                .expect("the golden directory is writable");
            std::fs::write(&path, &generated).expect("the golden document is writable");
            continue;
        }
        let golden = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "missing golden schema {}: {error}; bless with PHOXAL_BLESS_SCHEMAS=1",
                path.display()
            )
        });
        assert_eq!(
            generated, golden,
            "the {kind:?} editor schema changed; if that is intended, rebless it with \
             PHOXAL_BLESS_SCHEMAS=1 and say why in the change description"
        );
    }
}
