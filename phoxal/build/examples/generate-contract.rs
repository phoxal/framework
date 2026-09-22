//! Maintainer utility for bootstrapping checked-in contract candidates.
//!
//! Robot commands call the same library API automatically. This executable is
//! intentionally a low-level repository maintenance aid, not a consumer step.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let output = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("usage: generate-contract OUTPUT INCLUDE [--dependency PACKAGE DESCRIPTORS PROTO_PACKAGE RUST_PATH]... -- PROTO...")?;
    let include = arguments
        .next()
        .map(PathBuf::from)
        .ok_or("usage: generate-contract OUTPUT INCLUDE [--dependency PACKAGE DESCRIPTORS PROTO_PACKAGE RUST_PATH]... -- PROTO...")?;
    let mut arguments = arguments.peekable();
    let mut dependency_inputs = Vec::<(String, Vec<u8>, String, String)>::new();
    while arguments
        .peek()
        .is_some_and(|argument| argument == "--dependency")
    {
        let _ = arguments.next();
        let package = arguments
            .next()
            .ok_or("--dependency requires PACKAGE")?
            .into_string()
            .map_err(|_| "dependency package is not UTF-8")?;
        let descriptor_path = PathBuf::from(
            arguments
                .next()
                .ok_or("--dependency requires DESCRIPTORS")?,
        );
        let descriptors = std::fs::read(&descriptor_path)?;
        let proto_package = arguments
            .next()
            .ok_or("--dependency requires PROTO_PACKAGE")?
            .into_string()
            .map_err(|_| "dependency Protobuf package is not UTF-8")?;
        let rust_path = arguments
            .next()
            .ok_or("--dependency requires RUST_PATH")?
            .into_string()
            .map_err(|_| "dependency Rust path is not UTF-8")?;
        dependency_inputs.push((package, descriptors, proto_package, rust_path));
    }
    if arguments.peek().is_some_and(|argument| argument == "--") {
        let _ = arguments.next();
    }
    let protos = arguments.map(PathBuf::from).collect::<Vec<_>>();
    if protos.is_empty() {
        return Err("at least one owned .proto is required".into());
    }
    let dependencies = dependency_inputs
        .iter()
        .map(|(package, descriptors, _, _)| {
            phoxal_build::DependencyDescriptor::new(package, descriptors)
        })
        .collect::<Vec<_>>();
    let extern_paths = dependency_inputs
        .iter()
        .map(|(_, _, proto_package, rust_path)| (proto_package.as_str(), rust_path.as_str()))
        .collect::<Vec<_>>();
    let parent = output
        .parent()
        .ok_or("contract output must have a parent directory")?;
    let name = output
        .file_name()
        .ok_or("contract output must have a file name")?
        .to_string_lossy();
    let candidate = parent.join(format!(".{name}.candidate-{}", std::process::id()));
    let backup = parent.join(format!(".{name}.backup-{}", std::process::id()));
    if candidate.exists() || backup.exists() {
        return Err("contract generation candidate or backup already exists".into());
    }
    if let Err(error) = phoxal_build::generate_contract_package(
        &protos,
        &[include],
        &candidate,
        &dependencies,
        &extern_paths,
    ) {
        let _ = std::fs::remove_dir_all(&candidate);
        return Err(error.into());
    }
    let had_output = output.exists();
    if had_output {
        std::fs::rename(&output, &backup)?;
    }
    if let Err(error) = std::fs::rename(&candidate, &output) {
        if had_output {
            let _ = std::fs::rename(&backup, &output);
        }
        return Err(error.into());
    }
    if had_output {
        std::fs::remove_dir_all(backup)?;
    }
    Ok(())
}
