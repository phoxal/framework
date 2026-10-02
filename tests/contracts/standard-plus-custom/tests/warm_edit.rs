//! Warm-edit versus fresh-build regression for the derived standard
//! endpoint surface.
//!
//! A component that drops a capability must lose the derived endpoint on
//! an ordinary warm rebuild, not only on a fresh build: OUT_DIR contents
//! are invisible to Cargo's fingerprints, so the build helper digests its
//! generated surface into a build directive. This test authors a scratch
//! component package, prints its composed output surface from the built
//! binary, edits `component.yaml`, rebuilds warm, and compares against a
//! fresh cold build of the edited document.
//!
//! The test spawns Cargo on a scratch package with its own target
//! directory. To avoid nested-cargo lock deadlocks it is ignored by
//! default and additionally gated on an environment variable: run the
//! compiled test binary directly (not under `cargo test`) with
//! `PHOXAL_WARM_EDIT_REGRESSION=1`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const WITH_ENCODER: &str = "schema: phoxal/component/v0
model: { file: model.xml, root_body: mount }
capabilities:
  encoder:
    kind: encoder
    publish_rate_hz: 50.0
    target:
      kind: joint
      id: fixture_joint
";

const WITHOUT_ENCODER: &str = "schema: phoxal/component/v0
model: { file: model.xml, root_body: mount }
capabilities: {}
";

const MAIN_RS: &str = r#"//! Scratch component: a custom operation plus the derived standard
//! surface of the capabilities declared in component.yaml.

use phoxal::contracts::{Empty, RequestReply};

#[phoxal::endpoints]
pub struct FixtureApi {
    #[phoxal::operation(
        contract = "fixture.calibrate.v1.Calibrate",
        max_items = 4,
        max_bytes = 1024
    )]
    calibrate: RequestReply<Empty, Empty>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Fixture;

#[phoxal::runtime(contract = FixtureApi, period_ms = 20)]
impl Fixture {
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {
        Ok(Self)
    }

    #[handle(calibrate)]
    fn calibrate(
        &mut self,
        _ctx: &mut phoxal::runtime::Context<'_, Self>,
        _request: Empty,
    ) -> phoxal::Result<Empty> {
        Ok(Empty {})
    }

    #[step]
    fn advance(&mut self, _ctx: &mut phoxal::runtime::Context<'_, Self>) -> phoxal::Result<()> {
        Ok(())
    }
}

fn main() -> phoxal::Result<()> {
    if std::env::args().any(|arg| arg == "--print-outputs") {
        use phoxal::runtime::Runtime;
        use phoxal::runtime::outputs::OutputSet;
        let mut outputs: Vec<&str> =
            <phoxal_runtime_fixture::Adapter as Runtime>::Outputs::FIELDS
                .iter()
                .map(|field| field.name)
                .collect();
        outputs.sort_unstable();
        println!("{}", outputs.join(","));
        return Ok(());
    }
    phoxal::runtime::run::<Fixture>()
}
"#;

fn framework_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("framework root")
        .to_path_buf()
}

fn write_scratch(root: &Path, document: &str) -> PathBuf {
    let scratch = root.join("warm-edit-component");
    fs::create_dir_all(scratch.join("src")).expect("create scratch source directory");
    let manifest = format!(
        "[package]\n\
         name = \"warm-edit-component\"\n\
         version = \"0.1.0\"\n\
         edition = \"2024\"\n\
         publish = false\n\n\
         [[bin]]\n\
         name = \"warm-edit-component\"\n\
         path = \"src/main.rs\"\n\n\
         [build-dependencies]\n\
         phoxal = {{ path = {:?}, default-features = false, features = [\"build\"] }}\n\n\
         [dependencies]\n\
         phoxal = {{ path = {:?}, default-features = false, features = [\"runtime\"] }}\n",
        framework_root().join("phoxal"),
        framework_root().join("phoxal"),
    );
    fs::write(scratch.join("Cargo.toml"), manifest).expect("write scratch manifest");
    fs::write(
        scratch.join("build.rs"),
        "fn main() -> Result<(), phoxal::build::Error> {\n    phoxal::build::api(phoxal::build::BuildApiConfig::default())\n}\n",
    )
    .expect("write scratch build script");
    fs::write(scratch.join("component.yaml"), document).expect("write scratch document");
    fs::write(scratch.join("model.xml"), include_str!("../model.xml"))
        .expect("write scratch model");
    fs::write(scratch.join("src/main.rs"), MAIN_RS).expect("write scratch main");
    scratch
}

fn build(scratch: &Path) {
    let output = Command::new(env!("CARGO"))
        .arg("build")
        .arg("--manifest-path")
        .arg(scratch.join("Cargo.toml"))
        .arg("--offline")
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .expect("spawn cargo build");
    assert!(
        output.status.success(),
        "scratch build failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn output_surface(scratch: &Path) -> String {
    let output = Command::new(scratch.join("target/debug/warm-edit-component"))
        .arg("--print-outputs")
        .output()
        .expect("run scratch component");
    assert!(
        output.status.success(),
        "scratch component failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
#[ignore = "spawns Cargo on a scratch package; run the compiled test binary directly with PHOXAL_WARM_EDIT_REGRESSION=1"]
fn capability_removal_reaches_warm_rebuilds_like_fresh_builds() {
    if std::env::var_os("PHOXAL_WARM_EDIT_REGRESSION").is_none() {
        eprintln!("skipping: PHOXAL_WARM_EDIT_REGRESSION is not set");
        return;
    }
    let root = std::env::temp_dir().join(format!("phoxal-warm-edit-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("create scratch root");

    // Warm sequence: build with the encoder, then edit the document in
    // place and rebuild without cleaning.
    let warm = write_scratch(&root, WITH_ENCODER);
    build(&warm);
    assert_eq!(
        output_surface(&warm),
        "calibrate_replies,encoder",
        "the derived encoder endpoint composes with the custom operation"
    );
    fs::write(warm.join("component.yaml"), WITHOUT_ENCODER).expect("edit scratch document");
    build(&warm);
    let warm_surface = output_surface(&warm);
    assert_eq!(
        warm_surface, "calibrate_replies",
        "a warm rebuild after removing the capability must drop the derived endpoint"
    );

    // Fresh control: a clean package built once with the edited document
    // must report exactly the warm surface.
    let fresh = write_scratch(&root.join("fresh"), WITHOUT_ENCODER);
    build(&fresh);
    assert_eq!(
        output_surface(&fresh),
        warm_surface,
        "warm and fresh builds agree after the edit"
    );

    let _ = fs::remove_dir_all(&root);
}
