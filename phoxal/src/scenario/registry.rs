//! Static link-time scenario registry backed by [`inventory`].
//!
//! The [`phoxal::scenario`](crate::scenario) attribute submits one
//! [`ScenarioDescriptor`] per `impl Scenario for ConcreteType` block. The
//! harness iterates the registry, rejects duplicate struct identifiers, and
//! dispatches each selected case to its monomorphized entry function.
//!
//! Inventory iteration order is unspecified; the harness sorts before
//! listing or running. See <https://docs.rs/inventory/latest/inventory/>.
//!
//! Per the plan: monomorphized entry, no `dyn Scenario`. The descriptor
//! stores a non-generic `fn()` that internally constructs the concrete
//! scenario via `Default::default()`, calls `plan()`, executes the
//! simulation, and finally invokes `verify()`. The public [`Scenario`]
//! trait is only used for compile-time checking of the user's impl.

use std::sync::OnceLock;

/// One registration entry. The attribute generates a static of this type
/// at the impl site; the harness reads the registry through
/// [`list_scenarios`] and [`ScenarioRegistry::get`].
#[derive(Clone, Copy)]
pub struct ScenarioDescriptor {
    /// Public identity. Always of the form `scenarios/<StructIdent>`.
    pub name: &'static str,
    /// Struct identifier without the `scenarios/` prefix; the form `cargo
    /// phoxal simulation scenario run` accepts as input.
    pub short_name: &'static str,
    /// Rust module path of the impl, retained for diagnostics only.
    pub module_path: &'static str,
    /// Source file path as reported by the proc macro; diagnostics only.
    pub source_file: &'static str,
    /// Source line of the `impl` keyword; diagnostics only.
    pub source_line: u32,
    /// Monomorphized entry function. Each registered type produces its own
    /// concrete `fn()` through the `#[phoxal::scenario]` attribute; the
    /// descriptor stores the resulting non-generic function pointer.
    pub entry: ScenarioEntryFn,
}

/// Signature of the monomorphized case-host entry.
///
/// Each registered scenario type yields one `fn()` of this shape; the
/// attribute emits the body which constructs the concrete type, calls
/// `plan()`, runs the simulation, and finally invokes `verify()`. The
/// return uses `phoxal::Result` (which is `anyhow::Result`) so the
/// attribute can propagate user errors without coercing them through a
/// boxed trait object.
pub type ScenarioEntryFn = fn() -> crate::Result<ScenarioOutcome>;

/// Outcome of a scenario run. P3 will replace the body with the typed
/// [`ScenarioRun`](crate::scenario::ScenarioRun) carrying evidence; P1
/// uses a placeholder so the harness skeleton compiles and the gate can
/// verify discovery + dispatch.
#[derive(Debug, Clone)]
pub struct ScenarioOutcome {
    pub name: String,
    pub passed: bool,
    pub detail: Option<String>,
}

inventory::collect!(ScenarioDescriptor);

/// Snapshot of the registry, captured on first access and reused for
/// subsequent dispatches. Inventory's `iter()` is unsorted; we sort once
/// and hand back an immutable view.
#[derive(Debug, Clone)]
pub struct ScenarioRegistry {
    entries: Vec<RegisteredScenario>,
}

#[derive(Debug, Clone)]
pub struct RegisteredScenario {
    pub name: &'static str,
    pub short_name: &'static str,
    pub module_path: &'static str,
    pub source_file: &'static str,
    pub source_line: u32,
    pub entry: ScenarioEntryFn,
}

impl ScenarioRegistry {
    /// Capture and sort the registry. After this returns the snapshot is
    /// stable for the process lifetime.
    pub fn snapshot() -> &'static Self {
        SNAPSHOT.get_or_init(|| {
            let mut entries: Vec<RegisteredScenario> = inventory::iter::<ScenarioDescriptor>()
                .map(|d| RegisteredScenario {
                    name: d.name,
                    short_name: d.short_name,
                    module_path: d.module_path,
                    source_file: d.source_file,
                    source_line: d.source_line,
                    entry: d.entry,
                })
                .collect();
            entries.sort_by(|a, b| a.short_name.cmp(b.short_name));
            Self { entries }
        })
    }

    /// All registered scenarios, sorted by short name.
    pub fn all() -> &'static [RegisteredScenario] {
        &Self::snapshot().entries
    }

    /// Look up a scenario by short name (`ForwardTurnStop`). Returns
    /// `None` if not registered.
    pub fn get(short_name: &str) -> Option<&'static RegisteredScenario> {
        Self::all().iter().find(|e| e.short_name == short_name)
    }

    /// Reject duplicate `scenarios/<StructIdent>` identities. The
    /// descriptors themselves can also enforce uniqueness through a
    /// compile-time macro check; this runtime guard is the authoritative
    /// one before any launch.
    pub fn duplicates() -> Vec<(
        &'static str,
        &'static RegisteredScenario,
        &'static RegisteredScenario,
    )> {
        let mut out = Vec::new();
        let entries = Self::all();
        for (i, a) in entries.iter().enumerate() {
            for b in &entries[i + 1..] {
                if a.name == b.name {
                    out.push((a.name, a, b));
                }
            }
        }
        out
    }
}

static SNAPSHOT: OnceLock<ScenarioRegistry> = OnceLock::new();

/// Public list dispatch used by `cargo phoxal simulation scenario list`.
///
/// Returns `Err` with both source locations if the binary contains two
/// scenarios sharing the `scenarios/<StructIdent>` identity. The plan
/// forbids silent file-based disambiguation: listing must fail loudly so
/// the duplicate is fixed at compile time or at the next build.
pub fn list_scenarios() -> Result<&'static [RegisteredScenario], DuplicateScenarioError> {
    let entries = ScenarioRegistry::all();
    for (i, a) in entries.iter().enumerate() {
        for b in &entries[i + 1..] {
            if a.name == b.name {
                return Err(DuplicateScenarioError {
                    name: a.name,
                    first: DuplicateLocation {
                        module_path: a.module_path,
                        source_file: a.source_file,
                        source_line: a.source_line,
                    },
                    second: DuplicateLocation {
                        module_path: b.module_path,
                        source_file: b.source_file,
                        source_line: b.source_line,
                    },
                });
            }
        }
    }
    Ok(entries)
}

/// Failure when two registered scenarios share the same `scenarios/<StructIdent>`.
#[derive(Debug, Clone)]
pub struct DuplicateScenarioError {
    pub name: &'static str,
    pub first: DuplicateLocation,
    pub second: DuplicateLocation,
}

#[derive(Debug, Clone, Copy)]
pub struct DuplicateLocation {
    pub module_path: &'static str,
    pub source_file: &'static str,
    pub source_line: u32,
}

impl std::fmt::Display for DuplicateScenarioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "duplicate scenario identity `{}`: {} ({}:{}) and {} ({}:{})",
            self.name,
            self.first.module_path,
            self.first.source_file,
            self.first.source_line,
            self.second.module_path,
            self.second.source_file,
            self.second.source_line,
        )
    }
}

impl std::error::Error for DuplicateScenarioError {}
