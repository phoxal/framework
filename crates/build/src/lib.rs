//! Build-time generation for service-owned Phoxal Protobuf contracts.
//!
//! [`compile_protos`] supplies the shared `phoxal/port.proto` import and a
//! pinned Protobuf compiler, emits Prost messages with type names, retains the
//! original descriptor closure, and generates inert typed port references.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use heck::{ToShoutySnakeCase, ToSnakeCase};
use prost_reflect::{DescriptorPool, Value};

/// The shared option definition packaged with this crate.
pub const PORT_PROTO: &str = include_str!("../proto/phoxal/port.proto");

const DESCRIPTOR_FILE: &str = "phoxal-descriptors.bin";
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
/// ports in this build.
pub fn compile_protos(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
) -> Result<(), Error> {
    let out_dir = std::env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .ok_or(Error::MissingEnvironment("OUT_DIR"))?;
    compile_to(protos, includes, &out_dir)
}

fn compile_to(
    protos: &[impl AsRef<Path>],
    includes: &[impl AsRef<Path>],
    out_dir: &Path,
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
    let descriptor_path = out_dir.join(DESCRIPTOR_FILE);

    let mut command = Command::new(&protoc);
    command
        .arg("--include_imports")
        .arg("--include_source_info")
        .arg(format!(
            "--descriptor_set_out={}",
            descriptor_path.display()
        ));
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
    config.compile_protos(&owned_paths, &include_roots)?;

    if has_ports {
        embed_descriptor_section(
            out_dir,
            service_package.as_deref().unwrap_or_default(),
            descriptor_bytes.len(),
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
        "\n#[doc(hidden)]\n#[used]\n#[cfg_attr(target_os = \"macos\", unsafe(link_section = \"__DATA,__phoxal_desc\"))]\n#[cfg_attr(not(target_os = \"macos\"), unsafe(link_section = \".phoxal_desc\"))]\nstatic __PHOXAL_DESCRIPTOR_SET: [u8; {frame_len}] = ::phoxal_port::descriptor_frame::<{frame_len}>(include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{DESCRIPTOR_FILE}\")));\n"
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
                    "    pub const {}: ::phoxal_port::{}<{}> = ::phoxal_port::{}::with_signature({:?}, {:?}, {:?}, {:?}, {:?}, &super::__PHOXAL_DESCRIPTOR_SET);",
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
                    "    pub const {}: ::phoxal_port::{}<{}, {}> = ::phoxal_port::{}::with_signature({:?}, {:?}, {:?}, {:?}, {:?}, &super::__PHOXAL_DESCRIPTOR_SET);",
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
    use std::path::PathBuf;

    use prost_reflect::{DescriptorPool, Value};

    use super::{DESCRIPTOR_FILE, Error, Kind, PORT_PROTO, compile_to, include_dir};

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
        let result = compile_to(&paths, &[source.path()], output.path());
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

        compile_to(&[&proto], &[source.path()], output.path()).expect("contract generation");

        let generated = fs::read_to_string(output.path().join("example.vocabulary.v1.rs"))
            .expect("generated Rust");
        assert!(generated.contains("pub struct Measurement"));
        assert!(!generated.contains("phoxal_port"));
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
        assert!(generated.contains("phoxal_port::State<super::InspectionState>"));
        assert!(generated.contains("phoxal_port::Sample<super::InspectionSample>"));
        assert!(generated.contains("phoxal_port::Event<super::InspectionEvent>"));
        assert!(generated.contains("phoxal_port::Stream<super::InspectionStream>"));
        assert!(generated.contains("phoxal_port::Setpoint<super::InspectionSetpoint>"));
        assert!(generated.contains("phoxal_port::Read<"));
        assert!(generated.contains("super::InspectionReadRequest"));
        assert!(generated.contains("super::InspectionReadResponse"));
        assert!(generated.contains("phoxal_port::Commands<"));
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
        );
        assert!(matches!(result, Err(Error::SourceOutsideIncludes(_))));
    }
}
