use std::process::Command;

fn dependency_tree(package: &str) -> String {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(package)
        .join("Cargo.toml");
    let manifest_arg = manifest.to_string_lossy().into_owned();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = match Command::new(cargo)
        .args([
            "tree",
            "--manifest-path",
            &manifest_arg,
            "--edges",
            "normal",
            "--prefix",
            "none",
        ])
        .output()
    {
        Ok(output) => output,
        Err(error) => panic!("cargo tree must start: {error}"),
    };
    assert!(
        output.status.success(),
        "cargo tree failed for {}: {}",
        manifest.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    match String::from_utf8(output.stdout) {
        Ok(output) => output,
        Err(error) => panic!("cargo tree output is UTF-8: {error}"),
    }
}

fn assert_absent(tree: &str, forbidden: &[&str]) {
    for dependency in forbidden {
        assert!(
            !tree.lines().any(|line| line.starts_with(dependency)),
            "dependency tree unexpectedly contains {dependency}:\n{tree}"
        );
    }
}

#[test]
fn contract_consumer_has_no_sdk_runtime_dependencies() {
    let tree = dependency_tree("contract-consumer-fixture");
    assert!(tree.lines().any(|line| line.starts_with("phoxal-port v")));
    assert_absent(&tree, &["phoxal v", "tokio v", "zenoh v"]);
}

#[test]
fn sdk_port_feature_has_an_isolated_dependency_closure() {
    let tree = dependency_tree("port-consumer-fixture");
    assert!(tree.lines().any(|line| line.starts_with("phoxal v")));
    assert!(tree.lines().any(|line| line.starts_with("phoxal-port v")));
    assert_absent(&tree, &["tokio v", "zenoh v"]);
}
