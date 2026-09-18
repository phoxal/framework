//! Build-time generation for service-owned Phoxal Protobuf contracts.
//!
//! [`compile_protos`] supplies the shared `phoxal/port.proto` import and a
//! pinned Protobuf compiler, emits Prost messages with type names, retains the
//! original descriptor closure, and generates inert typed port references.
//!
//! Two flags tune what a build script needs:
//!
//! * **dependency descriptors** — [`compile_protos_with_dependencies`] (and
//!   [`compile_protos_with_dependencies_and_output`]) additionally import
//!   descriptor sets from build dependencies and map their packages to
//!   canonical Rust contract crates, so a service can consume one shared wire
//!   vocabulary without regenerating or locating dependency-owned source
//!   files.
//! * **descriptor output filename** — [`compile_protos_with_output`] (and
//!   [`compile_protos_with_dependencies_and_output`]) name the file the
//!   retained descriptor closure lands in. A build script that performs more
//!   than one compilation in the same `OUT_DIR` must give each invocation a
//!   distinct filename; the two simple forms write [`DESCRIPTOR_FILE`] and
//!   are the right call for a single compilation.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use heck::{ToShoutySnakeCase, ToSnakeCase};
use prost_reflect::{DescriptorPool, Value};

/// The shared option definition packaged with this crate.
pub const PORT_PROTO: &str = include_str!("../proto/phoxal/port.proto");

const DESCRIPTOR_FILE: &str = "phoxal-descriptors.bin";
const DEPENDENCY_DESCRIPTOR_FILE: &str = "phoxal-dependency-descriptors.bin";
const PORT_KIND_EXTENSION: &str = "phoxal.port.kind";
const MAX_DESCRIPTOR_BYTES: usize = 8 * 1024 * 1024;
const MAX_DESCRIPTOR_FILES: usize = 1_024;

/// Returns the packaged Protobuf include root containing `phoxal/port.proto`.
#[must_use]
pub fn include_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("proto")
}

/// Returns the descriptor-set path emitted into the current Cargo `OUT_DIR`.
///
/// Contract libraries can embed these exact bytes with `include_bytes!`.
pub fn descriptor_set_path() -> Result<PathBuf, Error> {
    std::env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .map(|out_dir| out_dir.join(DESCRIPTOR_FILE))
        .ok_or(Error::MissingEnvironment("OUT_DIR"))
}

/// Errors reported while compiling or validating a service-owned contract.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A required Cargo build-script environment value is unavailable.
    #[error("Cargo build environment is missing {0}")]
    MissingEnvironment(&'static str),
    /// A source or include path cannot be resolved.
    #[error("cannot resolve Protobuf path {path}: {source}")]
    Path {
        /// The failing path.
        path: PathBuf,
        /// The filesystem error.
        source: std::io::Error,
    },
    /// An owned source does not belong to any declared include root.
    #[error("owned Protobuf source {0} is outside every declared include root")]
    SourceOutsideIncludes(PathBuf),
    /// The pinned Protobuf compiler cannot be located.
    #[error("cannot locate the packaged Protobuf compiler: {0}")]
    Protoc(#[from] protoc_bin_vendored::Error),
    /// The Protobuf compiler rejected the contract closure.
    #[error("protoc failed: {0}")]
    ProtocFailed(String),
    /// The retained descriptor closure cannot be read.
    #[error("cannot read generated descriptor set {path}: {source}")]
    ReadDescriptor {
        /// Descriptor-set path.
        path: PathBuf,
        /// The filesystem error.
        source: std::io::Error,
    },
    /// The retained descriptors are invalid.
    #[error("invalid Protobuf descriptor closure: {0}")]
    Descriptor(#[from] prost_reflect::DescriptorError),
    /// Two descriptor dependencies provide different definitions for one file
    /// path or fully-qualified symbol.
    #[error("conflicting Protobuf descriptor {identity} from dependencies {first} and {second}")]
    DescriptorConflict {
        /// Conflicting descriptor path or fully-qualified symbol.
        identity: String,
        /// First dependency declaring the identity.
        first: String,
        /// Second dependency declaring the identity.
        second: String,
    },
    /// The retained descriptor closure exceeded one of the build-time bounds.
    #[error("Protobuf descriptor closure exceeds {what} bound of {limit} (actual {actual})")]
    DescriptorBounds {
        /// Bounded descriptor quantity.
        what: &'static str,
        /// Maximum accepted value.
        limit: usize,
        /// Observed value.
        actual: usize,
    },
    /// The packaged method option is absent from the compiled descriptor closure.
    #[error("compiled descriptor closure is missing {PORT_KIND_EXTENSION}")]
    MissingPortKindExtension,
    /// A service method omits its mandatory kind.
    #[error("owned method {0} must declare option (phoxal.port.kind)")]
    MissingKind(String),
    /// A service method uses the unspecified or an unknown kind value.
    #[error("owned method {method} declares invalid phoxal.port.kind value {value}")]
    InvalidKind {
        /// Fully-qualified Protobuf method.
        method: String,
        /// Unknown enum number.
        value: i32,
    },
    /// A method shape does not match its declared kind.
    #[error("owned method {method} declared {kind} but {reason}")]
    InvalidShape {
        /// Fully-qualified Protobuf method.
        method: String,
        /// Declared port kind.
        kind: &'static str,
        /// Shape mismatch.
        reason: &'static str,
    },
    /// Two generated identifiers collide after normalization.
    #[error("generated {what} name {name} collides in {owner}")]
    NameCollision {
        /// The generated namespace.
        what: &'static str,
        /// Colliding normalized name.
        name: String,
        /// Owning Protobuf package or service.
        owner: String,
    },
    /// Prost generation failed after descriptor validation.
    #[error("cannot generate Rust contract bindings: {0}")]
    Prost(#[from] std::io::Error),
}

/// Compiles owned Protobuf files and their imported descriptor closure.
///
/// The explicitly listed files are the contract owner's files.
/// Imported services remain dependency descriptors and do not generate Phoxal
/// ports in this build. The retained descriptor closure lands in
/// `OUT_DIR/<DESCRIPTOR_FILE>`; a build script that compiles more than one
/// owner in the same `OUT_DIR` must use [`compile_protos_with_output`] (or
/// [`compile_protos_with_dependencies_and_output`]) to name a distinct
/// descriptor file per compilation.
pub fn compile_protos(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
) -> Result<(), Error> {
    compile_protos_with_dependencies(protos, includes, &[], &[])
}

/// Compiles owned Protobuf files into a named descriptor output file.
///
/// Equivalent to [`compile_protos`] but writes the retained descriptor closure
/// to `OUT_DIR/<descriptor_file>` instead of [`DESCRIPTOR_FILE`]. A build
/// script that needs multiple descriptor closures in the same `OUT_DIR` —
/// for example one for the framework-owned protocol protos and a separate one
/// for a domain vocabulary — gives each compilation a distinct filename.
pub fn compile_protos_with_output(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    descriptor_file: &str,
) -> Result<(), Error> {
    compile_to_with_dependencies(
        protos,
        includes,
        &descriptor_out_dir()?,
        &[],
        &[],
        descriptor_file,
    )
}

/// One descriptor closure supplied by a direct Cargo build dependency.
#[derive(Clone, Copy, Debug)]
pub struct DependencyDescriptor<'a> {
    /// Human-readable Cargo package name used in conflict diagnostics.
    pub package: &'a str,
    /// The dependency owner's original encoded `FileDescriptorSet`.
    pub descriptors: &'a [u8],
}

impl<'a> DependencyDescriptor<'a> {
    /// Creates one exact dependency descriptor input.
    #[must_use]
    pub const fn new(package: &'a str, descriptors: &'a [u8]) -> Self {
        Self {
            package,
            descriptors,
        }
    }
}

/// Compiles owned Protobuf files using descriptor closures exported by direct
/// build dependencies.
///
/// Each tuple contains the fully-qualified Protobuf package (including its
/// leading dot) and the Rust path Prost should use for that package. The
/// imported descriptors remain in the owner's descriptor closure, while their
/// messages are referenced rather than regenerated. Dependency source trees
/// are deliberately not accepted as include roots. The retained descriptor
/// closure lands in `OUT_DIR/<DESCRIPTOR_FILE>`; the descriptor-filename
/// variant [`compile_protos_with_dependencies_and_output`] is the right call
/// when a build script needs both dependency imports and a non-default
/// descriptor filename.
pub fn compile_protos_with_dependencies(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    dependencies: &[DependencyDescriptor<'_>],
    extern_paths: &[(&str, &str)],
) -> Result<(), Error> {
    compile_protos_with_dependencies_and_output(
        protos,
        includes,
        dependencies,
        extern_paths,
        DESCRIPTOR_FILE,
    )
}

/// Compiles owned Protobuf files using dependency descriptors and writes the
/// retained closure to a named descriptor output file.
///
/// Combines [`compile_protos_with_dependencies`] with [`compile_protos_with_output`]:
/// a build script that imports descriptor closures from build dependencies
/// *and* must keep more than one closure distinct in the same `OUT_DIR` —
/// for example the framework compilation next to a domain-vocabulary
/// compilation — gives each invocation a distinct filename.
pub fn compile_protos_with_dependencies_and_output(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    dependencies: &[DependencyDescriptor<'_>],
    extern_paths: &[(&str, &str)],
    descriptor_file: &str,
) -> Result<(), Error> {
    let out_dir = descriptor_out_dir()?;
    compile_to_with_dependencies(protos, includes, &out_dir, dependencies, extern_paths, descriptor_file)
}

fn descriptor_out_dir() -> Result<PathBuf, Error> {
    std::env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .ok_or(Error::MissingEnvironment("OUT_DIR"))
}

#[cfg(test)]
fn compile_to(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    out_dir: &Path,
    descriptor_file: &str,
) -> Result<(), Error> {
    compile_to_with_dependencies(protos, includes, out_dir, &[], &[], descriptor_file)
}

fn compile_to_with_dependencies(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    out_dir: &Path,
    dependencies: &[DependencyDescriptor<'_>],
    extern_paths: &[(&str, &str)],
    descriptor_file: &str,
) -> Result<(), Error> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let google_include = protoc_bin_vendored::include_path()?;

    let mut include_roots = Vec::with_capacity(includes.len() + 2);
    include_roots.extend(
        includes
            .iter()
            .map(|path| canonical(path.as_ref()))
            .collect::<Result<Vec<_>, _>>()?,
    );
    include_roots.push(canonical(&include_dir())?);
    include_roots.push(canonical(&google_include)?);

    let owned_paths = protos
        .iter()
        .map(|path| canonical(path.as_ref()))
        .collect::<Result<Vec<_>, _>>()?;
    let owned_names = owned_file_names(&owned_paths, &include_roots)?;
    let descriptor_path = out_dir.join(descriptor_file);
    let dependency_descriptor_path = if dependencies.is_empty() {
        None
    } else {
        let path = out_dir.join(DEPENDENCY_DESCRIPTOR_FILE);
        let bytes = merge_dependency_descriptors(dependencies)?;
        std::fs::write(&path, bytes).map_err(|source| Error::Path {
            path: path.clone(),
            source,
        })?;
        Some(path)
    };

    let mut command = Command::new(&protoc);
    command
        .arg("--include_imports")
        .arg("--include_source_info")
        .arg(format!(
            "--descriptor_set_out={}",
            descriptor_path.display()
        ));
    if let Some(path) = &dependency_descriptor_path {
        command.arg(format!("--descriptor_set_in={}", path.display()));
    }
    for include in &include_roots {
        command.arg(format!("--proto_path={}", include.display()));
    }
    command.args(&owned_paths);

    let output = command.output().map_err(|source| Error::Path {
        path: protoc.clone(),
        source,
    })?;
    if !output.status.success() {
        return Err(Error::ProtocFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }

    let descriptor_bytes =
        std::fs::read(&descriptor_path).map_err(|source| Error::ReadDescriptor {
            path: descriptor_path.clone(),
            source,
        })?;
    if descriptor_bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err(Error::DescriptorBounds {
            what: "encoded bytes",
            limit: MAX_DESCRIPTOR_BYTES,
            actual: descriptor_bytes.len(),
        });
    }
    let pool = DescriptorPool::decode(descriptor_bytes.as_slice())?;
    let file_count = pool.files().count();
    if file_count > MAX_DESCRIPTOR_FILES {
        return Err(Error::DescriptorBounds {
            what: "file count",
            limit: MAX_DESCRIPTOR_FILES,
            actual: file_count,
        });
    }
    let ports = validate_owned_ports(&pool, &owned_names)?;

    let has_ports = !ports.is_empty();
    let service_package = ports.keys().next().and_then(|name| {
        name.rsplit_once('.')
            .and_then(|(prefix, _)| prefix.rsplit_once('.'))
            .map(|(package, _)| package.to_owned())
    });
    let mut config = prost_build::Config::new();
    config
        .out_dir(out_dir)
        .protoc_executable(protoc)
        .file_descriptor_set_path(&descriptor_path)
        .skip_protoc_run()
        .enable_type_names()
        .service_generator(Box::new(PortGenerator { ports }));
    for (proto_package, rust_path) in extern_paths {
        config.extern_path(*proto_package, *rust_path);
    }
    config.compile_protos(&owned_paths, &include_roots)?;

    if has_ports {
        embed_descriptor_section(
            out_dir,
            service_package.as_deref().unwrap_or_default(),
            descriptor_bytes.len(),
            descriptor_file,
        )?;
    }

    for path in owned_paths {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!(
        "cargo:rerun-if-changed={}",
        include_dir().join("phoxal/port.proto").display()
    );
    Ok(())
}

fn merge_dependency_descriptors(
    dependencies: &[DependencyDescriptor<'_>],
) -> Result<Vec<u8>, Error> {
    let mut known_files = HashMap::<String, (String, Vec<u8>)>::new();
    let mut known_symbols = HashMap::<String, (String, String)>::new();
    let mut pools = Vec::with_capacity(dependencies.len());

    for dependency in dependencies {
        if dependency.descriptors.len() > MAX_DESCRIPTOR_BYTES {
            return Err(Error::DescriptorBounds {
                what: "dependency encoded bytes",
                limit: MAX_DESCRIPTOR_BYTES,
                actual: dependency.descriptors.len(),
            });
        }
        let pool = DescriptorPool::decode(dependency.descriptors)?;
        let file_count = pool.files().count();
        if file_count > MAX_DESCRIPTOR_FILES {
            return Err(Error::DescriptorBounds {
                what: "dependency file count",
                limit: MAX_DESCRIPTOR_FILES,
                actual: file_count,
            });
        }

        for file in pool.files() {
            let name = file.name().to_owned();
            let encoded = file.encode_to_vec();
            if let Some((first, first_encoded)) = known_files.get(&name) {
                if first_encoded != &encoded {
                    return Err(Error::DescriptorConflict {
                        identity: name,
                        first: first.clone(),
                        second: dependency.package.to_owned(),
                    });
                }
                continue;
            }

            for identity in descriptor_symbols(&pool, &name) {
                if let Some((first, first_file)) = known_symbols.get(&identity)
                    && first_file != &name
                {
                    return Err(Error::DescriptorConflict {
                        identity,
                        first: first.clone(),
                        second: dependency.package.to_owned(),
                    });
                }
                known_symbols.insert(identity, (dependency.package.to_owned(), name.clone()));
            }
            known_files.insert(name, (dependency.package.to_owned(), encoded));
        }
        pools.push(pool);
    }

    let mut merged = DescriptorPool::new();
    for (dependency, pool) in dependencies.iter().zip(pools) {
        merged
            .decode_file_descriptor_set(pool.encode_to_vec().as_slice())
            .map_err(|_| Error::DescriptorConflict {
                identity: "descriptor closure".to_owned(),
                first: "previous dependencies".to_owned(),
                second: dependency.package.to_owned(),
            })?;
    }
    Ok(merged.encode_to_vec())
}

fn descriptor_symbols(pool: &DescriptorPool, file_name: &str) -> Vec<String> {
    let messages = pool
        .all_messages()
        .filter(|descriptor| descriptor.parent_file().name() == file_name)
        .map(|descriptor| descriptor.full_name().to_owned());
    let enums = pool
        .all_enums()
        .filter(|descriptor| descriptor.parent_file().name() == file_name)
        .map(|descriptor| descriptor.full_name().to_owned());
    let services = pool
        .services()
        .filter(|descriptor| descriptor.parent_file().name() == file_name)
        .map(|descriptor| descriptor.full_name().to_owned());
    let extensions = pool
        .all_extensions()
        .filter(|descriptor| descriptor.parent_file().name() == file_name)
        .map(|descriptor| descriptor.full_name().to_owned());
    messages
        .chain(enums)
        .chain(services)
        .chain(extensions)
        .collect()
}

fn canonical(path: &Path) -> Result<PathBuf, Error> {
    path.canonicalize().map_err(|source| Error::Path {
        path: path.to_owned(),
        source,
    })
}

fn owned_file_names(protos: &[PathBuf], includes: &[PathBuf]) -> Result<HashSet<String>, Error> {
    protos
        .iter()
        .map(|proto| {
            includes
                .iter()
                .find_map(|include| proto.strip_prefix(include).ok())
                .map(proto_name)
                .ok_or_else(|| Error::SourceOutsideIncludes(proto.clone()))
        })
        .collect()
}

fn proto_name(path: &Path) -> String {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    State,
    Sample,
    Event,
    Stream,
    Setpoint,
    Read,
    Commands,
}

impl Kind {
    fn from_number(method: &str, value: i32) -> Result<Self, Error> {
        match value {
            1 => Ok(Self::State),
            2 => Ok(Self::Sample),
            3 => Ok(Self::Event),
            4 => Ok(Self::Stream),
            5 => Ok(Self::Setpoint),
            6 => Ok(Self::Read),
            7 => Ok(Self::Commands),
            _ => Err(Error::InvalidKind {
                method: method.to_owned(),
                value,
            }),
        }
    }

    fn rust_type(self) -> &'static str {
        match self {
            Self::State => "State",
            Self::Sample => "Sample",
            Self::Event => "Event",
            Self::Stream => "Stream",
            Self::Setpoint => "Setpoint",
            Self::Read => "Read",
            Self::Commands => "Commands",
        }
    }

    fn is_publication(self) -> bool {
        matches!(
            self,
            Self::State | Self::Sample | Self::Event | Self::Stream | Self::Setpoint
        )
    }
}

#[derive(Clone, Debug)]
struct PortSpec {
    kind: Kind,
    public_name: String,
    constant_name: String,
}

fn embed_descriptor_section(
    out_dir: &Path,
    service_package: &str,
    descriptor_len: usize,
    descriptor_file: &str,
) -> Result<(), Error> {
    let generated = out_dir.join(format!("{service_package}.rs"));
    let frame_len = descriptor_len
        .checked_add(phoxal_port_frame_header_bytes())
        .ok_or(Error::DescriptorBounds {
            what: "framed bytes",
            limit: usize::MAX,
            actual: descriptor_len,
        })?;
    let section = format!(
        "\n#[doc(hidden)]\n#[used]\n#[cfg_attr(target_os = \"macos\", unsafe(link_section = \"__DATA,__phoxal_desc\"))]\n#[cfg_attr(not(target_os = \"macos\"), unsafe(link_section = \".phoxal_desc\"))]\nstatic __PHOXAL_DESCRIPTOR_SET: [u8; {frame_len}] = ::phoxal::port::descriptor_frame::<{frame_len}>(include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{descriptor_file}\")));\n"
    );
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&generated)
        .map_err(|source| Error::Prost(std::io::Error::new(source.kind(), source)))?;
    use std::io::Write;
    file.write_all(section.as_bytes())?;
    Ok(())
}

const fn phoxal_port_frame_header_bytes() -> usize {
    16
}

fn validate_owned_ports(
    pool: &DescriptorPool,
    owned_names: &HashSet<String>,
) -> Result<HashMap<String, PortSpec>, Error> {
    let owned_files = pool
        .files()
        .filter(|file| owned_names.contains(file.name()))
        .collect::<Vec<_>>();
    if !owned_files
        .iter()
        .any(|file| file.services().next().is_some())
    {
        return Ok(HashMap::new());
    }
    let extension = pool
        .get_extension_by_name(PORT_KIND_EXTENSION)
        .ok_or(Error::MissingPortKindExtension)?;
    let mut ports = HashMap::new();
    let mut package_modules: HashMap<String, HashSet<String>> = HashMap::new();

    for file in owned_files {
        for service in file.services() {
            let module_name = service.name().to_snake_case();
            if !package_modules
                .entry(service.package_name().to_owned())
                .or_default()
                .insert(module_name.clone())
            {
                return Err(Error::NameCollision {
                    what: "service module",
                    name: module_name,
                    owner: service.package_name().to_owned(),
                });
            }

            let mut names = HashSet::new();
            let mut constants = HashSet::new();
            for method in service.methods() {
                let full_name = method.full_name().to_owned();
                let options = method.options();
                if !options.has_extension(&extension) {
                    return Err(Error::MissingKind(full_name));
                }
                let value = match options.get_extension(&extension).as_ref() {
                    Value::EnumNumber(value) => *value,
                    _ => {
                        return Err(Error::InvalidKind {
                            method: full_name,
                            value: 0,
                        });
                    }
                };
                let kind = Kind::from_number(&full_name, value)?;
                validate_shape(&method, kind)?;

                let public_name = method.name().to_snake_case();
                let constant_name = method.name().to_shouty_snake_case();
                if !names.insert(public_name.clone()) {
                    return Err(Error::NameCollision {
                        what: "public port",
                        name: public_name,
                        owner: service.full_name().to_owned(),
                    });
                }
                if !constants.insert(constant_name.clone()) {
                    return Err(Error::NameCollision {
                        what: "Rust constant",
                        name: constant_name,
                        owner: service.full_name().to_owned(),
                    });
                }
                ports.insert(
                    full_name,
                    PortSpec {
                        kind,
                        public_name,
                        constant_name,
                    },
                );
            }
        }
    }
    Ok(ports)
}

fn validate_shape(method: &prost_reflect::MethodDescriptor, kind: Kind) -> Result<(), Error> {
    let full_name = method.full_name().to_owned();
    if method.is_client_streaming() {
        return Err(Error::InvalidShape {
            method: full_name,
            kind: kind.rust_type(),
            reason: "client streaming is not supported",
        });
    }
    if kind.is_publication() {
        if !method.is_server_streaming() {
            return Err(Error::InvalidShape {
                method: full_name,
                kind: kind.rust_type(),
                reason: "publication ports must return a stream",
            });
        }
        if method.input().full_name() != "google.protobuf.Empty" {
            return Err(Error::InvalidShape {
                method: full_name,
                kind: kind.rust_type(),
                reason: "publication ports must accept google.protobuf.Empty",
            });
        }
    } else if method.is_server_streaming() {
        return Err(Error::InvalidShape {
            method: full_name,
            kind: kind.rust_type(),
            reason: "read and command ports must be unary",
        });
    }
    Ok(())
}

struct PortGenerator {
    ports: HashMap<String, PortSpec>,
}

impl prost_build::ServiceGenerator for PortGenerator {
    fn generate(&mut self, service: prost_build::Service, buffer: &mut String) {
        // `protoc` hands Prost every service in the imported descriptor
        // closure, while typed ports are generated only for the owner's
        // service methods. Imported owner services are referenced through
        // `extern_path` and must not be looked up in this owner's port map.
        if !service.methods.iter().any(|method| {
            let full_name = format!(
                "{}.{}.{}",
                service.package, service.proto_name, method.proto_name
            );
            self.ports.contains_key(&full_name)
        }) {
            // Prost still expects a generated module for every imported
            // service package when it finalizes service output. Keep that
            // module empty because the imported package is mapped through an
            // `extern_path` and its service ports belong to its owner crate.
            let module_name = service.proto_name.to_snake_case();
            buffer.push_str(&format!("pub mod {module_name} {{}}\n"));
            return;
        }
        let module_name = service.proto_name.to_snake_case();
        buffer.push_str(&format!("pub mod {module_name} {{\n"));
        for method in &service.methods {
            let full_name = format!(
                "{}.{}.{}",
                service.package, service.proto_name, method.proto_name
            );
            let spec = &self.ports[&full_name];
            let port_type = spec.kind.rust_type();
            let input_type = nested_type(&method.input_type);
            let output_type = nested_type(&method.output_type);
            let request_name = proto_type_name(&method.input_proto_type);
            let response_name = proto_type_name(&method.output_proto_type);
            let service_name = if service.package.is_empty() {
                service.proto_name.clone()
            } else {
                format!("{}.{}", service.package, service.proto_name)
            };
            if spec.kind.is_publication() {
                buffer.push_str(&format!(
                    "    pub const {}: ::phoxal::port::{}<{}> = ::phoxal::port::{}::with_signature({:?}, {:?}, {:?}, {:?}, {:?}, &super::__PHOXAL_DESCRIPTOR_SET);",
                    spec.constant_name,
                    port_type,
                    output_type,
                    port_type,
                    spec.public_name,
                    service_name,
                    method.proto_name,
                    request_name,
                    response_name,
                ));
                buffer.push('\n');
            } else {
                buffer.push_str(&format!(
                    "    pub const {}: ::phoxal::port::{}<{}, {}> = ::phoxal::port::{}::with_signature({:?}, {:?}, {:?}, {:?}, {:?}, &super::__PHOXAL_DESCRIPTOR_SET);",
                    spec.constant_name,
                    port_type,
                    input_type,
                    output_type,
                    port_type,
                    spec.public_name,
                    service_name,
                    method.proto_name,
                    request_name,
                    response_name,
                ));
                buffer.push('\n');
            }
        }
        buffer.push_str("    pub mod ports {\n");
        for method in &service.methods {
            let full_name = format!(
                "{}.{}.{}",
                service.package, service.proto_name, method.proto_name
            );
            let constant = &self.ports[&full_name].constant_name;
            buffer.push_str(&format!("        pub use super::{constant};\n"));
        }
        buffer.push_str("    }\n}\n");
    }
}

fn proto_type_name(proto_type: &str) -> String {
    proto_type.trim_start_matches('.').to_owned()
}

fn nested_type(rust_type: &str) -> String {
    if rust_type.starts_with("::") {
        rust_type.to_owned()
    } else {
        format!("super::{rust_type}")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};

    use prost::Message as _;
    use prost_reflect::{DescriptorPool, DynamicMessage, Value};
    use prost_types::FileDescriptorSet;

    use super::{
        DESCRIPTOR_FILE, DependencyDescriptor, Error, Kind, PORT_PROTO, compile_to,
        compile_to_with_dependencies, include_dir, merge_dependency_descriptors,
    };

    fn compile_sources(
        files: &[(&str, &str)],
    ) -> (tempfile::TempDir, tempfile::TempDir, Result<(), Error>) {
        let source = tempfile::tempdir().expect("temporary source");
        let output = tempfile::tempdir().expect("temporary output");
        let mut paths = Vec::with_capacity(files.len());
        for (relative, contents) in files {
            let path = source.path().join(relative);
            fs::create_dir_all(path.parent().expect("fixture has a parent directory"))
                .expect("fixture directory");
            fs::write(&path, contents).expect("fixture source");
            paths.push(path);
        }
        let result = compile_to(&paths, &[source.path()], output.path(), DESCRIPTOR_FILE);
        (source, output, result)
    }

    const MESSAGES: &str = r#"
        syntax = "proto3";
        package example.inspection.v1;
        import "example/shared/v1/payload.proto";

        message InspectionState { example.shared.v1.SharedPayload payload = 1; }
        message InspectionSample { uint64 sequence = 1; }
        message InspectionEvent { string kind = 1; }
        message InspectionStream { uint64 sequence = 1; }
        message InspectionSetpoint { double target = 1; }
        message InspectionReadRequest { string key = 1; }
        message InspectionReadResponse { example.shared.v1.SharedPayload payload = 1; }
        message InspectionCommandRequest { string command = 1; }
        message InspectionCommandResponse { bool accepted = 1; }
    "#;

    const SHARED_PAYLOAD: &str = r#"
        syntax = "proto3";
        package example.shared.v1;
        message SharedPayload { uint64 value = 1; }
    "#;

    const ALL_KINDS: &str = r#"
        syntax = "proto3";
        package example.inspection.v1;
        import "google/protobuf/empty.proto";
        import "phoxal/port.proto";
        import "example/inspection/v1/messages.proto";

        service Inspection {
          rpc Status(google.protobuf.Empty) returns (stream InspectionState) {
            option (phoxal.port.kind) = STATE;
          }
          rpc Samples(google.protobuf.Empty) returns (stream InspectionSample) {
            option (phoxal.port.kind) = SAMPLE;
          }
          rpc Events(google.protobuf.Empty) returns (stream InspectionEvent) {
            option (phoxal.port.kind) = EVENT;
          }
          rpc Records(google.protobuf.Empty) returns (stream InspectionStream) {
            option (phoxal.port.kind) = STREAM;
          }
          rpc Target(google.protobuf.Empty) returns (stream InspectionSetpoint) {
            option (phoxal.port.kind) = SETPOINT;
          }
          rpc Read(InspectionReadRequest) returns (InspectionReadResponse) {
            option (phoxal.port.kind) = READ;
          }
          rpc Commands(InspectionCommandRequest) returns (InspectionCommandResponse) {
            option (phoxal.port.kind) = COMMANDS;
          }
          rpc Current(google.protobuf.Empty) returns (stream InspectionState) {
            option (phoxal.port.kind) = SAMPLE;
          }
        }
    "#;

    #[test]
    fn packaged_option_has_stable_public_identity() {
        assert!(PORT_PROTO.contains("package phoxal.port;"));
        assert!(PORT_PROTO.contains("PortKind kind = 50000;"));
        assert!(include_dir().join("phoxal/port.proto").is_file());
    }

    #[test]
    fn compiles_message_only_contract_without_port_option_import() {
        let source = tempfile::tempdir().expect("temporary source");
        let output = tempfile::tempdir().expect("temporary output");
        let proto = source.path().join("vocabulary.proto");
        fs::write(
            &proto,
            r#"
                syntax = "proto3";
                package example.vocabulary.v1;
                message Measurement { optional double value = 1; }
            "#,
        )
        .expect("fixture source");

        compile_to(&[&proto], &[source.path()], output.path(), DESCRIPTOR_FILE).expect("contract generation");

        let generated = fs::read_to_string(output.path().join("example.vocabulary.v1.rs"))
            .expect("generated Rust");
        assert!(generated.contains("pub struct Measurement"));
        // A messages-only owner must not pull the typed-port vocabulary into
        // its generated code. Both the legacy bare-crate spelling and the
        // current `phoxal::port` SDK path are absent; a regression that
        // silently added a port annotation would fail at least one of these.
        assert!(!generated.contains("phoxal_port"));
        assert!(!generated.contains("phoxal::port"));
        assert!(!generated.contains("descriptor_frame"));
        assert!(output.path().join(DESCRIPTOR_FILE).is_file());
    }

    #[test]
    fn generates_typed_ports_and_original_descriptors() {
        let (_source, output, result) = compile_sources(&[
            ("example/shared/v1/payload.proto", SHARED_PAYLOAD),
            ("example/inspection/v1/messages.proto", MESSAGES),
            ("example/inspection/v1/inspection.proto", ALL_KINDS),
        ]);
        result.expect("contract generation");

        let generated = fs::read_to_string(output.path().join("example.inspection.v1.rs"))
            .expect("generated Rust");
        assert!(generated.contains("pub mod inspection"));
        assert!(generated.contains("phoxal::port::State<super::InspectionState>"));
        assert!(generated.contains("phoxal::port::Sample<super::InspectionSample>"));
        assert!(generated.contains("phoxal::port::Event<super::InspectionEvent>"));
        assert!(generated.contains("phoxal::port::Stream<super::InspectionStream>"));
        assert!(generated.contains("phoxal::port::Setpoint<super::InspectionSetpoint>"));
        assert!(generated.contains("phoxal::port::Read<"));
        assert!(generated.contains("super::InspectionReadRequest"));
        assert!(generated.contains("super::InspectionReadResponse"));
        assert!(generated.contains("phoxal::port::Commands<"));
        assert!(generated.contains("super::InspectionCommandRequest"));
        assert!(generated.contains("super::InspectionCommandResponse"));
        assert!(generated.contains("pub use super::STATUS"));
        assert!(generated.contains("pub use super::CURRENT"));
        assert!(output.path().join(DESCRIPTOR_FILE).is_file());

        let descriptors = fs::read(output.path().join(DESCRIPTOR_FILE)).expect("descriptors");
        let pool = DescriptorPool::decode(descriptors.as_slice()).expect("descriptor closure");
        assert!(
            pool.files()
                .any(|file| file.name() == "example/shared/v1/payload.proto")
        );
        assert!(
            pool.files()
                .any(|file| file.name() == "google/protobuf/empty.proto")
        );
        let extension = pool
            .get_extension_by_name("phoxal.port.kind")
            .expect("packaged kind extension");
        let service = pool
            .get_service_by_name("example.inspection.v1.Inspection")
            .expect("inspection service");
        let status = service
            .methods()
            .find(|method| method.name() == "Status")
            .expect("status method");
        assert_eq!(
            status.options().get_extension(&extension).as_ref(),
            &Value::EnumNumber(1)
        );
        assert!(pool.get_message_by_name("google.protobuf.Empty").is_some());
    }

    #[test]
    fn imports_dependency_descriptors_without_dependency_sources() {
        let (dependency_source, dependency_output, dependency_result) = compile_sources(&[(
            "example/shared/v1/payload.proto",
            r#"
                syntax = "proto3";
                package example.shared.v1;
                message SharedPayload { uint64 value = 1; }
            "#,
        )]);
        dependency_result.expect("dependency descriptor generation");
        let descriptors = fs::read(dependency_output.path().join(DESCRIPTOR_FILE))
            .expect("dependency descriptors");
        drop(dependency_source);

        let owner_source = tempfile::tempdir().expect("owner source");
        let owner_output = tempfile::tempdir().expect("owner output");
        let owner_proto = owner_source.path().join("example/owner/v1/owner.proto");
        fs::create_dir_all(owner_proto.parent().expect("owner parent")).expect("owner directory");
        fs::write(
            &owner_proto,
            r#"
                syntax = "proto3";
                package example.owner.v1;
                import "example/shared/v1/payload.proto";
                message Owned { example.shared.v1.SharedPayload payload = 1; }
            "#,
        )
        .expect("owner source");

        compile_to_with_dependencies(
            &[&owner_proto],
            &[owner_source.path()],
            owner_output.path(),
            &[DependencyDescriptor::new("example-shared", &descriptors)],
            &[(".example.shared.v1", "::example_shared")],
            DESCRIPTOR_FILE,
        )
        .expect("descriptor-only dependency import");

        let generated = fs::read_to_string(owner_output.path().join("example.owner.v1.rs"))
            .expect("generated owner binding");
        assert!(generated.contains("::example_shared::SharedPayload"));
        let closure = fs::read(owner_output.path().join(DESCRIPTOR_FILE)).expect("owner closure");
        let pool = DescriptorPool::decode(closure.as_slice()).expect("owner descriptor closure");
        assert!(
            pool.get_message_by_name("example.shared.v1.SharedPayload")
                .is_some()
        );
    }

    #[test]
    fn imports_dependency_descriptors_without_source_locations() {
        let (_dependency_source, dependency_output, dependency_result) =
            compile_sources(&[("example/shared/v1/payload.proto", SHARED_PAYLOAD)]);
        dependency_result.expect("dependency descriptor generation");
        let descriptors = fs::read(dependency_output.path().join(DESCRIPTOR_FILE))
            .expect("dependency descriptors");
        let mut stripped = FileDescriptorSet::decode(descriptors.as_slice())
            .expect("dependency descriptor set decodes");
        for file in &mut stripped.file {
            file.source_code_info = None;
        }
        let stripped = stripped.encode_to_vec();

        let owner_source = tempfile::tempdir().expect("owner source");
        let owner_output = tempfile::tempdir().expect("owner output");
        let owner_proto = owner_source.path().join("example/owner/v1/owner.proto");
        fs::create_dir_all(owner_proto.parent().expect("owner parent")).expect("owner directory");
        fs::write(
            &owner_proto,
            r#"
                syntax = "proto3";
                package example.owner.v1;
                import "example/shared/v1/payload.proto";
                message Owned { example.shared.v1.SharedPayload payload = 1; }
            "#,
        )
        .expect("owner source");

        compile_to_with_dependencies(
            &[&owner_proto],
            &[owner_source.path()],
            owner_output.path(),
            &[DependencyDescriptor::new("example-shared", &stripped)],
            &[(".example.shared.v1", "::example_shared")],
            DESCRIPTOR_FILE,
        )
        .expect("source-location-free dependency import");

        let closure = fs::read(owner_output.path().join(DESCRIPTOR_FILE)).expect("owner closure");
        let pool = DescriptorPool::decode(closure.as_slice()).expect("owner descriptor closure");
        assert!(
            pool.get_message_by_name("example.shared.v1.SharedPayload")
                .is_some()
        );
    }

    #[test]
    fn exchanges_an_owned_message_with_an_independent_python_peer() {
        let (_source, output, result) =
            compile_sources(&[("example/shared/v1/payload.proto", SHARED_PAYLOAD)]);
        result.expect("contract generation");
        let descriptors = fs::read(output.path().join(DESCRIPTOR_FILE)).expect("descriptors");
        let pool = DescriptorPool::decode(descriptors.as_slice()).expect("descriptor closure");
        let descriptor = pool
            .get_message_by_name("example.shared.v1.SharedPayload")
            .expect("owned message descriptor");
        let mut request = DynamicMessage::new(descriptor.clone());
        request
            .try_set_field_by_name("value", Value::U64(150))
            .expect("fixture value");

        let script = r#"
import sys

payload = sys.stdin.buffer.read()
if payload != bytes((0x08, 0x96, 0x01)):
    raise SystemExit(f"unexpected protobuf payload: {payload.hex()}")
sys.stdout.buffer.write(bytes((0x08, 0xAC, 0x02)))
"#;
        let mut peer = Command::new("python3")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("independent Python peer starts");
        peer.stdin
            .as_mut()
            .expect("Python stdin")
            .write_all(&request.encode_to_vec())
            .expect("Rust request reaches Python");
        drop(peer.stdin.take());
        let reply = peer.wait_with_output().expect("Python peer completes");
        assert!(reply.status.success(), "Python peer rejected the request");

        let reply = DynamicMessage::decode(descriptor, reply.stdout.as_slice())
            .expect("Python reply decodes in Rust");
        assert_eq!(
            reply
                .get_field_by_name("value")
                .expect("reply value")
                .as_ref(),
            &Value::U64(300)
        );
    }

    #[test]
    fn accepts_an_identical_transitive_descriptor_diamond() {
        let (_left_source, left_output, left_result) = compile_sources(&[
            (
                "example/shared/v1/shared.proto",
                "syntax = \"proto3\"; package example.shared.v1; message Shared {}",
            ),
            (
                "example/left/v1/left.proto",
                "syntax = \"proto3\"; package example.left.v1; import \"example/shared/v1/shared.proto\"; message Left { example.shared.v1.Shared value = 1; }",
            ),
        ]);
        left_result.expect("left descriptor generation");
        let (_right_source, right_output, right_result) = compile_sources(&[
            (
                "example/shared/v1/shared.proto",
                "syntax = \"proto3\"; package example.shared.v1; message Shared {}",
            ),
            (
                "example/right/v1/right.proto",
                "syntax = \"proto3\"; package example.right.v1; import \"example/shared/v1/shared.proto\"; message Right { example.shared.v1.Shared value = 1; }",
            ),
        ]);
        right_result.expect("right descriptor generation");
        let left = fs::read(left_output.path().join(DESCRIPTOR_FILE)).expect("left descriptors");
        let right = fs::read(right_output.path().join(DESCRIPTOR_FILE)).expect("right descriptors");

        let merged = merge_dependency_descriptors(&[
            DependencyDescriptor::new("example-left", &left),
            DependencyDescriptor::new("example-right", &right),
        ])
        .expect("identical diamond");
        let pool = DescriptorPool::decode(merged.as_slice()).expect("merged descriptor closure");
        assert!(pool.get_message_by_name("example.left.v1.Left").is_some());
        assert!(pool.get_message_by_name("example.right.v1.Right").is_some());
        assert_eq!(
            pool.files()
                .filter(|file| file.name() == "example/shared/v1/shared.proto")
                .count(),
            1
        );
    }

    #[test]
    fn rejects_conflicting_dependency_paths_before_protoc() {
        let (_first_source, first_output, first_result) = compile_sources(&[(
            "example/shared/v1/shared.proto",
            "syntax = \"proto3\"; package example.shared.v1; message Shared { uint64 value = 1; }",
        )]);
        first_result.expect("first descriptor generation");
        let (_second_source, second_output, second_result) = compile_sources(&[(
            "example/shared/v1/shared.proto",
            "syntax = \"proto3\"; package example.shared.v1; message Shared { string value = 1; }",
        )]);
        second_result.expect("second descriptor generation");
        let first = fs::read(first_output.path().join(DESCRIPTOR_FILE)).expect("first descriptors");
        let second =
            fs::read(second_output.path().join(DESCRIPTOR_FILE)).expect("second descriptors");

        assert!(matches!(
            merge_dependency_descriptors(&[
                DependencyDescriptor::new("first-owner", &first),
                DependencyDescriptor::new("second-owner", &second),
            ]),
            Err(Error::DescriptorConflict { identity, first, second })
                if identity == "example/shared/v1/shared.proto"
                    && first == "first-owner"
                    && second == "second-owner"
        ));
    }

    #[test]
    fn rejects_conflicting_dependency_symbols_before_protoc() {
        let (_first_source, first_output, first_result) = compile_sources(&[(
            "example/first/v1/shared.proto",
            "syntax = \"proto3\"; package example.shared.v1; message Shared {}",
        )]);
        first_result.expect("first descriptor generation");
        let (_second_source, second_output, second_result) = compile_sources(&[(
            "example/second/v1/shared.proto",
            "syntax = \"proto3\"; package example.shared.v1; message Shared {}",
        )]);
        second_result.expect("second descriptor generation");
        let first = fs::read(first_output.path().join(DESCRIPTOR_FILE)).expect("first descriptors");
        let second =
            fs::read(second_output.path().join(DESCRIPTOR_FILE)).expect("second descriptors");

        assert!(matches!(
            merge_dependency_descriptors(&[
                DependencyDescriptor::new("first-owner", &first),
                DependencyDescriptor::new("second-owner", &second),
            ]),
            Err(Error::DescriptorConflict { identity, first, second })
                if identity == "example.shared.v1.Shared"
                    && first == "first-owner"
                    && second == "second-owner"
        ));
    }

    #[test]
    fn rejects_a_missing_kind() {
        let (_source, _output, result) = compile_sources(&[(
            "missing.proto",
            r#"
                syntax = "proto3";
                package example.invalid;
                import "google/protobuf/empty.proto";
                import "phoxal/port.proto";
                message State {}
                service Missing {
                  rpc Status(google.protobuf.Empty) returns (stream State) {}
                }
            "#,
        )]);
        assert!(
            matches!(result, Err(Error::MissingKind(method)) if method == "example.invalid.Missing.Status")
        );
    }

    #[test]
    fn rejects_unspecified_and_unknown_kinds() {
        let (_source, _output, unspecified) = compile_sources(&[(
            "unspecified.proto",
            r#"
                syntax = "proto3";
                package example.invalid;
                import "google/protobuf/empty.proto";
                import "phoxal/port.proto";
                message State {}
                service Unspecified {
                  rpc Status(google.protobuf.Empty) returns (stream State) {
                    option (phoxal.port.kind) = PORT_KIND_UNSPECIFIED;
                  }
                }
            "#,
        )]);
        assert!(matches!(
            unspecified,
            Err(Error::InvalidKind { value: 0, .. })
        ));

        let (_source, _output, unknown) = compile_sources(&[(
            "unknown.proto",
            r#"
                syntax = "proto3";
                package example.invalid;
                import "google/protobuf/empty.proto";
                import "phoxal/port.proto";
                message State {}
                service Unknown {
                  rpc Status(google.protobuf.Empty) returns (stream State) {
                    option (phoxal.port.kind) = 42;
                  }
                }
            "#,
        )]);
        assert!(matches!(unknown, Err(Error::ProtocFailed(_))));
        assert!(matches!(
            Kind::from_number("example.invalid.Unknown.Status", 42),
            Err(Error::InvalidKind { value: 42, .. })
        ));
    }

    #[test]
    fn rejects_publication_with_unary_or_non_empty_input() {
        let (_source, _output, unary) = compile_sources(&[(
            "unary.proto",
            r#"
                syntax = "proto3";
                package example.invalid;
                import "google/protobuf/empty.proto";
                import "phoxal/port.proto";
                message State {}
                service Unary {
                  rpc Status(google.protobuf.Empty) returns (State) {
                    option (phoxal.port.kind) = STATE;
                  }
                }
            "#,
        )]);
        assert!(matches!(
            unary,
            Err(Error::InvalidShape {
                reason: "publication ports must return a stream",
                ..
            })
        ));

        let (_source, _output, non_empty) = compile_sources(&[(
            "non_empty.proto",
            r#"
                syntax = "proto3";
                package example.invalid;
                import "google/protobuf/empty.proto";
                import "phoxal/port.proto";
                message State {}
                message Request {}
                service NonEmpty {
                  rpc Status(Request) returns (stream State) {
                    option (phoxal.port.kind) = STATE;
                  }
                }
            "#,
        )]);
        assert!(matches!(
            non_empty,
            Err(Error::InvalidShape {
                reason: "publication ports must accept google.protobuf.Empty",
                ..
            })
        ));
    }

    #[test]
    fn rejects_read_and_command_streaming_shapes() {
        let (_source, _output, server_stream) = compile_sources(&[(
            "server_stream.proto",
            r#"
                syntax = "proto3";
                package example.invalid;
                import "phoxal/port.proto";
                message Request {}
                message Response {}
                service ServerStream {
                  rpc Read(Request) returns (stream Response) {
                    option (phoxal.port.kind) = READ;
                  }
                }
            "#,
        )]);
        assert!(matches!(
            server_stream,
            Err(Error::InvalidShape {
                reason: "read and command ports must be unary",
                ..
            })
        ));

        let (_source, _output, client_stream) = compile_sources(&[(
            "client_stream.proto",
            r#"
                syntax = "proto3";
                package example.invalid;
                import "phoxal/port.proto";
                message Request {}
                message Response {}
                service ClientStream {
                  rpc Read(stream Request) returns (Response) {
                    option (phoxal.port.kind) = READ;
                  }
                }
            "#,
        )]);
        assert!(matches!(
            client_stream,
            Err(Error::InvalidShape {
                reason: "client streaming is not supported",
                ..
            })
        ));
    }

    #[test]
    fn rejects_normalized_method_name_collisions() {
        let (_source, _output, result) = compile_sources(&[(
            "methods.proto",
            r#"
                syntax = "proto3";
                package example.invalid;
                import "google/protobuf/empty.proto";
                import "phoxal/port.proto";
                message State {}
                service Methods {
                  rpc FooBar(google.protobuf.Empty) returns (stream State) {
                    option (phoxal.port.kind) = STATE;
                  }
                  rpc Foo_Bar(google.protobuf.Empty) returns (stream State) {
                    option (phoxal.port.kind) = STATE;
                  }
                }
            "#,
        )]);
        assert!(matches!(
            result,
            Err(Error::NameCollision { what: "public port", name, .. }) if name == "foo_bar"
        ));
    }

    #[test]
    fn rejects_normalized_service_module_collisions() {
        let (_source, _output, result) = compile_sources(&[(
            "services.proto",
            r#"
                syntax = "proto3";
                package example.invalid;
                import "google/protobuf/empty.proto";
                import "phoxal/port.proto";
                message State {}
                service FooBar {
                  rpc Status(google.protobuf.Empty) returns (stream State) {
                    option (phoxal.port.kind) = STATE;
                  }
                }
                service Foo_Bar {
                  rpc Status(google.protobuf.Empty) returns (stream State) {
                    option (phoxal.port.kind) = STATE;
                  }
                }
            "#,
        )]);
        assert!(matches!(
            result,
            Err(Error::NameCollision { what: "service module", name, .. }) if name == "foo_bar"
        ));
    }

    #[test]
    fn protoc_rejects_duplicate_singular_kind_options() {
        let (_source, _output, result) = compile_sources(&[(
            "duplicate.proto",
            r#"
                syntax = "proto3";
                package example.invalid;
                import "google/protobuf/empty.proto";
                import "phoxal/port.proto";
                message State {}
                service Duplicate {
                  rpc Status(google.protobuf.Empty) returns (stream State) {
                    option (phoxal.port.kind) = STATE;
                    option (phoxal.port.kind) = SAMPLE;
                  }
                }
            "#,
        )]);
        assert!(matches!(result, Err(Error::ProtocFailed(_))));
    }

    #[test]
    fn rejects_owned_sources_outside_include_roots() {
        let source = tempfile::tempdir().expect("temporary source");
        let output = tempfile::tempdir().expect("temporary output");
        let outside = tempfile::NamedTempFile::new().expect("temporary outside source");
        let result = compile_to(
            &[PathBuf::from(outside.path())],
            &[source.path()],
            output.path(),
            DESCRIPTOR_FILE,
        );
        assert!(matches!(result, Err(Error::SourceOutsideIncludes(_))));
    }
}
