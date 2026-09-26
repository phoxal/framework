//! Offline generation from a package's `api/` tree and exact prepared inputs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use heck::{ToShoutySnakeCase, ToSnakeCase};
use prost::Message;
use prost_reflect::DescriptorPool;
use quote::ToTokens;
use serde::Deserialize;

use crate::manifest::{self, ResolvedService};
use crate::{Error, compile_protos_impl};

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
pub fn validate_project_api(package: &Path, robot_source: &[u8], out: &Path) -> Result<(), Error> {
    generate(package, out, Some(robot_source))
}

/// Loads, compiles, and validates the service declaration beside an `api/`
/// root for composition checking, reusing the generation pipeline.
///
/// Returns `None` for legacy participants that declare endpoints only in
/// compiled Protobuf services.
pub fn participant_declaration(
    api_root: &Path,
) -> Result<Option<crate::manifest::DeclarationEvidence>, Error> {
    let Some(package) = api_root.parent() else {
        return Ok(None);
    };
    let Some(declaration) = unit_declaration(package)? else {
        return Ok(None);
    };
    let scratch = unique_scratch("phoxal-declaration");
    fs::create_dir_all(&scratch).map_err(|source| Error::Path {
        path: scratch.clone(),
        source,
    })?;
    let unit = Unit {
        root: api_root.to_owned(),
        label: api_root.display().to_string(),
        declaration: Some(declaration),
    };
    let result = compile(&unit, &scratch, 0, false, None);
    let descriptors = result
        .as_ref()
        .ok()
        .and_then(|_| read(&scratch.join("phoxal-descriptors.bin")).ok())
        .unwrap_or_default();
    let cleaned = fs::remove_dir_all(&scratch);
    let compiled = result?;
    cleaned.map_err(|source| Error::Path {
        path: scratch,
        source,
    })?;
    Ok(compiled
        .manifest
        .map(|service| crate::manifest::DeclarationEvidence {
            service,
            descriptors,
        }))
}

/// Checks a runnable participant's packaged API before preparation publishes it.
pub fn validate_participant_api(api_root: &Path, out: &Path) -> Result<(), Error> {
    fs::create_dir_all(out).map_err(|source| Error::Path {
        path: out.to_owned(),
        source,
    })?;
    let Some(package) = api_root.parent() else {
        return Err(input(
            api_root,
            "runnable participant source has no package root",
        ));
    };
    let declaration = unit_declaration(package)?;
    let unit = Unit {
        root: api_root.to_owned(),
        label: api_root.display().to_string(),
        declaration,
    };
    let result = compile(&unit, out, 0, false, None)?;
    if result.manifest.is_none() {
        return Err(input(
            api_root,
            "runnable participant owns no endpoint declaration \
             (service.yaml or component.yaml sections)",
        ));
    }
    Ok(())
}

/// The robot document as generation consumes it: the same authored surface
/// the SDK's artifact DTO validates, with carrier fields the generator does
/// not read kept as opaque values. Both parsers reject unknown fields so a
/// typo cannot silently drop a declaration on either path.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Robot {
    schema: String,
    robot: RobotSection,
    /// The brain's embedded endpoint declaration, if it declares one.
    #[serde(default)]
    brain: Option<manifest::BrainSection>,
    #[serde(default)]
    services: BTreeMap<String, Selection>,
    #[serde(default)]
    #[allow(
        dead_code,
        reason = "the key belongs to this document; the values are cargo-phoxal's to validate and resolve"
    )]
    connections: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RobotSection {
    #[allow(dead_code, reason = "robot identity carrier; consumed by cargo-phoxal")]
    id: String,
    #[serde(default)]
    #[allow(dead_code, reason = "native model carrier; consumed by cargo-phoxal")]
    model: Option<PathBuf>,
    #[serde(default)]
    components: BTreeMap<String, Component>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Component {
    source: Source,
    #[serde(default)]
    #[allow(
        dead_code,
        reason = "component selection carrier; consumed by cargo-phoxal"
    )]
    binary: Option<String>,
    #[allow(dead_code, reason = "native mount carrier; consumed by cargo-phoxal")]
    mount_site: String,
    #[serde(default)]
    driver: Option<serde_yaml::Value>,
    #[serde(default)]
    #[allow(
        dead_code,
        reason = "component configuration carrier; consumed by cargo-phoxal"
    )]
    config: Option<serde_yaml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    source: Source,
    #[serde(default)]
    #[allow(
        dead_code,
        reason = "binary selection carrier; consumed by cargo-phoxal"
    )]
    binary: Option<String>,
    #[serde(default)]
    #[allow(
        dead_code,
        reason = "service configuration carrier; consumed by cargo-phoxal"
    )]
    config: Option<serde_yaml::Value>,
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
    /// The unit's authored endpoint declaration, when it has one.
    declaration: Option<(manifest::ServiceDocument, PathBuf)>,
}

/// Returns a package's authored endpoint declaration.
///
/// A service package declares endpoints in `service.yaml`; a component
/// package embeds the same sections in `component.yaml`. Declaring both is
/// rejected: one package has exactly one endpoint authority.
fn unit_declaration(
    package_root: &Path,
) -> Result<Option<(manifest::ServiceDocument, PathBuf)>, Error> {
    let service = package_root.join(manifest::FILE_NAME);
    let component = package_root.join(manifest::COMPONENT_FILE_NAME);
    match (service.is_file(), component.is_file()) {
        (true, true) => Err(input(
            &service,
            format!(
                "this package declares endpoints in both {} and {}; \
                 exactly one endpoint authority is allowed",
                manifest::FILE_NAME,
                manifest::COMPONENT_FILE_NAME
            ),
        )),
        (true, false) => {
            let document = manifest::parse_document(&read(&service)?, &service)?;
            Ok(Some((document, service)))
        }
        (false, true) => {
            let document = manifest::parse_component_document(&read(&component)?, &component)?;
            Ok(Some((document, component)))
        }
        (false, false) => Ok(None),
    }
}

struct CompiledUnit {
    manifest: Option<ResolvedService>,
    /// Crate-private endpoint descriptor constants for the manifest unit.
    methods: Option<String>,
    pool: DescriptorPool,
    packages: Vec<(String, String, String)>,
}

#[derive(Default)]
struct Module {
    file: Option<(String, String)>,
    additional_files: Vec<(String, String)>,
    children: BTreeMap<String, Module>,
}

/// One robot project's assembled generation units and instance bindings.
struct Assembled {
    units: Vec<Unit>,
    bindings: Vec<(String, usize)>,
    /// The local unit is present (an `api/` tree or any declaration).
    has_local_api: bool,
}

/// Parses the robot document and collects every generation unit: the local
/// unit first (the robot.yaml brain section for a robot project, otherwise
/// the package's own declaration), then one unit per selected participant.
fn assemble_units(
    package: &Path,
    robot_override: Option<&[u8]>,
    emit_changes: bool,
) -> Result<Assembled, Error> {
    let mut units = Vec::<Unit>::new();
    let mut bindings = Vec::<(String, usize)>::new();
    let local_api = package.join("api");
    let robot_path = package.join("robot.yaml");
    let robot_source = if robot_path.exists() || robot_override.is_some() {
        let source = match robot_override {
            Some(source) => source.to_vec(),
            None => read(&robot_path)?,
        };
        if emit_changes && robot_path.is_file() {
            println!("cargo:rerun-if-changed={}", robot_path.display());
        }
        Some(source)
    } else {
        None
    };
    // The owning document is decided by package role. A robot project's
    // brain is instructed by robot.yaml: the brain section is the local
    // unit's only endpoint authority — an empty brain has an empty contract,
    // and unrelated declaration files beside robot.yaml belong to other
    // operations: they are neither read nor rejected here. A non-robot
    // package reads service.yaml or component.yaml.
    let (robot, local_document) = match robot_source {
        Some(source) => {
            manifest::reject_duplicate_keys(&source, &robot_path, "robot.yaml")?;
            let parsed: Robot =
                serde_yaml::from_slice(&source).map_err(|source| Error::ApiDocument {
                    path: robot_path.clone(),
                    source,
                })?;
            if parsed.schema != manifest::ROBOT_SCHEMA {
                return Err(input(
                    &robot_path,
                    format!(
                        "robot.yaml declares schema {:?}; this build supports {:?}",
                        parsed.schema,
                        manifest::ROBOT_SCHEMA
                    ),
                ));
            }
            let brain = parsed
                .brain
                .as_ref()
                .map(manifest::BrainSection::document)
                .unwrap_or_default();
            (Some(parsed), Some((brain, robot_path.clone())))
        }
        None => {
            let local = unit_declaration(package)?;
            if emit_changes && let Some((_, path)) = &local {
                println!("cargo:rerun-if-changed={}", path.display());
            }
            (None, local)
        }
    };
    let has_local_api = local_api.is_dir() || local_document.is_some();
    if emit_changes {
        println!("cargo:rerun-if-changed={}", local_api.display());
    }
    if has_local_api {
        units.push(Unit {
            root: local_api,
            label: "local API".into(),
            declaration: local_document,
        });
    }
    if let Some(robot) = robot {
        for (instance, selection) in robot.services {
            add_selection(
                package,
                &robot_path,
                instance,
                selection.source,
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
                    component.source,
                    &mut units,
                    &mut bindings,
                )?;
            }
        }
    }
    Ok(Assembled {
        units,
        bindings,
        has_local_api,
    })
}

/// Resolves a robot project's brain declaration for composition checking.
///
/// Returns `None` when the brain declares no endpoints.  The brain's types
/// resolve against the same union of participant schemas that code
/// generation uses.
pub fn brain_declaration(
    package: &Path,
    robot_source: &[u8],
) -> Result<Option<crate::manifest::DeclarationEvidence>, Error> {
    let assembled = assemble_units(package, Some(robot_source), false)?;
    let units = &assembled.units;
    if units
        .first()
        .and_then(|unit| unit.declaration.as_ref())
        .is_none_or(|(document, _)| document.is_empty())
    {
        return Ok(None);
    }
    let union_pool = union_resolution_pool(units)?;
    let scratch_root = unique_scratch("phoxal-brain");
    let scratch = scratch_root.join("u0");
    fs::create_dir_all(&scratch).map_err(|source| Error::Path {
        path: scratch.clone(),
        source,
    })?;
    let result = compile(&units[0], &scratch, 0, false, union_pool.as_ref());
    // The brain's types resolve against the union of participant schemas, so
    // its declaration evidence must carry that closure for cross-side
    // definition agreement; without participants the local unit's own
    // descriptor file is the closure.
    let descriptors = union_pool
        .as_ref()
        .map(|pool| pool.encode_to_vec())
        .or_else(|| {
            result
                .as_ref()
                .ok()
                .and_then(|_| read(&scratch.join("phoxal-descriptors.bin")).ok())
        })
        .unwrap_or_default();
    let cleaned = fs::remove_dir_all(&scratch_root);
    let compiled = result?;
    cleaned.map_err(|source| Error::Path {
        path: scratch_root,
        source,
    })?;
    Ok(compiled
        .manifest
        .map(|service| crate::manifest::DeclarationEvidence {
            service,
            descriptors,
        }))
}

fn generate(package: &Path, out: &Path, robot_override: Option<&[u8]>) -> Result<(), Error> {
    let Assembled {
        units,
        bindings,
        has_local_api,
    } = assemble_units(package, robot_override, true)?;
    let robot_path = package.join("robot.yaml");

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
    // YAML type references resolve against every unit's schemas so a brain
    // can declare call requirements into a participant's message contracts.
    let any_manifest = units.iter().any(|unit| unit.declaration.is_some());
    let union_pool = if any_manifest {
        union_resolution_pool(&units)?
    } else {
        None
    };

    let mut tree = Module::default();
    let mut compiled = Vec::with_capacity(units.len());
    let mut symbols = BTreeMap::<String, (String, Vec<u8>)>::new();
    for (index, unit) in units.iter().enumerate() {
        let target = candidate_root.join(format!("u{index}"));
        fs::create_dir_all(&target).map_err(|source| Error::Path {
            path: target.clone(),
            source,
        })?;
        let result = compile(unit, &target, index, true, union_pool.as_ref())?;
        validate_symbols(&result.pool, &unit.label, &mut symbols)?;
        for (name, relative, content) in &result.packages {
            insert_package(&mut tree, name, relative, content, &unit.label)?;
        }
        compiled.push(result);
    }
    materialize_merged(&mut tree, &candidate_root, "")?;

    let mut output = format!(
        "// @generated by phoxal-build; do not edit.\n\
         const _: () = assert!(::phoxal::generated::API_GENERATOR_MARKER == {}, \"Phoxal build helper and SDK versions differ\");\n\
         /// Generated message and enum definitions, preserving Protobuf\n\
         /// package namespaces.\n\
         pub mod types {{\n",
        version_marker(env!("CARGO_PKG_VERSION"))
    );
    emit_tree(&tree, &mut output, 1);
    output.push_str("}\n");

    // Endpoint descriptor constants stay crate-private bookkeeping: they are
    // referenced by generated bindings, requirement handles, and provider
    // glue, never by authored code.
    let mut methods_modules = compiled.iter().enumerate().filter_map(|(index, unit)| {
        unit.methods
            .as_ref()
            .map(|methods| format!("    pub mod u{index} {{\n{methods}    }}\n"))
    });
    if let Some(first) = methods_modules.next() {
        output.push_str(
            "/// Generated endpoint descriptor constants, one module per\n\
             /// composed service unit.\n\
             #[allow(dead_code, reason = \"a composing crate does not reference every participant's constants\")]\n\
             pub(crate) mod service_methods {\n",
        );
        output.push_str(&first);
        for rest in methods_modules {
            output.push_str(&rest);
        }
        output.push_str("}\n");
    }

    let mut public_names = BTreeSet::new();
    for (instance, index) in bindings {
        let unit = &units[index];
        let compiled_unit = &compiled[index];
        let binding = service_binding(compiled_unit, index).ok_or_else(|| Error::ApiInput {
            path: unit.root.clone(),
            message: format!(
                "{} owns no deployable Protobuf service or service.yaml declaration",
                unit.label
            ),
        })?;
        emit_binding(&mut output, &mut public_names, &instance, &binding)?;
    }
    if has_local_api && let Some(resolved) = &compiled[0].manifest {
        // A robot project's brain is instructed by robot.yaml: it attaches
        // its declared endpoints (binding instance "brain") exactly like any
        // other runtime, and composition binds participants through the same
        // document.  A standalone service package has no composition
        // instance of its own; it is consumed through a composing project.
        if robot_path.exists() || robot_override.is_some() {
            let binding = service_binding(&compiled[0], 0).ok_or_else(|| Error::ApiInput {
                path: units[0].root.clone(),
                message: "resolved service declaration produced no binding".to_owned(),
            })?;
            emit_binding(&mut output, &mut public_names, "brain", &binding)?;
            if !resolved.endpoint_names().is_empty() && compiled[0].methods.is_none() {
                return Err(input(
                    &units[0].root,
                    "the brain declares endpoints but owns no api/ schemas; add the \
                     referenced vocabulary beside the robot project",
                ));
            }
        }
        emit_calls_module(&mut output, resolved);
        emit_projections_module(&mut output, resolved);
        let provider = crate::provider::emit_provider(resolved);
        write_stable(&out.join("phoxal-provider.rs"), provider.as_bytes())?;
    } else if has_local_api {
        // A removed or renamed manifest must not leave stale attachment.
        let stale = out.join("phoxal-provider.rs");
        if stale.is_file() {
            fs::remove_file(&stale).map_err(|source| Error::Path {
                path: stale.clone(),
                source,
            })?;
        }
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
    source: Source,
    units: &mut Vec<Unit>,
    bindings: &mut Vec<(String, usize)>,
) -> Result<(), Error> {
    if !identifier(&instance) {
        return Err(input(
            robot_path,
            format!("invalid participant `{instance}`"),
        ));
    }
    let (root, label) = match source {
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
    if root.is_dir() {
        println!("cargo:rerun-if-changed={}", root.display());
    }
    // A built-in-only contract has no api/ directory; its declaration still
    // selects the participant.  Track the declaration for rebuilds without
    // leaving a permanently dirty marker on packages without one.
    let package = root
        .parent()
        .ok_or_else(|| input(&root, format!("{instance} source has no package root")))?;
    let declaration = unit_declaration(package)?;
    if let Some((_, path)) = &declaration {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    if !root.is_dir() && declaration.is_none() {
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
                declaration,
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
    resolution_pool: Option<&DescriptorPool>,
) -> Result<CompiledUnit, Error> {
    let declaration = unit.declaration.as_ref();

    let mut protos = Vec::new();
    discover(&unit.root, &mut protos, track_changes)?;
    let builtin_robotics = declaration
        .is_some_and(|(document, _)| document.references_package("phoxal.robotics.v1"))
        && !unit
            .root
            .join("phoxal/robotics/v1/robotics.proto")
            .is_file();
    let mut inputs = protos.clone();
    if builtin_robotics {
        // A YAML-only reference to the built-in robotics vocabulary supplies
        // the canonical schema as a generation root so its descriptors stay
        // in the retained closure without an authored copy.
        inputs.push(crate::include_dir().join("phoxal/robotics/v1/robotics.proto"));
        inputs.sort();
    }
    if inputs.is_empty() {
        let manifest = declaration
            .map(|(document, path)| {
                manifest::resolve_document(
                    document,
                    resolution_pool.unwrap_or(&DescriptorPool::new()),
                    path,
                )
            })
            .transpose()?;
        return Ok(CompiledUnit {
            manifest,
            methods: None,
            pool: DescriptorPool::new(),
            packages: Vec::new(),
        });
    }
    let original = inputs
        .iter()
        .map(|path| Ok((path.clone(), read(path)?)))
        .collect::<Result<Vec<_>, Error>>()?;
    let extern_paths: &[(&str, &str)] = if declaration.is_some() {
        &[(".phoxal.robotics.v1", "::phoxal::robotics")]
    } else {
        &[]
    };
    // A built-in-only unit has no api/ directory; its package root still
    // anchors relative include resolution for the helper-supplied schemas.
    let include_root = if unit.root.is_dir() {
        unit.root.clone()
    } else {
        unit.root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| unit.root.clone())
    };
    compile_protos_impl(
        &inputs,
        &[include_root.as_path()],
        target,
        extern_paths,
        "phoxal-descriptors.bin",
        Some("::phoxal::generated::prost"),
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
    let owned = inputs
        .iter()
        .filter_map(|path| path.strip_prefix(&unit.root).ok())
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect::<BTreeSet<_>>();
    if pool
        .services()
        .any(|service| owned.contains(service.parent_file().name()))
    {
        return Err(input(
            &unit.root,
            format!(
                "{} authors Protobuf service declarations; Protobuf files carry message \
                 definitions only — declare endpoints in service.yaml, component.yaml \
                 sections, or the robot.yaml brain section",
                unit.label
            ),
        ));
    }
    let resolved_manifest = declaration
        .map(|(document, path)| {
            manifest::resolve_document(document, resolution_pool.unwrap_or(&pool), path)
        })
        .transpose()?;
    let mut methods = None;
    let mut packages = Vec::new();
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
        write_stable(&path, content.as_bytes())?;
        packages.push((
            package_name.to_owned(),
            format!("phoxal-api/u{index}/{name}"),
            content,
        ));
    }
    if let Some(resolved) = &resolved_manifest {
        methods = Some(emit_methods_module(resolved, index, descriptors.len()));
    }
    packages.sort_by(|left, right| left.1.cmp(&right.1));
    Ok(CompiledUnit {
        manifest: resolved_manifest,
        methods,
        pool,
        packages,
    })
}

/// Builds one descriptor pool spanning every unit's Protobuf sources.
///
/// A robot project's brain may declare cross-service call requirements whose
/// request and response messages belong to a selected participant's schema;
/// YAML type references resolve against the whole composition, while code
/// generation still emits per-unit trees exactly as before.
///
/// Each unit compiles against only its own package root — sharing roots in
/// one invocation makes protoc reject units that author copies of the same
/// relative schema path — and the per-unit descriptor sets fold into one
/// pool.  Folding preserves package-local schema identity: equal relative
/// file names from independent services rename instead of shadowing,
/// identical duplicate definitions collapse onto their first provider, and
/// different definitions under one qualified name are rejected explicitly.
fn union_resolution_pool(units: &[Unit]) -> Result<Option<DescriptorPool>, Error> {
    let mut merge = UnionMerge::default();
    for (unit_index, unit) in units.iter().enumerate() {
        let mut files = Vec::new();
        discover(&unit.root, &mut files, false)?;
        // A participant with no api/ directory still resolves YAML-only
        // references against the packaged built-in vocabulary, so the union
        // pool must carry the same schema the per-unit compile would add.
        let document = unit
            .declaration
            .as_ref()
            .map(|(document, _)| document.clone());
        let builtin_robotics = document
            .as_ref()
            .is_some_and(|document| document.references_package("phoxal.robotics.v1"))
            && !unit
                .root
                .join("phoxal/robotics/v1/robotics.proto")
                .is_file();
        if builtin_robotics {
            files.push(crate::include_dir().join("phoxal/robotics/v1/robotics.proto"));
        }
        if files.is_empty() {
            continue;
        }
        let include_root = if unit.root.is_dir() {
            unit.root.clone()
        } else {
            unit.root
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| unit.root.clone())
        };
        let set = compile_descriptor_set(
            &files,
            &[include_root.as_path()],
            "phoxal-union-descriptors.bin",
        )?;
        merge.fold(unit_index, set)?;
    }
    if merge.files.is_empty() {
        return Ok(None);
    }
    let bytes =
        prost::Message::encode_to_vec(&prost_types::FileDescriptorSet { file: merge.files });
    DescriptorPool::decode(bytes.as_slice())
        .map(Some)
        .map_err(Error::Descriptor)
}

/// Bookkeeping for folding per-unit descriptor sets into one resolution pool.
#[derive(Default)]
struct UnionMerge {
    files: Vec<prost_types::FileDescriptorProto>,
    /// Qualified definition -> (providing pool file, kind-tagged identity).
    definitions: BTreeMap<String, (String, Vec<u8>)>,
    used_names: BTreeSet<String>,
}

impl UnionMerge {
    /// Folds one unit's descriptor set, renaming colliding file names,
    /// collapsing identical duplicate definitions, and rejecting conflicting
    /// definitions under one qualified name.
    fn fold(
        &mut self,
        unit_index: usize,
        set: prost_types::FileDescriptorSet,
    ) -> Result<(), Error> {
        // Final names are decided for the whole set first so dependency
        // rewrites see one consistent mapping; a file whose definitions are
        // all already present keeps its name and is skipped as a duplicate.
        let renames: BTreeMap<String, String> = set
            .file
            .iter()
            .filter(|file| self.used_names.contains(file.name()))
            .filter(|file| !self.definitions_covered(file))
            .map(|file| {
                (
                    file.name().to_owned(),
                    format!("phoxal-union-{unit_index}/{}", file.name()),
                )
            })
            .collect();
        let mut stripped: BTreeMap<String, String> = BTreeMap::new();
        let first_pushed = self.files.len();
        for mut file in set.file {
            if let Some(renamed) = renames.get(file.name()).cloned() {
                file.name = Some(renamed);
                for dependency in &mut file.dependency {
                    if let Some(renamed) = renames.get(dependency.as_str()) {
                        *dependency = renamed.clone();
                    }
                }
            }
            self.merge_definitions(&mut file, &mut stripped)?;
            // An emptied duplicate under a taken name disappears; any other
            // file stays so in-set imports keep resolving (type references
            // that lost their definitions gain provider edges below).
            if definitions_remain(&file) || !self.used_names.contains(file.name()) {
                self.used_names.insert(file.name().to_owned());
                self.files.push(file);
            }
        }
        // Type references resolve inside each file's dependency closure, so
        // every file referencing a stripped duplicate gains an edge to the
        // pool file that still provides that definition.
        for file in &mut self.files[first_pushed..] {
            add_provider_dependencies(file, &stripped);
        }
        Ok(())
    }

    fn merge_definitions(
        &mut self,
        file: &mut prost_types::FileDescriptorProto,
        stripped: &mut BTreeMap<String, String>,
    ) -> Result<(), Error> {
        let package = file.package().to_owned();
        let file_name = file.name().to_owned();
        file.message_type = merge_entries(
            std::mem::take(&mut file.message_type),
            &package,
            "message",
            &file_name,
            &mut self.definitions,
            stripped,
        )?;
        file.enum_type = merge_entries(
            std::mem::take(&mut file.enum_type),
            &package,
            "enum",
            &file_name,
            &mut self.definitions,
            stripped,
        )?;
        file.service = merge_entries(
            std::mem::take(&mut file.service),
            &package,
            "service",
            &file_name,
            &mut self.definitions,
            stripped,
        )?;
        file.extension = merge_entries(
            std::mem::take(&mut file.extension),
            &package,
            "extension",
            &file_name,
            &mut self.definitions,
            stripped,
        )?;
        Ok(())
    }

    /// Whether every top-level definition of the file is already present with
    /// an identical shape.
    fn definitions_covered(&self, file: &prost_types::FileDescriptorProto) -> bool {
        let package = file.package();
        entries_covered(&file.message_type, package, "message", &self.definitions)
            && entries_covered(&file.enum_type, package, "enum", &self.definitions)
            && entries_covered(&file.service, package, "service", &self.definitions)
            && entries_covered(&file.extension, package, "extension", &self.definitions)
    }
}

/// Whether all entries already exist in the merged definitions with equal
/// shape.
fn entries_covered<T: prost::Message + NamedEntry>(
    entries: &[T],
    package: &str,
    kind: &'static str,
    definitions: &BTreeMap<String, (String, Vec<u8>)>,
) -> bool {
    entries.iter().all(|entry| {
        definitions
            .get(&qualified(package, entry.entry_name()))
            .is_some_and(|(_, existing)| existing == &identity(kind, entry))
    })
}

/// A named top-level descriptor entry (message, enum, service, extension),
/// delegating to the generated Protobuf accessor.
trait NamedEntry {
    fn entry_name(&self) -> &str;
}

impl NamedEntry for prost_types::DescriptorProto {
    fn entry_name(&self) -> &str {
        self.name()
    }
}

impl NamedEntry for prost_types::EnumDescriptorProto {
    fn entry_name(&self) -> &str {
        self.name()
    }
}

impl NamedEntry for prost_types::ServiceDescriptorProto {
    fn entry_name(&self) -> &str {
        self.name()
    }
}

impl NamedEntry for prost_types::FieldDescriptorProto {
    fn entry_name(&self) -> &str {
        self.name()
    }
}

fn qualified(package: &str, name: &str) -> String {
    format!("{package}.{name}")
}

/// Kind-tagged encoded identity of one definition; equal bytes mean equal
/// shape, and the tag keeps a message and an enum of one name distinct.
fn identity(kind: &str, entry: &impl prost::Message) -> Vec<u8> {
    let mut identity = kind.as_bytes().to_vec();
    identity.push(b':');
    identity.extend_from_slice(&entry.encode_to_vec());
    identity
}

fn merge_entries<T: prost::Message + NamedEntry>(
    entries: Vec<T>,
    package: &str,
    kind: &'static str,
    file_name: &str,
    definitions: &mut BTreeMap<String, (String, Vec<u8>)>,
    stripped: &mut BTreeMap<String, String>,
) -> Result<Vec<T>, Error> {
    let mut kept = Vec::with_capacity(entries.len());
    for entry in entries {
        let qualified = qualified(package, entry.entry_name());
        let identity = identity(kind, &entry);
        match definitions.get(&qualified) {
            Some((provider, existing)) if *existing == identity => {
                stripped.insert(qualified, provider.clone());
            }
            Some((provider, _)) => {
                return Err(Error::DescriptorConflict {
                    identity: qualified,
                    first: provider.clone(),
                    second: file_name.to_owned(),
                });
            }
            None => {
                definitions.insert(qualified, (file_name.to_owned(), identity));
                kept.push(entry);
            }
        }
    }
    Ok(kept)
}

/// Whether any top-level definition remains in the file.
fn definitions_remain(file: &prost_types::FileDescriptorProto) -> bool {
    !file.message_type.is_empty()
        || !file.enum_type.is_empty()
        || !file.service.is_empty()
        || !file.extension.is_empty()
}

/// Appends dependency edges so references to stripped duplicates resolve
/// within this file's closure.
fn add_provider_dependencies(
    file: &mut prost_types::FileDescriptorProto,
    stripped: &BTreeMap<String, String>,
) {
    let mut providers: Vec<String> = Vec::new();
    for reference in referenced_definitions(file) {
        if let Some(provider) = stripped.get(&reference)
            && !file.dependency.contains(provider)
            && !providers.contains(provider)
        {
            providers.push(provider.clone());
        }
    }
    file.dependency.extend(providers);
}

/// Fully-qualified type names referenced by fields, extensions, and service
/// methods (protoc emits them with a leading dot).
fn referenced_definitions(file: &prost_types::FileDescriptorProto) -> Vec<String> {
    fn walk_message(message: &prost_types::DescriptorProto, references: &mut Vec<String>) {
        for field in message.field.iter().chain(&message.extension) {
            for name in field.type_name.iter().chain(&field.extendee) {
                references.push(name.trim_start_matches('.').to_owned());
            }
        }
        for nested in &message.nested_type {
            walk_message(nested, references);
        }
    }
    let mut references = Vec::new();
    for message in &file.message_type {
        walk_message(message, &mut references);
    }
    for field in &file.extension {
        for name in field.type_name.iter().chain(&field.extendee) {
            references.push(name.trim_start_matches('.').to_owned());
        }
    }
    for service in &file.service {
        for method in &service.method {
            for name in method.input_type.iter().chain(&method.output_type) {
                references.push(name.trim_start_matches('.').to_owned());
            }
        }
    }
    references
}

/// Scratch directory sequence: the process id is shared by every concurrent
/// caller in one process (parallel tests, workspace builds) and the clock can
/// repeat a tick, so uniqueness is decided by a counter.
static SCRATCH_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Returns a fresh scratch directory unique across concurrent callers.
fn unique_scratch(prefix: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        SCRATCH_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    ))
}

/// Compiles one descriptor set for schema resolution only (no code emitted).
fn compile_descriptor_set(
    files: &[PathBuf],
    include_roots: &[&Path],
    descriptor_name: &str,
) -> Result<prost_types::FileDescriptorSet, Error> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let scratch = unique_scratch("phoxal-union");
    fs::create_dir_all(&scratch).map_err(|source| Error::Path {
        path: scratch.clone(),
        source,
    })?;
    let result = (|| -> Result<prost_types::FileDescriptorSet, Error> {
        let descriptor_path = scratch.join(descriptor_name);
        let mut command = std::process::Command::new(&protoc);
        command.arg("--include_imports").arg(format!(
            "--descriptor_set_out={}",
            descriptor_path.display()
        ));
        for root in include_roots {
            command.arg(format!("--proto_path={}", root.display()));
        }
        command.arg(format!("--proto_path={}", crate::include_dir().display()));
        command.arg(format!(
            "--proto_path={}",
            protoc_bin_vendored::include_path()?.display()
        ));
        command.args(files);
        let output = command.output().map_err(|source| Error::Path {
            path: scratch.clone(),
            source,
        })?;
        let bytes = if output.status.success() {
            fs::read(&descriptor_path).ok()
        } else {
            None
        };
        if !output.status.success() {
            return Err(Error::ProtocFailed(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let Some(bytes) = bytes.filter(|bytes| !bytes.is_empty()) else {
            return Ok(prost_types::FileDescriptorSet::default());
        };
        prost::Message::decode(bytes.as_slice()).map_err(|error| {
            Error::ProtocFailed(format!("union descriptor set is malformed: {error}"))
        })
    })();
    fs::remove_dir_all(&scratch).map_err(|source| Error::Path {
        path: scratch.clone(),
        source,
    })?;
    result
}

fn discover(directory: &Path, files: &mut Vec<PathBuf>, track_changes: bool) -> Result<(), Error> {
    if !directory.exists() {
        return Ok(());
    }
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

/// One robot-facing endpoint of a deployable service.
enum BindingEndpoint {
    Observation {
        function: String,
        constant: String,
        response_path: String,
    },
    Call {
        function: String,
        constant: String,
        request_path: String,
        response_path: String,
        leased: bool,
    },
}

/// The normalized robot-facing shape of one resolved service document.
struct ServiceBinding {
    /// Crate path of the module holding this service's descriptor constants.
    method_path: String,
    message_types: BTreeMap<String, BTreeSet<String>>,
    endpoints: Vec<BindingEndpoint>,
}

fn service_binding(unit: &CompiledUnit, index: usize) -> Option<ServiceBinding> {
    let resolved = unit.manifest.as_ref()?;
    let mut message_types: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for message in resolved
        .outputs
        .iter()
        .map(|output| &output.message)
        .chain(
            resolved
                .inputs
                .iter()
                .filter(|input| input.lease_valid_for_ms.is_some())
                .map(|input| &input.message),
        )
        .chain(resolved.operations.iter().map(|op| &op.response))
        .chain(resolved.operations.iter().map(|op| &op.request))
    {
        let short = message
            .fqn
            .rsplit('.')
            .next()
            .unwrap_or(&message.fqn)
            .to_owned();
        if message.fqn != "google.protobuf.Empty" {
            message_types
                .entry(short)
                .or_default()
                .insert(message.rust_path.clone());
        }
    }
    let mut endpoints = Vec::new();
    for output in &resolved.outputs {
        endpoints.push(BindingEndpoint::Observation {
            function: output.name.clone(),
            constant: output.name.to_shouty_snake_case(),
            response_path: output.message.rust_path.clone(),
        });
    }
    for input in &resolved.inputs {
        if input.lease_valid_for_ms.is_some() {
            endpoints.push(BindingEndpoint::Call {
                function: input.name.clone(),
                constant: input.name.to_shouty_snake_case(),
                request_path: input.message.rust_path.clone(),
                response_path: "::phoxal::contract::Empty".to_owned(),
                leased: true,
            });
        }
    }
    for operation in &resolved.operations {
        endpoints.push(BindingEndpoint::Call {
            function: operation.name.clone(),
            constant: operation.name.to_shouty_snake_case(),
            request_path: operation.request.rust_path.clone(),
            response_path: operation.response.rust_path.clone(),
            leased: false,
        });
    }
    Some(ServiceBinding {
        method_path: format!("crate::api::service_methods::u{index}"),
        message_types,
        endpoints,
    })
}

fn emit_binding(
    output: &mut String,
    names: &mut BTreeSet<String>,
    instance: &str,
    binding: &ServiceBinding,
) -> Result<(), Error> {
    let module = instance.to_snake_case();
    if ["types", "calls", "projections", "service_methods"].contains(&module.as_str()) {
        return Err(input(
            Path::new("robot.yaml"),
            format!(
                "API instance `{instance}` collides with the fixed generated module `{module}`"
            ),
        ));
    }
    if !names.insert(module.clone()) {
        return Err(input(
            Path::new("robot.yaml"),
            format!("API instance `{instance}` collides with another generated module"),
        ));
    }
    output.push_str(&format!("pub mod {module} {{\n"));
    for paths in binding.message_types.values() {
        if let Some(path) = (paths.len() == 1).then(|| paths.iter().next()).flatten() {
            output.push_str(&format!("    pub use {path};\n"));
        }
    }
    for endpoint in &binding.endpoints {
        let method_path = &binding.method_path;
        match endpoint {
            BindingEndpoint::Observation {
                function,
                constant,
                response_path,
            } => {
                output.push_str(&format!(
                    "    /// The typed contract method behind [`{function}`].\n    pub const {constant}: ::phoxal::contract::ObservationMethod<{response_path}> = {method_path}::{constant};\n    #[must_use]\n    pub fn {function}() -> ::phoxal::contract::Observation<{response_path}> {{\n        {method_path}::{constant}.bind({instance:?})\n    }}\n"
                ));
            }
            BindingEndpoint::Call {
                function,
                constant,
                request_path,
                response_path,
                leased,
            } => {
                output.push_str(&format!(
                    "    /// The typed contract method behind [`{function}`].\n    pub const {constant}: ::phoxal::contract::CallMethod<{request_path}, {response_path}> = {method_path}::{constant};\n    #[must_use]\n    pub fn {function}(request: {request_path}) -> ::phoxal::contract::Call<{request_path}, {response_path}> {{\n        {method_path}::{constant}.bind({instance:?}, request)\n    }}\n"
                ));
                if *leased {
                    output.push_str(&format!(
                        "    #[must_use]\n    pub fn withdraw_{function}() -> ::phoxal::contract::Withdraw<{request_path}, {response_path}> {{\n        {method_path}::{constant}.withdraw({instance:?})\n    }}\n"
                    ));
                }
            }
        }
    }
    output.push_str("}\n");
    Ok(())
}

/// Generates one unit's endpoint descriptor constants.
///
/// Data endpoints, leased inputs, operations, and calls each receive one
/// inert descriptor constant so robot builds and provider builds share one
/// endpoint identity.  Data endpoints are keyed by their qualified message
/// identity; operations and calls are keyed by their declared contract.
fn emit_methods_module(resolved: &ResolvedService, index: usize, descriptor_len: usize) -> String {
    let mut output = String::from("// @generated by phoxal-build; do not edit.\n");
    output.push_str(&format!(
        "#[used]\n#[cfg_attr(target_os = \"macos\", unsafe(link_section = \"__DATA,__phoxal_desc\"))]\n#[cfg_attr(not(target_os = \"macos\"), unsafe(link_section = \".phoxal_desc\"))]\npub(crate) static DESCRIPTOR_SET: [u8; {}] = ::phoxal::contract::descriptor_frame::<{}>(include_bytes!(concat!(env!(\"OUT_DIR\"), \"/phoxal-api/u{index}/phoxal-descriptors.bin\")));\n",
        descriptor_len + 16,
        descriptor_len + 16,
    ));
    let descriptor = "&crate::api::service_methods::u{index}::DESCRIPTOR_SET";
    let descriptor = descriptor.replace("{index}", &index.to_string());
    for output_endpoint in &resolved.outputs {
        let name = &output_endpoint.name;
        let constant = name.to_shouty_snake_case();
        let message = &output_endpoint.message;
        let lease = output_endpoint
            .lease_valid_for_ms
            .map_or_else(|| "None".to_owned(), |value| format!("Some({value})"));
        output.push_str(&format!(
            "pub const {constant}: ::phoxal::contract::ObservationMethod<{}> = ::phoxal::contract::ObservationMethod::new({:?}, {:?}, {:?}, \"google.protobuf.Empty\", {:?}, {}, {}, {descriptor});\n",
            message.rust_path,
            message.fqn,
            name,
            name,
            message.fqn,
            output_endpoint.retained_latest,
            lease
        ));
    }
    for input in &resolved.inputs {
        let Some(valid_for_ms) = input.lease_valid_for_ms else {
            continue;
        };
        let name = &input.name;
        let constant = name.to_shouty_snake_case();
        output.push_str(&format!(
            "pub const {constant}: ::phoxal::contract::CallMethod<{}, ::phoxal::contract::Empty> = ::phoxal::contract::CallMethod::new({:?}, {:?}, {:?}, {:?}, \"google.protobuf.Empty\", Some({valid_for_ms}), {descriptor});\n",
            input.message.rust_path,
            input.message.fqn,
            name,
            name,
            input.message.fqn
        ));
    }
    for endpoint in resolved.operations.iter().chain(resolved.calls.iter()) {
        let name = &endpoint.name;
        let constant = name.to_shouty_snake_case();
        output.push_str(&format!(
            "pub const {constant}: ::phoxal::contract::CallMethod<{}, {}> = ::phoxal::contract::CallMethod::new({:?}, {:?}, {:?}, {:?}, {:?}, None, {descriptor});\n",
            endpoint.request.rust_path,
            endpoint.response.rust_path,
            endpoint.contract,
            name,
            name,
            endpoint.request.fqn,
            endpoint.response.fqn
        ));
    }
    output
}

/// Emits the author-facing module of composition-bound requirement handles
/// declared by the local service document.
fn emit_calls_module(output: &mut String, resolved: &ResolvedService) {
    if resolved.calls.is_empty() {
        return;
    }
    output.push_str(
        "/// Composition-bound requirement handles declared by this package's\n\
         /// service document.  Each call stays inert until the output\n\
         /// transaction accepts it; `robot.yaml` selects the provider\n\
         /// instance and the typed completion arrives in a later input cut.\n\
         pub mod calls {\n",
    );
    for call in &resolved.calls {
        let constant = call.name.to_shouty_snake_case();
        output.push_str(&format!(
            "    /// Stages one call on the composition-bound `{}` requirement.\n    #[must_use]\n    pub fn {name}(request: {request}) -> ::phoxal::contract::Call<{request}, {response}> {{\n        super::service_methods::u0::{constant}.bind(\"\", request)\n    }}\n",
            call.name,
            name = call.name,
            request = call.request.rust_path,
            response = call.response.rust_path,
        ));
    }
    output.push_str("}\n");
}

/// Emits the author-facing projection hooks for a service document that
/// declares projected outputs.
fn emit_projections_module(output: &mut String, resolved: &ResolvedService) {
    let projections: Vec<_> = resolved
        .outputs
        .iter()
        .filter(|endpoint| endpoint.projection)
        .collect();
    if projections.is_empty() {
        return;
    }
    output.push_str(
        "/// State-projection hooks owned by the service implementation.\n\
         /// The generator owns registration, bounds, and invocation timing;\n\
         /// each hook owns payload construction from private state.\n\
         pub mod projections {\n    pub trait Projections {\n        /// The runtime state these hooks project from.\n        type State;\n",
    );
    for endpoint in &projections {
        let returns = if endpoint.lease_valid_for_ms.is_some() {
            format!("::std::option::Option<{}>", endpoint.message.rust_path)
        } else {
            endpoint.message.rust_path.clone()
        };
        output.push_str(&format!(
            "        /// Projects the `{}` output.\n        fn {name}(&self, state: &Self::State) -> {returns};\n",
            endpoint.name,
            name = endpoint.name,
            returns = returns,
        ));
    }
    output.push_str("    }\n}\n");
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
    fn equivalent_shared_messages_merge_across_manifest_services()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        for (name, extra) in [
            ("alpha", ""),
            ("beta", "message Additional { string value = 1; }"),
        ] {
            let package = directory.path().join(name);
            fs::create_dir_all(package.join("api"))?;
            fs::write(
                package.join("api/shared.proto"),
                format!(
                    "syntax = \"proto3\"; package proof.shared.v1; message Shared {{ string value = 1; }} {extra}"
                ),
            )?;
            fs::write(
                package.join("api/service.proto"),
                format!(
                    "syntax = \"proto3\"; package proof.{name}.v1; import \"shared.proto\"; message Reading {{ proof.shared.v1.Shared shared = 1; }}"
                ),
            )?;
            fs::write(
                package.join("service.yaml"),
                "schema: phoxal/service/v0\noutputs:\n  reading:\n    type: proof.{name}.v1.Reading\n    delivery: latest\n    retained_latest: true\n    max_bytes: 1024\n"
                    .replace("{name}", name),
            )?;
        }
        let robot = directory.path().join("robot");
        let out = directory.path().join("out");
        fs::create_dir_all(&robot)?;
        fs::create_dir_all(&out)?;
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
        let message = error.to_string();
        assert!(
            message.contains("proof.shared.v1.Shared"),
            "the rejection names the conflicting symbol, got: {message}"
        );
        Ok(())
    }

    /// Writes one manifest-authored service whose output references its own
    /// qualified `Reading` message from the given relative proto file.
    fn manifest_service(
        directory: &Path,
        name: &str,
        file_name: &str,
        proto: &str,
    ) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
        let package = directory.join(name);
        fs::create_dir_all(package.join("api"))?;
        fs::write(package.join("api").join(file_name), proto)?;
        fs::write(
            package.join("service.yaml"),
            format!(
                "schema: phoxal/service/v0\noutputs:\n  reading:\n    type: proof.{name}.v1.Reading\n    delivery: latest\n    retained_latest: true\n    max_bytes: 1024\n"
            ),
        )?;
        Ok(package)
    }

    fn compose(directory: &Path, services: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
        let robot = directory.join("robot");
        let out = directory.join("out");
        fs::create_dir_all(&robot)?;
        fs::create_dir_all(&out)?;
        let selections = services
            .iter()
            .map(|name| format!("  {name}:\n    source: {{ path: ../{name} }}\n"))
            .collect::<String>();
        fs::write(
            robot.join("robot.yaml"),
            format!("schema: phoxal/robot/v0\nrobot: {{ id: rover }}\nservices:\n{selections}"),
        )?;
        generate(&robot, &out, None)?;
        Ok(())
    }

    #[test]
    fn compatible_shared_definitions_merge_across_manifest_services()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        // alpha owns the shared vocabulary; beta carries an identical copy
        // under a different relative filename and references it.
        manifest_service(
            directory.path(),
            "alpha",
            "messages.proto",
            "syntax = \"proto3\"; package proof.alpha.v1; import \"common.proto\"; message Reading { double value = 1; proof.shared.v1.Common common = 2; }\n",
        )?;
        fs::write(
            directory.path().join("alpha/api/common.proto"),
            "syntax = \"proto3\"; package proof.shared.v1; message Common { string value = 1; }\n",
        )?;
        manifest_service(
            directory.path(),
            "beta",
            "messages.proto",
            "syntax = \"proto3\"; package proof.beta.v1; import \"shared.proto\"; message Reading { double value = 1; proof.shared.v1.Common common = 2; }\n",
        )?;
        fs::write(
            directory.path().join("beta/api/shared.proto"),
            "syntax = \"proto3\"; package proof.shared.v1; message Common { string value = 1; }\n",
        )?;
        // gamma carries the shared copy under alpha's relative filename: the
        // whole-file duplicate collapses onto the kept provider.
        manifest_service(
            directory.path(),
            "gamma",
            "messages.proto",
            "syntax = \"proto3\"; package proof.gamma.v1; import \"common.proto\"; message Reading { double value = 1; proof.shared.v1.Common common = 2; }\n",
        )?;
        fs::write(
            directory.path().join("gamma/api/common.proto"),
            "syntax = \"proto3\"; package proof.shared.v1; message Common { string value = 1; }\n",
        )?;
        compose(directory.path(), &["alpha", "beta", "gamma"])?;
        let api = fs::read_to_string(directory.path().join("out/phoxal_api.rs"))?;
        assert_eq!(
            api.matches("proof.shared.v1.rs").count(),
            1,
            "identical shared definitions collapse to one included module: {api}"
        );
        for instance in ["alpha", "beta", "gamma"] {
            assert!(
                api.contains(&format!("pub mod {instance}")),
                "every service binds through the merged pool"
            );
        }
        Ok(())
    }

    #[test]
    fn conflicting_shared_definitions_are_rejected_explicitly()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        manifest_service(
            directory.path(),
            "alpha",
            "messages.proto",
            "syntax = \"proto3\"; package proof.alpha.v1; import \"common.proto\"; message Reading { double value = 1; proof.shared.v1.Common common = 2; }\n",
        )?;
        fs::write(
            directory.path().join("alpha/api/common.proto"),
            "syntax = \"proto3\"; package proof.shared.v1; message Common { string value = 1; }\n",
        )?;
        manifest_service(
            directory.path(),
            "beta",
            "messages.proto",
            "syntax = \"proto3\"; package proof.beta.v1; import \"shared.proto\"; message Reading { double value = 1; proof.shared.v1.Common common = 2; }\n",
        )?;
        fs::write(
            directory.path().join("beta/api/shared.proto"),
            "syntax = \"proto3\"; package proof.shared.v1; message Common { int64 value = 1; }\n",
        )?;
        let error = compose(directory.path(), &["alpha", "beta"])
            .expect_err("conflicting shared definition must fail");
        assert!(
            error.to_string().contains("proof.shared.v1.Common"),
            "the rejection names the conflicting symbol, got: {error}"
        );
        Ok(())
    }
}

#[cfg(test)]
mod manifest_tests {
    use super::*;

    const CONSUMER_PROTO: &str = "syntax = \"proto3\"; package example.contract_evaluation.v1; message ConsumerStatus { string phase = 1; }\n";
    const CONSUMER_MANIFEST: &str = "schema: phoxal/service/v0\ninputs:\n  encoder:\n    type: phoxal.robotics.v1.EncoderSample\n    delivery: latest\n    required: true\n    max_age_ms: 100\n    max_bytes: 1024\noutputs:\n  status:\n    type: example.contract_evaluation.v1.ConsumerStatus\n    delivery: latest\n    retained_latest: true\n    max_bytes: 4096\noperations:\n  inspect:\n    contract: example.contract_evaluation.v1.InspectConsumer\n    request: google.protobuf.Empty\n    response: example.contract_evaluation.v1.ConsumerStatus\n    max_items: 8\n    max_bytes: 4096\ncalls:\n  read_encoder:\n    contract: example.contract_evaluation.v1.ReadEncoder\n    request: google.protobuf.Empty\n    response: phoxal.robotics.v1.EncoderSample\n    required: true\n    max_items: 8\n    max_bytes: 1024\n";

    fn manifest_package(
        directory: &std::path::Path,
    ) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
        let package = directory.join("consumer");
        fs::create_dir_all(package.join("api/example/contract_evaluation/v1"))?;
        fs::write(
            package.join("api/example/contract_evaluation/v1/messages.proto"),
            CONSUMER_PROTO,
        )?;
        fs::write(package.join("service.yaml"), CONSUMER_MANIFEST)?;
        Ok(package)
    }

    fn generate_package(package: &Path) -> Result<(String, String), Box<dyn std::error::Error>> {
        let out = package.parent().unwrap().join("out");
        fs::create_dir_all(&out)?;
        generate(package, &out, None)?;
        Ok((
            fs::read_to_string(out.join("phoxal_api.rs"))?,
            fs::read_to_string(out.join("phoxal-provider.rs"))?,
        ))
    }

    #[test]
    fn manifest_generates_provider_and_instance_bindings() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let package = manifest_package(directory.path())?;
        let (api, provider) = generate_package(&package)?;
        assert!(
            provider.contains("pub struct Inputs"),
            "provider module declares the generated input transaction"
        );
        assert!(provider.contains("Latest<::phoxal::robotics::EncoderSample>"));
        assert!(provider.contains("commands_port()"));
        assert!(provider.contains("Completions"));
        assert!(
            api.contains("pub mod calls"),
            "requirement handles live in the documented calls module"
        );
        assert!(api.contains("pub fn read_encoder(request"));
        assert!(api.contains("bind(\"\", request)"));
        assert!(
            !api.contains("pub mod consumer"),
            "a standalone manifest service has no composition instance of its own"
        );
        assert!(!api.contains("withdraw_"), "no leased endpoints exist");
        assert!(
            api.contains("example.contract_evaluation.v1.InspectConsumer"),
            "operation identity uses the declared contract"
        );
        assert!(
            api.contains("__phoxal_desc"),
            "descriptor frames are embedded"
        );
        Ok(())
    }

    #[test]
    fn endpoint_rename_changes_generated_surface() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let package = manifest_package(directory.path())?;
        let (api, _provider) = generate_package(&package)?;
        let edited = CONSUMER_MANIFEST.replace("read_encoder:", "sample_encoder:");
        fs::write(package.join("service.yaml"), edited)?;
        let (api_renamed, provider_renamed) = generate_package(&package)?;
        assert!(api.contains("pub fn read_encoder"));
        assert!(api_renamed.contains("pub fn sample_encoder"));
        assert!(!api_renamed.contains("read_encoder"));
        assert!(!provider_renamed.contains("read_encoder"));
        Ok(())
    }

    #[test]
    fn manifest_removal_stops_provider_generation() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let package = manifest_package(directory.path())?;
        let (_, provider) = generate_package(&package)?;
        assert!(provider.contains("Inputs"));
        fs::remove_file(package.join("service.yaml"))?;
        let out = directory.path().join("out");
        generate(&package, &out, None)?;
        assert!(
            !out.join("phoxal-provider.rs").exists(),
            "removing the manifest must remove provider attachment"
        );
        assert!(!fs::read_to_string(out.join("phoxal_api.rs"))?.contains("pub mod calls"));
        Ok(())
    }

    #[test]
    fn conflicting_protobuf_service_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let package = manifest_package(directory.path())?;
        fs::write(
            package.join("api/example/contract_evaluation/v1/service.proto"),
            "syntax = \"proto3\"; package example.contract_evaluation.v1; import \"google/protobuf/empty.proto\"; message PokeRequest { } service Extra { rpc Poke(PokeRequest) returns (google.protobuf.Empty); }\n",
        )?;
        let out = directory.path().join("out");
        fs::create_dir_all(&out)?;
        let error = match generate(&package, &out, None) {
            Err(error) => error.to_string(),
            Ok(()) => panic!("two endpoint authorities must be rejected"),
        };
        assert!(
            error.contains("Protobuf files carry message definitions only"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[test]
    fn robot_document_schema_must_be_a_supported_generation()
    -> Result<(), Box<dyn std::error::Error>> {
        // The schema line is a consumed value: it selects the document
        // generation, so an unsupported one is rejected instead of read as
        // the wrong generation — on the generation path just as the SDK
        // robot DTO rejects it.
        let directory = tempfile::tempdir()?;
        let package = directory.path().join("robot");
        fs::create_dir_all(&package)?;
        fs::write(
            package.join("robot.yaml"),
            "schema: phoxal/robot/v9\nrobot: { id: rover }\n",
        )?;
        let out = directory.path().join("out");
        fs::create_dir_all(&out)?;
        let error = match generate(&package, &out, None) {
            Err(error) => error.to_string(),
            Ok(()) => panic!("unsupported robot schema must be rejected"),
        };
        assert!(
            error.contains("robot.yaml declares schema"),
            "unsupported robot schema produced {error}"
        );
        Ok(())
    }

    #[test]
    fn empty_brain_generates_an_empty_contract() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let package = directory.path().join("robot");
        fs::create_dir_all(&package)?;
        fs::write(
            package.join("robot.yaml"),
            "schema: phoxal/robot/v0\nrobot: { id: rover }\n",
        )?;
        let out = directory.path().join("out");
        fs::create_dir_all(&out)?;
        generate(&package, &out, None)?;
        let provider = fs::read_to_string(out.join("phoxal-provider.rs"))?;
        assert!(
            provider.contains("Inputs"),
            "empty brain keeps its provider"
        );
        let api = fs::read_to_string(out.join("phoxal_api.rs"))?;
        assert!(
            !api.contains("service_methods"),
            "an empty brain declares no endpoint constants"
        );
        Ok(())
    }

    #[test]
    fn malformed_documents_fail_with_exact_diagnostics() -> Result<(), Box<dyn std::error::Error>> {
        let cases: &[(&str, &str, &str)] = &[
            (
                "unknown-field",
                "schema: phoxal/service/v0\noutputs:\n  status:\n    type: a.b.S\n    delivery: latest\n    retained_latest: true\n    max_bytes: 10\n    freshness_ms: 5\n",
                "unknown field",
            ),
            (
                "wrong-schema",
                "schema: phoxal/service/v9\n",
                "supports \"phoxal/service/v0\"",
            ),
            (
                "duplicate-endpoint",
                "schema: phoxal/service/v0\ninputs:\n  encoder:\n    type: phoxal.robotics.v1.EncoderSample\n    max_bytes: 10\n  encoder:\n    type: phoxal.robotics.v1.EncoderSample\n    max_bytes: 10\n",
                "more than once in one mapping",
            ),
            (
                "missing-bound",
                "schema: phoxal/service/v0\ninputs:\n  encoder:\n    type: phoxal.robotics.v1.EncoderSample\n    max_bytes: 0\n",
                "max_bytes > 0",
            ),
            (
                "unresolved-type",
                "schema: phoxal/service/v0\noutputs:\n  status:\n    type: no.such.Message\n    delivery: latest\n    retained_latest: true\n    max_bytes: 10\n",
                "no message definition resolves",
            ),
            (
                "non-retained-latest",
                "schema: phoxal/service/v0\noutputs:\n  status:\n    type: phoxal.robotics.v1.EncoderSample\n    delivery: latest\n    retained_latest: false\n    max_bytes: 10\n",
                "retained by definition",
            ),
            (
                "reserved-name",
                "schema: phoxal/service/v0\ninputs:\n  methods:\n    type: phoxal.robotics.v1.EncoderSample\n    max_bytes: 10\n",
                "reserved by generated provider glue",
            ),
            (
                "queue-lease-conflict",
                "schema: phoxal/service/v0\ninputs:\n  events:\n    type: phoxal.robotics.v1.EncoderSample\n    delivery: queue\n    max_items: 4\n    max_bytes: 10\n    lease: { valid_for_ms: 100 }\n",
                "queued delivery cannot carry a lease",
            ),
            (
                "optional-input",
                "schema: phoxal/service/v0\ninputs:\n  encoder:\n    type: phoxal.robotics.v1.EncoderSample\n    required: false\n    max_bytes: 10\n",
                "optional inputs are not supported",
            ),
            (
                "optional-call",
                "schema: phoxal/service/v0\ncalls:\n  read:\n    contract: a.b.Read\n    request: google.protobuf.Empty\n    response: phoxal.robotics.v1.EncoderSample\n    required: false\n    max_bytes: 10\n",
                "optional requirements are not supported",
            ),
        ];
        for (name, document, expected) in cases {
            let directory = tempfile::tempdir()?;
            let package = directory.path().join("consumer");
            fs::create_dir_all(package.join("api"))?;
            fs::write(package.join("service.yaml"), document)?;
            let out = directory.path().join("out");
            fs::create_dir_all(&out)?;
            let error = match generate(&package, &out, None) {
                Err(error) => error.to_string(),
                Ok(()) => panic!("{name} must be rejected"),
            };
            assert!(error.contains(expected), "{name} produced {error}");
        }
        Ok(())
    }
}
