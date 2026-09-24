//! Offline generation from a package's `api/` tree and exact prepared inputs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use heck::{ToShoutySnakeCase, ToSnakeCase};
use prost::Message;
use prost_reflect::{DescriptorPool, MessageDescriptor, ServiceDescriptor};
use quote::ToTokens;
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
    generate(&package, &out, None)
}

/// Validates an exact proposed robot API before its authored document is replaced.
#[doc(hidden)]
pub fn validate_project_api(package: &Path, robot_source: &[u8], out: &Path) -> Result<(), Error> {
    generate(package, out, Some(robot_source))
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
    if compile(&unit, out, 0, false, false)?.service.is_none() {
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
#[serde(deny_unknown_fields)]
struct Selection {
    source: Source,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Source {
    Path(PathSource),
    Package(PackageSourceWrapper),
    Git(GitSourceWrapper),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathSource {
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageSourceWrapper {
    package: PackageSource,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GitSourceWrapper {
    git: GitSource,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageSource {
    name: String,
    version: String,
    registry: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GitSource {
    name: String,
    url: String,
    rev: String,
    path: Option<PathBuf>,
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
    additional_files: Vec<(String, String)>,
    children: BTreeMap<String, Module>,
}

fn generate(package: &Path, out: &Path, robot_override: Option<&[u8]>) -> Result<(), Error> {
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
    if robot_path.exists() || robot_override.is_some() {
        let source = match robot_override {
            Some(source) => source.to_vec(),
            None => read(&robot_path)?,
        };
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
    let mut symbols = BTreeMap::<String, (String, Vec<u8>)>::new();
    for (index, unit) in units.iter().enumerate() {
        let target = candidate_root.join(format!("u{index}"));
        fs::create_dir_all(&target).map_err(|source| Error::Path {
            path: target.clone(),
            source,
        })?;
        let result = compile(
            unit,
            &target,
            index,
            true,
            index == 0 && has_local_api && robot_override.is_none() && !robot_path.exists(),
        )?;
        validate_symbols(&result.pool, &unit.label, &mut symbols)?;
        for (name, (relative, content)) in &result.packages {
            insert_package(&mut tree, name, relative, content, &unit.label)?;
        }
        compiled.push(result);
    }
    materialize_merged(&mut tree, &candidate_root, "")?;

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
        let instance = if robot_path.exists() || robot_override.is_some() {
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
    if !identifier(&instance) {
        return Err(input(
            robot_path,
            format!("invalid participant `{instance}`"),
        ));
    }
    let (root, label) = match selection.source {
        Source::Package(PackageSourceWrapper { package: source }) => {
            if !identifier(&source.name)
                || !semver::Version::parse(&source.version)
                    .is_ok_and(|parsed| parsed.to_string() == source.version)
                || source
                    .registry
                    .as_ref()
                    .is_some_and(|registry| !identifier(registry))
            {
                return Err(input(
                    robot_path,
                    format!("{instance} has an invalid package source"),
                ));
            }
            let registry = source.registry.as_deref().unwrap_or("phoxal");
            (
                package_root
                    .join(".phoxal/registry")
                    .join(registry)
                    .join(&source.name)
                    .join(&source.version)
                    .join("api"),
                format!("{} {}", source.name, source.version),
            )
        }
        Source::Git(GitSourceWrapper { git: source }) => {
            if !identifier(&source.name)
                || source.url.trim().is_empty()
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
            (
                package_root
                    .join(".phoxal/git")
                    .join(&source.name)
                    .join(&source.rev)
                    .join("api"),
                format!("{} @ {}", source.name, source.rev),
            )
        }
        Source::Path(PathSource { path }) => {
            if path.as_os_str().is_empty() || !path.is_relative() {
                return Err(input(
                    robot_path,
                    format!("{instance} local source must be a nonempty relative path"),
                ));
            }
            (package_root.join(path).join("api"), "local path".to_owned())
        }
    };
    println!("cargo:rerun-if-changed={}", root.display());
    if !root.is_dir() {
        let message = if root.starts_with(package_root.join(".phoxal")) {
            format!(
                "Phoxal contracts are not prepared for {instance} {label}. Run `cargo phoxal prepare` from the robot project root."
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
                label: format!("{instance} {label}"),
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
    allow_multiple: bool,
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
    if services.len() > 1 && !allow_multiple {
        return Err(input(
            &unit.root,
            format!(
                "{} owns {} deployable services; exactly one is supported",
                unit.label,
                services.len()
            ),
        ));
    }
    let service = (services.len() == 1).then(|| services[0].clone());
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
        let mut content = content
            .replace(
                "static __PHOXAL_DESCRIPTOR_SET:",
                &format!("static __PHOXAL_DESCRIPTOR_SET_U{index}:"),
            )
            .replace(
                "include_bytes!(\"phoxal-descriptors.bin\")",
                &format!(
                    "include_bytes!(concat!(env!(\"OUT_DIR\"), \"/phoxal-api/u{index}/phoxal-descriptors.bin\"))"
                ),
            )
            .replace(
                "&super::super::__PHOXAL_DESCRIPTOR_SET",
                &format!("&super::super::__PHOXAL_DESCRIPTOR_SET_U{index}"),
            );
        if content.contains(&format!("&super::super::__PHOXAL_DESCRIPTOR_SET_U{index}"))
            && !content.contains(&format!("static __PHOXAL_DESCRIPTOR_SET_U{index}:"))
        {
            content.push_str(&format!(
                "\nstatic __PHOXAL_DESCRIPTOR_SET_U{index}: [u8; {}] = ::phoxal::contract::descriptor_frame::<{}>(include_bytes!(concat!(env!(\"OUT_DIR\"), \"/phoxal-api/u{index}/phoxal-descriptors.bin\")));\n",
                descriptors.len() + 16,
                descriptors.len() + 16,
            ));
        }
        write_stable(&path, content.as_bytes())?;
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
    _label: &str,
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
    if let Some((_, code)) = &node.file {
        if code != content {
            node.additional_files
                .push((relative.to_owned(), content.to_owned()));
        }
    } else {
        node.file = Some((relative.to_owned(), content.to_owned()));
    }
    Ok(())
}

fn validate_symbols(
    pool: &DescriptorPool,
    label: &str,
    symbols: &mut BTreeMap<String, (String, Vec<u8>)>,
) -> Result<(), Error> {
    let definitions = pool
        .all_messages()
        .map(|message| {
            (
                format!("message {}", message.full_name()),
                message.descriptor_proto().encode_to_vec(),
            )
        })
        .chain(pool.all_enums().map(|enumeration| {
            (
                format!("enum {}", enumeration.full_name()),
                enumeration.enum_descriptor_proto().encode_to_vec(),
            )
        }))
        .chain(pool.services().map(|service| {
            (
                format!("service {}", service.full_name()),
                service.service_descriptor_proto().encode_to_vec(),
            )
        }));
    for (name, definition) in definitions {
        if let Some((previous, accepted)) = symbols.get(&name) {
            if *accepted != definition {
                return Err(input(
                    Path::new(label),
                    format!("{name} differs between {previous} and {label}"),
                ));
            }
        } else {
            symbols.insert(name, (label.to_owned(), definition));
        }
    }
    Ok(())
}

fn materialize_merged(node: &mut Module, root: &Path, name: &str) -> Result<(), Error> {
    if !node.additional_files.is_empty() {
        let (first_path, first_code) = node
            .file
            .as_ref()
            .ok_or_else(|| input(root, "merged package has no first generated source"))?;
        let mut paths = vec![(first_path.clone(), first_code.clone())];
        paths.append(&mut node.additional_files);
        let mut seen = BTreeSet::new();
        let mut items = Vec::new();
        for (path, code) in paths {
            let parsed = syn::parse_file(&code)
                .map_err(|error| input(Path::new(&path), error.to_string()))?;
            for item in parsed.items {
                let key = item_key(&item);
                if key.as_ref().is_none_or(|key| seen.insert(key.clone())) {
                    items.push(item);
                }
            }
        }
        let merged = prettyplease::unparse(&syn::File {
            shebang: None,
            attrs: Vec::new(),
            items,
        });
        let relative = format!("merged/{name}.rs");
        let path = root.join(&relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::Path {
                path: parent.to_owned(),
                source,
            })?;
        }
        write_stable(&path, merged.as_bytes())?;
        node.file = Some((format!("phoxal-api/{relative}"), merged));
    }
    for (segment, child) in &mut node.children {
        let child_name = if name.is_empty() {
            segment.clone()
        } else {
            format!("{name}.{segment}")
        };
        materialize_merged(child, root, &child_name)?;
    }
    Ok(())
}

fn item_key(item: &syn::Item) -> Option<String> {
    match item {
        syn::Item::Struct(value) => Some(format!("struct {}", value.ident)),
        syn::Item::Enum(value) => Some(format!("enum {}", value.ident)),
        syn::Item::Mod(value) => Some(format!("mod {}", value.ident)),
        syn::Item::Const(value) => Some(format!("const {}", value.ident)),
        syn::Item::Static(value) => Some(format!("static {}", value.ident)),
        syn::Item::Type(value) => Some(format!("type {}", value.ident)),
        syn::Item::Fn(value) => Some(format!("fn {}", value.sig.ident)),
        syn::Item::Impl(value) => Some(format!(
            "impl {} for {}",
            value
                .trait_
                .as_ref()
                .map(|(_, path, _)| path.to_token_stream().to_string())
                .unwrap_or_default(),
            value.self_ty.to_token_stream()
        )),
        _ => None,
    }
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
    let mut message_types: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for method in service.methods() {
        for message in [method.input(), method.output()] {
            if message.full_name() != "google.protobuf.Empty" {
                message_types
                    .entry(message.name().to_owned())
                    .or_default()
                    .insert(message_path(&message));
            }
        }
    }
    for paths in message_types.values() {
        if let Some(path) = (paths.len() == 1).then(|| paths.iter().next()).flatten() {
            output.push_str(&format!("    pub use {path};\n"));
        }
    }
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
            "source:\n  git:\n    name: acme-motion\n    url: https://example.test/motion.git\n    rev: 0123456789abcdef0123456789abcdef01234567\n    path: services/motion\n",
        )
        .expect("Git selection");
        assert!(matches!(selected.source, Source::Git(_)));
        let invalid = serde_yaml::from_str::<Selection>(
            "source:\n  path: ../motion\n  package: { name: acme-motion, version: '1.2.3' }\n",
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
                "schema: phoxal/robot/v0\nrobot: {{ id: rover }}\nservices:\n  left_motion:\n    source: {{ path: {} }}\n  right_motion:\n    source: {{ path: {} }}\n",
                "../provider", "../provider"
            ),
        )?;
        generate(&robot, &out, None)?;
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

    #[test]
    fn equivalent_shared_messages_merge_across_package_scoped_sources()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let robot = directory.path().join("robot");
        let out = directory.path().join("out");
        fs::create_dir_all(&robot)?;
        fs::create_dir_all(&out)?;
        for (name, extra) in [
            ("alpha", ""),
            ("beta", "message Additional { string value = 1; }"),
        ] {
            let api = directory.path().join(name).join("api");
            fs::create_dir_all(&api)?;
            fs::write(
                api.join("shared.proto"),
                format!(
                    "syntax = \"proto3\"; package proof.shared.v1; message Shared {{ string value = 1; }} {extra}"
                ),
            )?;
            fs::write(
                api.join(format!("{name}.proto")),
                format!(
                    "syntax = \"proto3\"; package proof.{name}.v1; import \"shared.proto\"; import \"google/protobuf/empty.proto\"; service {name} {{ rpc Read(proof.shared.v1.Shared) returns (google.protobuf.Empty); }}"
                ),
            )?;
        }
        fs::write(
            robot.join("robot.yaml"),
            "schema: phoxal/robot/v0\nrobot: { id: rover }\nservices:\n  alpha:\n    source: { path: ../alpha }\n  beta:\n    source: { path: ../beta }\n",
        )?;
        generate(&robot, &out, None)?;
        let merged = fs::read_to_string(out.join("phoxal-api/merged/proof.shared.v1.rs"))?;
        assert_eq!(merged.matches("pub struct Shared").count(), 1);
        assert_eq!(merged.matches("pub struct Additional").count(), 1);
        fs::write(
            directory.path().join("beta/api/shared.proto"),
            "syntax = \"proto3\"; package proof.shared.v1; message Shared { int64 value = 1; } message Additional { string value = 1; }",
        )?;
        let error = generate(&robot, &out, None).expect_err("conflicting shared symbol must fail");
        assert!(
            error
                .to_string()
                .contains("message proof.shared.v1.Shared differs")
        );
        Ok(())
    }
}
