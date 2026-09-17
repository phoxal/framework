//! The source digest is a shared bundle wire format, including length prefixes.
use phoxal_project::{BundleSourceFile, digest_source_files};

#[test]
fn source_digest_has_a_fixed_wire_vector_and_canonical_order() {
    let mut files = vec![
        BundleSourceFile {
            path: "b".into(),
            sha256: "22".into(),
            bytes: 9,
        },
        BundleSourceFile {
            path: "a".into(),
            sha256: "11".into(),
            bytes: 3,
        },
    ];
    let digest = digest_source_files(&files);
    files.reverse();
    assert_eq!(digest, digest_source_files(&files));
    assert_eq!(
        digest,
        "6bba922dab7ab1946ffb4f591cd17c8ddd44a0195de23988aca7def7578b5156"
    );
    files[0].bytes += 1;
    assert_ne!(digest, digest_source_files(&files));
}
