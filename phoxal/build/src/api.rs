//! Offline generation from a package's `api/` tree and exact prepared inputs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use heck::{ToShoutySnakeCase, ToSnakeCase};
use prost_reflect::{DescriptorPool, MessageDescriptor, ServiceDescriptor};
use serde::Deserialize;

use crate::{Error, compile_contracts_impl};

/// Configuration for the package-local API build helper.
///
/// The empty configuration is intentional. Package and source selection come
/// only from Cargo's build environment and the authored `robot.yaml`.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct BuildApiConfig {
    _private: (),
}

/// Generates the current package's API under Cargo's `OUT_DIR`.
///
/// This function reads local and previously prepared Protobuf sources only.
/// Run `cargo phoxal prepare` first for registry and Git selections.
pub fn api(_config: BuildApiConfig) -> Result<(), Error> {
    let package = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .ok_or(Error::MissingEnvironment("CARGO_MANIFEST_DIR"))?;
    let out = std::env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .ok_or(Error::MissingEnvironment("OUT_DIR"))?;
    generate(&package, &out)
}

/// Checks a runnable participant's packaged API before preparation publishes it.
#[doc(hidden)]
pub fn validate_participant_api(api_root: &Path, out: &Path) -> Result<(), Error> {
    fs::create_dir_all(out).map_err(|source| Error::Path {
        path: out.to_owned(),
        source,
    })?;
    let unit = Unit {
        root: api_root.to_owned(),
        label: api_root.display().to_string(),
    };
    if compile(&unit, out, 0, false)?.service.is_none() {
        return Err(input(
            api_root,
            "runnable participant owns no Protobuf service",
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct Robot {
    #[serde(default)]
    services: BTreeMap<String, Selection>,
    robot: RobotSection,
}

#[derive(Deserialize, Default)]
struct RobotSection {
    #[serde(default)]
    components: BTreeMap<String, Component>,
}

#[derive(Deserialize)]
struct Component {
    #[serde(flatten)]
    selection: Selection,
    #[serde(default)]
    driver: Option<serde_yaml::Value>,
}

#[derive(Deserialize)]
struct Selection {
    package: String,
    version: String,
    #[serde(default)]
    source: Option<Source>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Source {
    Path(PathSource),
    Git(GitSource),
    Registry(RegistrySource),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathSource {
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GitSource {
    git: String,
    rev: String,
    path: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistrySource {
    registry: String,
}

struct Unit {
    root: PathBuf,
    label: String,
}

struct CompiledUnit {
    service: Option<ServiceDescriptor>,
    pool: DescriptorPool,
    packages: BTreeMap<String, (String, String)>,
}

#[derive(Default)]
struct Module {
    file: Option<(String, String)>,
    children: BTreeMap<String, Module>,
}

fn generate(package: &Path, out: &Path) -> Result<(), Error> {
    let mut units = Vec::<Unit>::new();
    let mut bindings = Vec::<(String, usize)>::new();
    let local_api = package.join("api");
    let has_local_api = local_api.is_dir();
    println!("cargo:rerun-if-changed={}", local_api.display());
    if has_local_api {
        units.push(Unit {
            root: local_api,
            label: "local API".into(),
        });
    }

    let robot_path = package.join("robot.yaml");
    println!("cargo:rerun-if-changed={}", robot_path.display());
    if robot_path.exists() {
        let source = read(&robot_path)?;
        let robot: Robot =
            serde_yaml::from_slice(&source).map_err(|source| Error::ApiDocument {
                path: robot_path.clone(),
                source,
            })?;
        for (instance, selection) in robot.services {
            add_selection(
                package,
                &robot_path,
                instance,
                selection,
                &mut units,
                &mut bindings,
            )?;
        }
        for (instance, component) in robot.robot.components {
            if component.driver.is_some() {
                add_selection(
                    package,
                    &robot_path,
                    instance,
                    component.selection,
                    &mut units,
                    &mut bindings,
                )?;
            }
        }
    }

    let candidate_root = out.join("phoxal-api-candidate");
    if candidate_root.exists() {
        fs::remove_dir_all(&candidate_root).map_err(|source| Error::Path {
            path: candidate_root.clone(),
            source,
        })?;
    }
    fs::create_dir_all(&candidate_root).map_err(|source| Error::Path {
        path: candidate_root.clone(),
        source,
    })?;

    let mut tree = Module::default();
    let mut compiled = Vec::with_capacity(units.len());
    for (index, unit) in units.iter().enumerate() {
        let target = candidate_root.join(format!("u{index}"));
        fs::create_dir_all(&target).map_err(|source| Error::Path {
            path: target.clone(),
            source,
        })?;
        let result = compile(unit, &target, index, true)?;
        for (name, (relative, content)) in &result.packages {
            insert_package(&mut tree, name, relative, content, &unit.label)?;
        }
        compiled.push(result);
    }

    let mut output = format!(
        "// @generated by phoxal-build; do not edit.\n\
         const _: () = assert!(::phoxal::__generated::API_GENERATOR_MARKER == {}, \"Phoxal build helper and SDK versions differ\");\n\
         #[doc(hidden)]\npub mod __contracts {{\n",
        version_marker(env!("CARGO_PKG_VERSION"))
    );
    emit_tree(&tree, &mut output, 1);
    output.push_str("}\n");

    let mut public_names = BTreeSet::new();
    for (instance, index) in bindings {
        let unit = &units[index];
        let compiled_unit = &compiled[index];
        let service = compiled_unit
            .service
            .as_ref()
            .ok_or_else(|| Error::ApiInput {
                path: unit.root.clone(),
                message: format!("{} owns no deployable Protobuf service", unit.label),
            })?;
        emit_binding(
            &mut output,
            &mut public_names,
            &instance,
            service,
            &compiled_unit.pool,
        )?;
    }
    if has_local_api && let Some(service) = &compiled[0].service {
        let instance = if robot_path.exists() {
            "brain".to_owned()
        } else {
            service.name().to_snake_case()
        };
        emit_binding(
            &mut output,
            &mut public_names,
            &instance,
            service,
            &compiled[0].pool,
        )?;
    }
    sync_tree(&candidate_root, &out.join("phoxal-api"))?;
    fs::remove_dir_all(&candidate_root).map_err(|source| Error::Path {
        path: candidate_root,
        source,
    })?;
    write_stable(&out.join("phoxal_api.rs"), output.as_bytes())
}

fn add_selection(
    package_root: &Path,
    robot_path: &Path,
    instance: String,
    selection: Selection,
    units: &mut Vec<Unit>,
    bindings: &mut Vec<(String, usize)>,
) -> Result<(), Error> {
    if !identifier(&instance) || !identifier(&selection.package) {
        return Err(input(
            robot_path,
            format!(
                "invalid participant `{instance}` or package `{}`",
                selection.package
            ),
        ));
    }
    let version = semver::Version::parse(&selection.version).map_err(|error| {
        input(
            robot_path,
            format!("{instance} needs an exact semantic version: {error}"),
        )
    })?;
    if version.to_string() != selection.version {
        return Err(input(
            robot_path,
            format!("{instance} version must be canonical and exact"),
        ));
    }
    let root = match selection.source {
        None => package_root
            .join(".phoxal/registry/phoxal")
            .join(&selection.package)
            .join(&selection.version)
            .join("api"),
        Some(Source::Registry(source)) => {
            if !identifier(&source.registry) {
                return Err(input(
                    robot_path,
                    format!("{instance} has an invalid registry"),
                ));
            }
            package_root
                .join(".phoxal/registry")
                .join(source.registry)
                .join(&selection.package)
                .join(&selection.version)
                .join("api")
        }
        Some(Source::Git(source)) => {
            if source.git.trim().is_empty()
                || source.rev.len() != 40
                || !source.rev.bytes().all(|byte| byte.is_ascii_hexdigit())
                || source
                    .path
                    .as_ref()
                    .is_some_and(|path| !safe_relative_path(path))
            {
                return Err(input(
                    robot_path,
                    format!("{instance} needs a Git URL and complete commit"),
                ));
            }
            package_root
                .join(".phoxal/git")
                .join(&selection.package)
                .join(source.rev)
                .join(&selection.version)
                .join("api")
        }
        Some(Source::Path(source)) => {
            if source.path.as_os_str().is_empty() || !source.path.is_relative() {
                return Err(input(
                    robot_path,
                    format!("{instance} local source must be a nonempty relative path"),
                ));
            }
            package_root.join(source.path).join("api")
        }
    };
    println!("cargo:rerun-if-changed={}", root.display());
    if !root.is_dir() {
        let message = if root.starts_with(package_root.join(".phoxal")) {
            format!(
                "Phoxal contracts are not prepared for {instance} {} {}. Run `cargo phoxal prepare` from the robot project root.",
                selection.package, selection.version
            )
        } else {
            format!(
                "local API directory for {instance} is missing: {}",
                root.display()
            )
        };
        return Err(input(&root, message));
    }
    let index = units
        .iter()
        .position(|unit| unit.root == root)
        .unwrap_or_else(|| {
            let index = units.len();
            units.push(Unit {
                root,
                label: format!("{instance} {} {}", selection.package, selection.version),
            });
            index
        });
    bindings.push((instance, index));
    Ok(())
}

fn compile(
    unit: &Unit,
    target: &Path,
    index: usize,
    track_changes: bool,
) -> Result<CompiledUnit, Error> {
    let mut protos = Vec::new();
    discover(&unit.root, &mut protos, track_changes)?;
    if protos.is_empty() {
        return Ok(CompiledUnit {
            service: None,
            pool: DescriptorPool::new(),
            packages: BTreeMap::new(),
        });
    }
    let original = protos
        .iter()
        .map(|path| Ok((path.clone(), read(path)?)))
        .collect::<Result<Vec<_>, Error>>()?;
    compile_contracts_impl(
        &protos,
        &[unit.root.as_path()],
        target,
        &[],
        &[],
        "phoxal-descriptors.bin",
        Some("::phoxal::__generated::prost"),
        track_changes,
    )?;
    let mut current_paths = Vec::new();
    discover(&unit.root, &mut current_paths, track_changes)?;
    if current_paths != protos
        || original
            .iter()
            .any(|(path, bytes)| !read(path).is_ok_and(|current| current == *bytes))
    {
        return Err(input(
            &unit.root,
            format!(
                "{} API sources changed during generation; retry the build",
                unit.label
            ),
        ));
    }
    let descriptors = read(&target.join("phoxal-descriptors.bin"))?;
    let pool = DescriptorPool::decode(descriptors.as_slice())?;
    let owned = protos
        .iter()
        .filter_map(|path| path.strip_prefix(&unit.root).ok())
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect::<BTreeSet<_>>();
    let services = pool
        .services()
        .filter(|service| owned.contains(service.parent_file().name()))
        .collect::<Vec<_>>();
    if services.len() > 1 {
        return Err(input(
            &unit.root,
            format!(
                "{} owns {} deployable services; exactly one is supported",
                unit.label,
                services.len()
            ),
        ));
    }
    let service = services.into_iter().next();
    let mut packages = BTreeMap::new();
    for entry in fs::read_dir(target).map_err(|source| Error::Path {
        path: target.to_owned(),
        source,
    })? {
        let entry = entry.map_err(|source| Error::Path {
            path: target.to_owned(),
            source,
        })?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(package_name) = name.strip_suffix(".rs") else {
            continue;
        };
        if package_name == "phoxal.api" || package_name == "google.protobuf" {
            continue;
        }
        let content =
            String::from_utf8(read(&path)?).map_err(|error| input(&path, error.to_string()))?;
        packages.insert(
            package_name.to_owned(),
            (format!("phoxal-api/u{index}/{name}"), content),
        );
    }
    Ok(CompiledUnit {
        service,
        pool,
        packages,
    })
}

fn discover(directory: &Path, files: &mut Vec<PathBuf>, track_changes: bool) -> Result<(), Error> {
    for entry in fs::read_dir(directory).map_err(|source| Error::Path {
        path: directory.to_owned(),
        source,
    })? {
        let entry = entry.map_err(|source| Error::Path {
            path: directory.to_owned(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| Error::Path {
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(input(&path, "API sources may not contain symlinks"));
        }
        if metadata.is_dir() {
            discover(&path, files, track_changes)?;
        } else if metadata.is_file() && path.extension().is_some_and(|ext| ext == "proto") {
            if track_changes {
                println!("cargo:rerun-if-changed={}", path.display());
            }
            files.push(path);
        }
    }
    files.sort();
    Ok(())
}

fn insert_package(
    tree: &mut Module,
    package: &str,
    relative: &str,
    content: &str,
    label: &str,
) -> Result<(), Error> {
    let mut node = tree;
    for segment in package.split('.') {
        if !identifier(segment) {
            return Err(input(
                Path::new(relative),
                format!("invalid Protobuf package `{package}`"),
            ));
        }
        node = node.children.entry(segment.to_snake_case()).or_default();
    }
    if let Some((previous, code)) = &node.file {
        if code != content {
            return Err(input(
                Path::new(relative),
                format!("Protobuf package `{package}` conflicts between {previous} and {label}"),
            ));
        }
    } else {
        node.file = Some((relative.to_owned(), content.to_owned()));
    }
    Ok(())
}

fn emit_tree(node: &Module, output: &mut String, depth: usize) {
    if let Some((relative, _)) = &node.file {
        output.push_str(&"    ".repeat(depth));
        output.push_str(&format!("include!({relative:?});\n"));
    }
    for (name, child) in &node.children {
        output.push_str(&"    ".repeat(depth));
        output.push_str(&format!("pub mod {name} {{\n"));
        emit_tree(child, output, depth + 1);
        output.push_str(&"    ".repeat(depth));
        output.push_str("}\n");
    }
}

fn emit_binding(
    output: &mut String,
    names: &mut BTreeSet<String>,
    instance: &str,
    service: &ServiceDescriptor,
    pool: &DescriptorPool,
) -> Result<(), Error> {
    let module = instance.to_snake_case();
    if module == "__contracts" || !names.insert(module.clone()) {
        return Err(input(
            Path::new("robot.yaml"),
            format!("API instance `{instance}` collides with another generated module"),
        ));
    }
    let package = service.package_name();
    let path = package
        .split('.')
        .map(ToSnakeCase::to_snake_case)
        .collect::<Vec<_>>()
        .join("::");
    let version = package
        .rsplit('.')
        .next()
        .unwrap_or(package)
        .to_snake_case();
    let service_module = service.name().to_snake_case();
    output.push_str(&format!(
        "pub mod {module} {{\n    pub use crate::api::__contracts::{path} as {version};\n"
    ));
    let lease = pool.get_extension_by_name("phoxal.api.lease");
    for method in service.methods() {
        let function = method.name().to_snake_case();
        let constant = method.name().to_shouty_snake_case();
        let method_path =
            format!("crate::api::__contracts::{path}::{service_module}::methods::{constant}");
        let response = message_path(&method.output());
        if method.is_server_streaming() {
            output.push_str(&format!(
                "    #[must_use]\n    pub fn {function}() -> ::phoxal::contract::Observation<{response}> {{\n        {method_path}.bind({instance:?})\n    }}\n"
            ));
        } else {
            let request = message_path(&method.input());
            output.push_str(&format!(
                "    #[must_use]\n    pub fn {function}(request: {request}) -> ::phoxal::contract::Call<{request}, {response}> {{\n        {method_path}.bind({instance:?}, request)\n    }}\n"
            ));
            if lease
                .as_ref()
                .is_some_and(|extension| method.options().has_extension(extension))
            {
                output.push_str(&format!(
                    "    #[must_use]\n    pub fn withdraw_{function}() -> ::phoxal::contract::Withdraw<{request}, {response}> {{\n        {method_path}.withdraw({instance:?})\n    }}\n"
                ));
            }
        }
    }
    output.push_str("}\n");
    Ok(())
}

fn message_path(message: &MessageDescriptor) -> String {
    if message.full_name() == "google.protobuf.Empty" {
        return "::phoxal::contract::Empty".into();
    }
    let package = message
        .parent_file()
        .package_name()
        .split('.')
        .map(ToSnakeCase::to_snake_case)
        .collect::<Vec<_>>()
        .join("::");
    let relative = message
        .full_name()
        .trim_start_matches(message.parent_file().package_name())
        .trim_start_matches('.');
    let mut names = relative.split('.').collect::<Vec<_>>();
    let name = names.pop().unwrap_or(message.name());
    let nested = names
        .into_iter()
        .map(ToSnakeCase::to_snake_case)
        .collect::<Vec<_>>()
        .join("::");
    if nested.is_empty() {
        format!("crate::api::__contracts::{package}::{name}")
    } else {
        format!("crate::api::__contracts::{package}::{nested}::{name}")
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
}

fn version_marker(version: &str) -> u64 {
    version.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}

fn safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn read(path: &Path) -> Result<Vec<u8>, Error> {
    fs::read(path).map_err(|source| Error::Path {
        path: path.to_owned(),
        source,
    })
}

fn write_stable(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    if fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(());
    }
    fs::write(path, bytes).map_err(|source| Error::Path {
        path: path.to_owned(),
        source,
    })
}

fn sync_tree(candidate: &Path, published: &Path) -> Result<(), Error> {
    fs::create_dir_all(published).map_err(|source| Error::Path {
        path: published.to_owned(),
        source,
    })?;
    let mut expected = BTreeSet::new();
    for entry in fs::read_dir(candidate).map_err(|source| Error::Path {
        path: candidate.to_owned(),
        source,
    })? {
        let entry = entry.map_err(|source| Error::Path {
            path: candidate.to_owned(),
            source,
        })?;
        let name = entry.file_name();
        expected.insert(name.clone());
        let source = entry.path();
        let destination = published.join(name);
        if source.is_dir() {
            sync_tree(&source, &destination)?;
        } else {
            write_stable(&destination, &read(&source)?)?;
        }
    }
    for entry in fs::read_dir(published).map_err(|source| Error::Path {
        path: published.to_owned(),
        source,
    })? {
        let entry = entry.map_err(|source| Error::Path {
            path: published.to_owned(),
            source,
        })?;
        if expected.contains(&entry.file_name()) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            fs::remove_dir_all(&path).map_err(|source| Error::Path {
                path: path.clone(),
                source,
            })?;
        } else {
            fs::remove_file(&path).map_err(|source| Error::Path {
                path: path.clone(),
                source,
            })?;
        }
    }
    Ok(())
}

fn input(path: &Path, message: impl Into<String>) -> Error {
    Error::ApiInput {
        path: path.to_owned(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_package_path_is_not_mistaken_for_a_local_source() {
        let selected: Selection = serde_yaml::from_str(
            "package: acme-motion\nversion: '1.2.3'\nsource:\n  git: https://example.test/motion.git\n  rev: 0123456789abcdef0123456789abcdef01234567\n  path: services/motion\n",
        )
        .expect("Git selection");
        assert!(matches!(selected.source, Some(Source::Git(_))));
        let invalid = serde_yaml::from_str::<Selection>(
            "package: acme-motion\nversion: '1.2.3'\nsource:\n  path: ../motion\n  registry: phoxal\n",
        );
        assert!(invalid.is_err());
    }

    #[test]
    fn one_local_api_supplies_two_instance_bindings() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let provider = directory.path().join("provider");
        let robot = directory.path().join("robot");
        let out = directory.path().join("out");
        fs::create_dir_all(provider.join("api"))?;
        fs::create_dir_all(&robot)?;
        fs::create_dir_all(&out)?;
        fs::write(
            provider.join("api/motion.proto"),
            "syntax = \"proto3\"; package proof.motion.v1; import \"google/protobuf/empty.proto\"; message ManualRequest { double speed_mps = 1; } service Motion { rpc Manual(ManualRequest) returns (google.protobuf.Empty); }",
        )?;
        fs::write(
            robot.join("robot.yaml"),
            format!(
                "schema: phoxal/robot/v0\nrobot: {{ id: rover }}\nservices:\n  left_motion:\n    package: proof-motion\n    version: '1.2.3'\n    source: {{ path: {} }}\n  right_motion:\n    package: proof-motion\n    version: '1.2.3'\n    source: {{ path: {} }}\n",
                "../provider", "../provider"
            ),
        )?;
        generate(&robot, &out)?;
        let code = fs::read_to_string(out.join("phoxal_api.rs"))?;
        assert!(code.contains("pub mod left_motion"));
        assert!(code.contains("pub mod right_motion"));
        assert_eq!(
            code.matches("include!(\"phoxal-api/u0/proof.motion.v1.rs\")")
                .count(),
            1
        );
        assert!(!out.join("Cargo.toml").exists());
        Ok(())
    }
}
