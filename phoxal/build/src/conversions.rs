//! Build-time conversion runtime emission.
//!
//! After `cargo phoxal prepare` builds each selected participant, it inspects
//! the prepared contracts to discover the `Latest<PublishedType> ->
//! Latest<ServiceExpectedType>` edges the robot project needs to bridge.
//! Those edges are persisted to `.phoxal/conversions/edges.json`; the build
//! helper reads the sidecar at build time and emits the conversion runtime
//! into `OUT_DIR/phoxal-conversions.rs`. The robot's source attaches it with
//! `phoxal::conversions!()` and enters it through the generated
//! `run_hosted_roles`; no source-tree file or generated Cargo target is ever
//! produced.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One conversion edge `Latest<source> -> Latest<destination>`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ConversionEdge {
    /// Graph edge consumer (`instance.port`).
    pub consumer: String,
    /// Graph edge producer (`instance.port`).
    pub producer: String,
    /// Fully-qualified published payload type (Rust source side).
    pub source_type: String,
    /// Fully-qualified private payload type the service expects (Rust dest side).
    pub destination_type: String,
    /// Compiled consumer latest-age bound forwarded to the adapter input.
    pub max_age_ms: Option<u64>,
    /// Producer output byte bound compiled into the consumer input.
    pub input_max_bytes: u64,
    /// Compiled consumer input byte bound for this delivery shape.
    pub output_max_bytes: u64,
    /// Adapter runtime period (smallest among selected consumers).
    pub period_ms: u64,
    /// Adapter timeout (largest among selected consumers).
    pub timeout_ms: u64,
    /// Adapter init timeout (largest among selected consumers).
    pub init_timeout_ms: u64,
}

/// Persisted shape written by `cargo phoxal prepare` and read at build time.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ConversionPlan {
    /// All conversion edges this robot's brain executable must host.
    #[serde(default)]
    pub edges: Vec<ConversionEdge>,
}

impl ConversionPlan {
    /// Sidecar filename relative to the project root.
    pub const FILENAME: &'static str = ".phoxal/conversions/edges.json";

    /// Returns the project's local sidecar path.
    pub fn sidecar(project_root: &Path) -> PathBuf {
        project_root.join(Self::FILENAME)
    }

    /// Reads the sidecar if it exists; returns an empty plan otherwise.
    pub fn read(project_root: &Path) -> Result<Self, Error> {
        let path = Self::sidecar(project_root);
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path).map_err(|source| Error::Read {
            path: path.clone(),
            source,
        })?;
        let plan: Self =
            serde_json::from_str(&text).map_err(|source| Error::Parse { path, source })?;
        Ok(plan)
    }

    /// Writes the sidecar atomically by staging and renaming.
    pub fn write(&self, project_root: &Path) -> Result<(), Error> {
        let destination = Self::sidecar(project_root);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Write {
                path: parent.to_owned(),
                source,
            })?;
        }
        let staging = destination.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).map_err(Error::Serialize)?;
        std::fs::write(&staging, text).map_err(|source| Error::Write {
            path: staging.clone(),
            source,
        })?;
        std::fs::rename(&staging, &destination).map_err(|source| Error::Write {
            path: destination.clone(),
            source,
        })?;
        Ok(())
    }
}

/// Generates the conversion hosting glue into `OUT_DIR` from the persisted
/// plan.
///
/// `project_root` is `CARGO_MANIFEST_DIR`; the emitted file is
/// `OUT_DIR/phoxal-conversions.rs` and is attached by the robot source
/// through `phoxal::conversions!()`. An empty plan emits a compiled stub
/// whose `run_hosted_roles` runs only the brain, so robot sources compile
/// uniformly with and without conversions.
pub fn emit(out_dir: &Path, project_root: &Path) -> Result<(), Error> {
    let plan = ConversionPlan::read(project_root)?;
    let source = render(&plan);
    let path = out_dir.join("phoxal-conversions.rs");
    let mut staging = path.clone();
    staging.set_extension("rs.tmp");
    std::fs::write(&staging, source).map_err(|source| Error::Write {
        path: staging.clone(),
        source,
    })?;
    std::fs::rename(&staging, &path).map_err(|source| Error::Write {
        path: path.clone(),
        source,
    })?;
    println!(
        "cargo:rerun-if-changed={}",
        ConversionPlan::sidecar(project_root).display()
    );
    Ok(())
}

fn render(plan: &ConversionPlan) -> String {
    let mut source = String::new();
    source.push_str("// @generated by phoxal::build::conversions; do not edit.\n\n");
    if plan.edges.is_empty() {
        source.push_str(
            "/// The robot composition declares no cross-service conversions; this\n\
             /// executable hosts only the authored brain runtime.\n\
             pub fn run_hosted_roles<BRAIN>(brain: BRAIN) -> phoxal::Result<()>\n\
             where\n\
                 BRAIN: phoxal::runtime::RegisteredRuntime,\n\
                 BRAIN::Inputs:\n\
                     phoxal::runtime::input::TransportInputSet\n\
                     + phoxal::runtime::input::TransportInputSink,\n\
             {\n\
                 phoxal::runtime::dispatch_hosted(brain, &[])\n\
             }\n",
        );
        return source;
    }
    let period_ms = plan
        .edges
        .iter()
        .map(|edge| edge.period_ms)
        .min()
        .unwrap_or(1);
    let timeout_ms = plan
        .edges
        .iter()
        .map(|edge| edge.timeout_ms)
        .max()
        .unwrap_or(1);
    let init_timeout_ms = plan
        .edges
        .iter()
        .map(|edge| edge.init_timeout_ms)
        .max()
        .unwrap_or(1);

    // The conversion role and its generated bindings compile only once the
    // package's own prepared products exist — the same cfg integration
    // tests gate on — so the first preparation build (which extracts those
    // products) and recovery from a deleted prepared tree both compile the
    // brain alone, and every later build hosts the full conversion role.
    source.push_str("#[cfg(phoxal_self_prepared)]\n");
    source.push_str("include!(concat!(env!(\"OUT_DIR\"), \"/phoxal-self-retention.rs\"));\n\n");
    source.push_str(
        "#[cfg(phoxal_self_prepared)]\n#[allow(\n    clippy::too_many_lines,\n    clippy::unnecessary_fallible_conversions,\n    reason = \"generated conversion runtime supports both From and TryFrom paths\"\n)]\npub mod phoxal_conversions {\n",
    );
    source.push_str("    use phoxal::runtime::input::Latest;\n");
    source.push_str("    use phoxal::runtime::{InitContext, Runtime, Sample, StepContext};\n\n");
    for (index, edge) in plan.edges.iter().enumerate() {
        let destination = sdk_path(&edge.destination_type);
        let _ = writeln!(
            source,
            "    const TARGET_{index}: phoxal::contracts::ObservationMethod<\n        {destination},\n    > = phoxal::contracts::ObservationMethod::new(\n        \"{type_name}\",\n        \"target_{index}\",\n        \"target_{index}\",\n        \"google.protobuf.Empty\",\n        \"{type_name}\",\n        true,\n        None,\n        &[],\n    );",
            type_name = edge.destination_type,
        );
    }
    source.push_str(
        "\n    #[phoxal::runtime::inputs]\n    #[derive(Default)]\n    struct Inputs {\n",
    );
    for (index, edge) in plan.edges.iter().enumerate() {
        let _ = writeln!(source, "        // {} <- {}", edge.consumer, edge.producer);
        if let Some(max_age_ms) = edge.max_age_ms {
            let _ = writeln!(
                source,
                "        #[phoxal::runtime::input(max_age_ms = {max_age_ms}, max_bytes = {})]",
                edge.input_max_bytes
            );
        } else {
            let _ = writeln!(
                source,
                "        #[phoxal::runtime::input(max_bytes = {})]",
                edge.input_max_bytes
            );
        }
        let _ = writeln!(
            source,
            "        source_{index}: Latest<{}>,",
            sdk_path(&edge.source_type)
        );
    }
    source.push_str("    }\n\n");
    source.push_str(
        "    #[phoxal::runtime::outputs]\n    #[derive(Default)]\n    struct Outputs {\n",
    );
    for (index, edge) in plan.edges.iter().enumerate() {
        let _ = writeln!(
            source,
            "        #[phoxal::runtime::outputs::state(port = TARGET_{index}.state_port(), max_bytes = {})]",
            edge.output_max_bytes
        );
        let _ = writeln!(
            source,
            "        target_{index}: Option<Sample<{}>>,",
            sdk_path(&edge.destination_type)
        );
    }
    source.push_str("    }\n\n    struct Adapter;\n\n");
    let _ = writeln!(
        source,
        "    #[phoxal::runtime(role = \"phoxal-adapter\", arrival_releases, period_ms = {period_ms}, timeout_ms = {timeout_ms}, init_timeout_ms = {init_timeout_ms})]"
    );
    source.push_str(
        "    impl Runtime for Adapter {\n        type Config = ();\n        type State = ();\n        type Inputs = Inputs;\n        type Outputs = Outputs;\n\n        fn init(&self, _ctx: &InitContext, _config: ()) -> phoxal::Result<()> {\n            Ok(())\n        }\n\n        fn step(&self, ctx: &StepContext, state: (), inputs: &Inputs) -> phoxal::Result<((), Outputs)> {\n            let mut outputs = Outputs::default();\n",
    );
    for (index, edge) in plan.edges.iter().enumerate() {
        let destination = sdk_path(&edge.destination_type);
        let _ = writeln!(
            source,
            "        if inputs.source_{index}.is_fresh_at(ctx.now(), {:?})",
            edge.max_age_ms
        );
        let _ = writeln!(
            source,
            "            && let Some(sample) = inputs.source_{index}.sample()"
        );
        source.push_str("        {\n");
        let _ = writeln!(
            source,
            "            let converted: {destination} = sample.payload().clone().try_into().map_err(|error| phoxal::anyhow!(\"{consumer} <- {producer}: {{error:?}}\"))?;",
            consumer = edge.consumer,
            producer = edge.producer,
        );
        let _ = writeln!(
            source,
            "            outputs.target_{index} = Some(Sample::new(converted, sample.stamp().clone()));"
        );
        source.push_str("        }\n");
    }
    source.push_str(
        "            Ok((state, outputs))\n        }\n    }\n\n    #[phoxal::runtime::outputs]\n    impl Adapter {}\n\n    pub(crate) fn run_adapter() -> phoxal::Result<()> {\n        phoxal::runtime::run(Adapter)\n    }\n}\n\n",
    );
    source.push_str(
        "/// Enter this executable: run the authored brain runtime, or the\n\
         /// hosted conversion role when the supervisor launches the\n\
         /// `phoxal-adapter` instance of this same binary.\n\
         pub fn run_hosted_roles<BRAIN>(brain: BRAIN) -> phoxal::Result<()>\n\
         where\n\
             BRAIN: phoxal::runtime::RegisteredRuntime,\n\
             BRAIN::Inputs:\n\
                 phoxal::runtime::input::TransportInputSet\n\
                 + phoxal::runtime::input::TransportInputSink,\n\
         {\n\
             #[cfg(phoxal_self_prepared)]\n\
             const HOSTED: &[phoxal::runtime::HostedRole] = &[phoxal::runtime::HostedRole {\n\
                 registration: phoxal::runtime::RoleRegistration {\n\
                     instance_id: \"phoxal-adapter\",\n\
                     description: \"robot-owned conversion runtime (latest observation bridges)\",\n\
                 },\n\
                 run: phoxal_conversions::run_adapter,\n\
             }];\n\
             #[cfg(not(phoxal_self_prepared))]\n\
             const HOSTED: &[phoxal::runtime::HostedRole] = &[];\n\
             phoxal::runtime::dispatch_hosted(brain, HOSTED)\n\
         }\n",
    );
    source
}

fn sdk_path(identity: &str) -> String {
    crate::sdk_type_path(identity)
        .unwrap_or_else(|| format!("crate::api::types::{}", identity.replace('.', "::")))
}

/// Errors reported while reading, parsing, or emitting the conversion plan.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The persisted plan cannot be read.
    #[error("cannot read conversion plan {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The persisted plan is not valid JSON.
    #[error("cannot parse conversion plan {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    /// The conversion plan cannot be serialized.
    #[error("cannot serialize conversion plan: {0}")]
    Serialize(serde_json::Error),
    /// The conversion plan or generated file cannot be written.
    #[error("cannot write conversion artifact {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(max_age_ms: Option<u64>) -> ConversionEdge {
        ConversionEdge {
            consumer: "navigation.map".to_owned(),
            producer: "world.revision".to_owned(),
            source_type: "phoxal.world.v1.WorldRevision".to_owned(),
            destination_type: "example.MapState".to_owned(),
            max_age_ms,
            input_max_bytes: 128,
            output_max_bytes: 512,
            period_ms: 20,
            timeout_ms: 100,
            init_timeout_ms: 1_000,
        }
    }

    #[test]
    fn an_empty_plan_emits_a_brain_only_stub() {
        let source = render(&ConversionPlan::default());
        assert!(source.contains("dispatch_hosted(brain, &[])"), "{source}");
        assert!(!source.contains("phoxal-adapter"), "{source}");
        assert!(!source.contains("ObservationMethod"), "{source}");
    }

    #[test]
    fn a_plan_renders_the_hosted_conversion_role_faithfully() {
        let source = render(&ConversionPlan {
            edges: vec![edge(Some(100)), edge(None)],
        });
        // The hosted record names its launch instance and aligns releases
        // to arrivals; the period is the fastest consumer cadence.
        assert!(
            source.contains(
                "#[phoxal::runtime(role = \"phoxal-adapter\", arrival_releases, period_ms = 20,"
            ),
            "{source}"
        );
        // Outputs bind the canonical observation ports, inputs carry the
        // compiled byte bounds, and only bounded inputs forward.
        assert!(source.contains("port = TARGET_0.state_port()"), "{source}");
        assert!(
            source.contains("#[phoxal::runtime::input(max_age_ms = 100, max_bytes = 128)]"),
            "{source}"
        );
        assert!(
            source.contains("#[phoxal::runtime::input(max_bytes = 128)]"),
            "{source}"
        );
        assert!(
            source.contains("is_fresh_at(ctx.now(), Some(100)"),
            "{source}"
        );
        assert!(source.contains("is_fresh_at(ctx.now(), None)"), "{source}");
        // The original capture stamp is preserved through conversion.
        assert!(
            source.contains("Sample::new(converted, sample.stamp().clone())"),
            "{source}"
        );
        // The executable enters through the generated hosted dispatch,
        // gated on the package's own prepared products existing.
        assert!(source.contains("#[cfg(phoxal_self_prepared)]"), "{source}");
        assert!(
            source.contains("phoxal::runtime::dispatch_hosted("),
            "{source}"
        );
        assert!(
            source.contains("run: phoxal_conversions::run_adapter"),
            "{source}"
        );
    }
}
