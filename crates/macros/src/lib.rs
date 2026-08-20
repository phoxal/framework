//! Proc-macros for the phoxal framework.
//!
//! Three macro families make up the authoring surface:
//!
//! - [`derive@Config`] - derives the config schema embedded in participant
//!   metadata.
//! - [`macro@service`] / [`macro@driver`] / [`macro@brain`] - declare a unit
//!   marker's `Config`/`State`/`Api` types and identity (`ParticipantSpec`).
//! - [`macro@step`] - records cadence on the ordinary `Participant::step`
//!   override. Setup, reset, shutdown, and query handlers are plain Rust.
//!
//! The participant authoring macros (`macro@service` / `macro@driver` /
//! `macro@brain` / `macro@step`) reference the framework through
//! `::phoxal::…`; the engine crate makes that path resolve to itself with
//! `extern crate self as phoxal;`.

mod authoring;
mod wire_schema;

use proc_macro::TokenStream;

/// Attach a positive, finite frequency to the ordinary
/// [`Participant::step`](https://docs.rs/phoxal) override.
#[proc_macro_attribute]
pub fn step(attr: TokenStream, item: TokenStream) -> TokenStream {
    authoring::expand_step(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive a compile-time Draft 2020-12 JSON Schema from the same supported
/// `#[serde(...)]` attributes used by `Deserialize`: `rename`, `rename_all`,
/// `default`, and `deny_unknown_fields`. Unsupported Serde attributes are a
/// compile error rather than an approximate schema.
#[proc_macro_derive(Config, attributes(serde))]
pub fn derive_config(input: TokenStream) -> TokenStream {
    authoring::expand_config(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive the serialized wire shape a type puts on the wire, read from the same
/// `#[serde(...)]` model `Serialize` itself reads.
///
/// The declared shape is what compatibility CI compares against a published
/// baseline, so a serde feature whose serialized form this cannot compute -
/// `flatten`, adjacent tagging, an arbitrary `serialize_with` - is a compile
/// error pointing at a hand-written
/// `phoxal::__compat::wire::DescribeWire`, never an approximate
/// schema.
#[proc_macro_derive(DescribeWire, attributes(serde))]
pub fn derive_describe_wire(input: TokenStream) -> TokenStream {
    wire_schema::expand(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Declare a service marker's `Config`/`State`/`Api` types. Each omitted type
/// defaults to `()`; identity defaults from `CARGO_PKG_NAME`.
///
/// An explicit `id` is still required whenever a crate defines more than one
/// participant - they cannot all default to the one package name - and
/// remains available any time the package name isn't the id you want.
///
/// For user runtimes, `Config` is the user-authored `robot.yaml` surface -
/// `services.<id>.config` for a service, and the `config` half of a driven
/// instance's `driver:` block for a [`driver`]. A framework runtime may use this
/// same slot for a CLI-synthesized launch payload (for example a cross-robot
/// staging product); ordinary framework knobs belong in the robot model
/// received through `ctx.robot()`.
///
/// A service is not bound to a component, so it declares no `connection` and
/// reads none.
#[proc_macro_attribute]
pub fn service(attr: TokenStream, item: TokenStream) -> TokenStream {
    authoring::expand_participant(
        attr.into(),
        item.into(),
        authoring::ParticipantKind::Service,
    )
    .unwrap_or_else(syn::Error::into_compile_error)
    .into()
}

/// The driver-shaped counterpart to [`service`]: one process per driven
/// `robot.components` entry, launched under that entry's own id.
///
/// A driven instance's `driver:` block is two slots with one owner each:
///
/// ```yaml
/// driver:
///   connection: { type: serial, port: /dev/ttyUSB0, baud: 115200 }
///   config:     { id: 1 }
/// ```
///
/// `connection` is the framework's closed vocabulary and is read through
/// `ctx.connection()`; `config` is this binary's own, and is the only thing
/// deserialized into `config = …`.
///
/// `connection = <kind>` optionally declares the one kind this driver accepts,
/// spelled as the authored token (`can`, `i2c`, `spi`, `serial`, `uart`, `usb`,
/// `gpio`). Declaring it makes `ctx.connection()` return that kind's payload
/// struct directly, and the runner refuses any other authored kind before the
/// process reaches the bus. Omitting it means every kind is accepted and
/// `ctx.connection()` returns the whole `Connection` enum.
///
/// ```ignore
/// use phoxal::model::connection::Serial;
/// use phoxal::prelude::*;
///
/// #[derive(serde::Deserialize, phoxal::Config)]
/// struct WheelConfig {
///     /// This motor's address on the bus the port opens.
///     id: u8,
/// }
///
/// struct WheelState {
///     serial: Serial,
///     id: u8,
/// }
///
/// #[phoxal::driver(config = WheelConfig, connection = serial, state = WheelState)]
/// struct Wheel;
///
/// impl Participant for Wheel {
///     async fn setup(
///         &self,
///         ctx: &mut SetupContext<Self>,
///         config: Self::Config,
///     ) -> Result<(Self::State, Self::Api)> {
///         let serial = ctx.connection()?;
///         Ok((WheelState { serial, id: config.id }, ()))
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn driver(attr: TokenStream, item: TokenStream) -> TokenStream {
    authoring::expand_participant(attr.into(), item.into(), authoring::ParticipantKind::Driver)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Declare the one mandatory root brain: the robot project's composition root.
///
/// The brain is the root Cargo package's binary (`src/main.rs`). It is a
/// checked, clocked graph participant with exactly the typed-I/O and step
/// scheduling surface [`service`] has, and no privileged capability.
///
/// It differs from [`service`] in two fixed ways:
///
/// - its participant identity is always `brain`, so an `id = "…"` argument is
///   rejected - there is exactly one per robot project, and the CLI stages it
///   under the canonical `bin/brain`; and
/// - its `Config` is always `()`, so a `config = …` argument is rejected -
///   robot policy is ordinary Rust code compiled into this binary, not an
///   authored configuration side channel.
///
/// `state = …` and `api = …` work exactly as on every other checked role.
///
/// ```ignore
/// use phoxal::prelude::*;
///
/// #[phoxal::brain]
/// struct Brain;
///
/// impl Participant for Brain {
///     async fn setup(
///         &self,
///         _ctx: &mut SetupContext<Self>,
///         _config: Self::Config,
///     ) -> Result<(Self::State, Self::Api)> {
///         Ok(((), ()))
///     }
/// }
///
/// fn main() -> phoxal::Result<()> { phoxal::run::<Brain>() }
/// ```
#[proc_macro_attribute]
pub fn brain(attr: TokenStream, item: TokenStream) -> TokenStream {
    authoring::expand_participant(attr.into(), item.into(), authoring::ParticipantKind::Brain)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
