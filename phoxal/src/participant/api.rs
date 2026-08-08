//! The participant authoring model (`ParticipantSpec` + `Participant`).
//! Role attributes declare static identity and `Config`/`State`/`Api` types;
//! authors implement lifecycle behavior directly.
//!
//! `Api` names a participant-authored struct of bus handles. Every field comes
//! from the one train-selected `phoxal::api` facade, which is what makes
//! [`ParticipantSpec::ContractApi`] a true statement about the process.
//!
//! The runner's own infrastructure contracts (liveliness, bus logs, simulation
//! clock) resolve independently of any participant's `Api`:
//! `participant::runner` names `crate::api` directly.
//!
//! Setup capabilities are role-gated by the marker traits in
//! [`surface`](crate::participant::surface): a driver or simulator resolves a
//! bound component, and a simulator alone reaches world-clock authority.

use std::future::Future;
use std::marker::PhantomData;
use std::ops::AsyncFn;
use std::pin::Pin;

use crate::bus::{Codec, ContractBody, MessagePack, QueryFailure};
use crate::participant::context::{ResetContext, SetupContext, StepContext};
use crate::participant::scheduler::StepSchedule;

/// Const-eval plumbing the participant attribute macros
/// (`phoxal-macros/src/authoring.rs`'s `expand_participant`) use to build the
/// binary's embedded linker-section metadata static:
/// `{"schema", "api", "schemas", "id", "kind", "config_schema"}`.
///
/// The problem this solves: a config schema is composed recursively from
/// nested `ParticipantConfig` impls (`Self::SCHEMA_JSON`, itself built the
/// same way), so the final JSON string is only known after `rustc` const-evals
/// the whole tree in the downstream participant crate - a proc-macro cannot
/// pre-resolve it into a literal. So the participant attribute emits
/// **tokens**, not a string: a call into [`meta::concatcp`] splicing the
/// participant id and `<Config as ParticipantConfig>::SCHEMA_JSON` between
/// macro-time-known JSON literal fragments, which `rustc` const-evaluates in
/// the participant crate. [`meta::bytes_of`] then copies the final string into the
/// fixed byte array placed in the linker section.
#[doc(hidden)]
pub mod meta {
    /// `const_format::concatcp!`, made reachable as
    /// `$crate::__private::meta::concatcp!`.
    ///
    /// The role attributes expand inside a participant's own crate, which does
    /// not depend on `const_format`, so the expansion cannot name that crate
    /// directly. Routing the call through here is what makes it hygienic: it
    /// resolves in the participant crate no matter what is in scope there.
    /// That obligation is the only reason this is public, and it is why the
    /// item cannot be made private or removed while the role attributes exist.
    #[doc(hidden)]
    pub use const_format::concatcp;

    /// Fixed-capacity const-eval string builder used for recursively composed
    /// config schemas. A fixed backing array is necessary because stable Rust
    /// cannot express an array length computed from a generic associated
    /// constant; only the used prefix is exposed by [`ConstSchema::as_str`].
    #[derive(Clone, Copy)]
    pub struct ConstSchema {
        bytes: [u8; 65_536],
        len: usize,
    }

    impl Default for ConstSchema {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ConstSchema {
        pub const fn new() -> Self {
            Self {
                bytes: [0; 65_536],
                len: 0,
            }
        }

        pub const fn from_str(value: &str) -> Self {
            Self::new().push_str(value)
        }

        #[must_use]
        pub const fn push_str(mut self, value: &str) -> Self {
            let value = value.as_bytes();
            assert!(
                self.len + value.len() <= self.bytes.len(),
                "phoxal: const config schema exceeds 64 KiB"
            );
            let mut index = 0;
            while index < value.len() {
                self.bytes[self.len + index] = value[index];
                index += 1;
            }
            self.len += value.len();
            self
        }

        pub const fn as_str(&self) -> &str {
            let (used, _) = self.bytes.split_at(self.len);
            // Every byte originates in a Rust `&str`, so concatenation
            // preserves UTF-8 validity.
            unsafe { core::str::from_utf8_unchecked(used) }
        }
    }

    /// Copies a `rustc`-const-evaluated `&str` into a fixed `[u8; N]` array so
    /// it can be assigned to a `#[link_section]` static (which must be a
    /// plain byte-sized value, not a fat `&str` pointer/len pair). Callers
    /// are expected to pass `N` implicitly via the assignment's expected
    /// array type (`static X: [u8; LEN] = bytes_of(S);` with `const LEN:
    /// usize = S.len();`); the `assert!` is a defense-in-depth check against
    /// that inference ever mismatching, not the primary correctness
    /// mechanism.
    pub const fn bytes_of<const N: usize>(s: &str) -> [u8; N] {
        let bytes = s.as_bytes();
        assert!(
            bytes.len() == N,
            "phoxal: metadata manifest length mismatch"
        );
        let mut out = [0u8; N];
        let mut i = 0;
        while i < N {
            out[i] = bytes[i];
            i += 1;
        }
        out
    }
}

/// Emitted by `#[derive(phoxal::Config)]`: the participant config's compile-time
/// JSON Schema (Draft 2020-12).
pub trait ParticipantConfig: serde::de::DeserializeOwned + Send + 'static {
    #[doc(hidden)]
    const __SCHEMA: meta::ConstSchema;
    /// A complete schema or subschema, const-composable by another derived
    /// config without runtime allocation.
    const SCHEMA_JSON: &'static str = Self::__SCHEMA.as_str();
}

impl ParticipantConfig for () {
    const __SCHEMA: meta::ConstSchema = meta::ConstSchema::from_str(r#"{"type":"null"}"#);
}

/// An optional config is a config: `config = Option<T>` (a participant whose
/// `PHOXAL_CONFIG` may be absent, deserializing to `None`) works whenever
/// `T: ParticipantConfig`; `Option<T>`'s own `Deserialize` maps `null`/absent
/// to `None` and a present object to `Some(T)`. A participant crate cannot
/// write `impl ParticipantConfig for Option<LocalConfig>` itself (orphan
/// rule: both the trait and `Option` are foreign), so the blanket lives here.
impl<T: ParticipantConfig> ParticipantConfig for Option<T> {
    const __SCHEMA: meta::ConstSchema = meta::ConstSchema::new()
        .push_str(r#"{"anyOf":["#)
        .push_str(T::SCHEMA_JSON)
        .push_str(r#",{"type":"null"}]}"#);
}

impl<T: ParticipantConfig> ParticipantConfig for Vec<T> {
    const __SCHEMA: meta::ConstSchema = meta::ConstSchema::new()
        .push_str(r#"{"type":"array","items":"#)
        .push_str(T::SCHEMA_JSON)
        .push_str("}");
}

impl<T: ParticipantConfig> ParticipantConfig for std::collections::BTreeMap<String, T> {
    const __SCHEMA: meta::ConstSchema = meta::ConstSchema::new()
        .push_str(r#"{"type":"object","additionalProperties":"#)
        .push_str(T::SCHEMA_JSON)
        .push_str("}");
}

impl<T: ParticipantConfig> ParticipantConfig for std::collections::HashMap<String, T> {
    const __SCHEMA: meta::ConstSchema = meta::ConstSchema::new()
        .push_str(r#"{"type":"object","additionalProperties":"#)
        .push_str(T::SCHEMA_JSON)
        .push_str("}");
}

macro_rules! primitive_config_schema {
    ($ty:ty => $schema:literal) => {
        impl ParticipantConfig for $ty {
            const __SCHEMA: meta::ConstSchema = meta::ConstSchema::from_str($schema);
        }
    };
}

primitive_config_schema!(bool => r#"{"type":"boolean"}"#);
primitive_config_schema!(String => r#"{"type":"string"}"#);
primitive_config_schema!(char => r#"{"type":"string","minLength":1,"maxLength":1}"#);
primitive_config_schema!(i8 => r#"{"type":"integer","format":"int8"}"#);
primitive_config_schema!(i16 => r#"{"type":"integer","format":"int16"}"#);
primitive_config_schema!(i32 => r#"{"type":"integer","format":"int32"}"#);
primitive_config_schema!(i64 => r#"{"type":"integer","format":"int64"}"#);
primitive_config_schema!(i128 => r#"{"type":"integer"}"#);
primitive_config_schema!(isize => r#"{"type":"integer"}"#);
primitive_config_schema!(u8 => r#"{"type":"integer","format":"uint8","minimum":0,"maximum":255}"#);
primitive_config_schema!(u16 => r#"{"type":"integer","format":"uint16","minimum":0,"maximum":65535}"#);
primitive_config_schema!(u32 => r#"{"type":"integer","format":"uint32","minimum":0}"#);
primitive_config_schema!(u64 => r#"{"type":"integer","format":"uint64","minimum":0}"#);
primitive_config_schema!(u128 => r#"{"type":"integer","minimum":0}"#);
primitive_config_schema!(usize => r#"{"type":"integer","minimum":0}"#);
primitive_config_schema!(f32 => r#"{"type":"number","format":"float"}"#);
primitive_config_schema!(f64 => r#"{"type":"number","format":"double"}"#);

/// Static participant identity and associated types, emitted by a role
/// attribute on a unit marker.
#[doc(hidden)]
pub trait ParticipantSpec: Sized + Send + Sync + 'static {
    /// The authoring kind that produced this artifact, as the framework-owned
    /// value the embedded metadata record declares.
    const KIND: phoxal_runtime_contract::metadata::ParticipantKind;
    /// The participant id (`id = "…"`, default derived from the crate's
    /// `CARGO_PKG_NAME`; see `#[phoxal::service]`'s docs).
    const ID: &'static str;
    /// The process launch contract. Simulators use a clockless policy;
    /// services and drivers use the configurable robot-clock policy.
    #[doc(hidden)]
    type LaunchPolicy: crate::participant::launch::ParticipantLaunchPolicy;
    /// The single API revision every typed handle this participant builds must
    /// come from. Role attributes fix it to the train-selected facade
    /// (`phoxal::api::Api`); there is no participant-local choice.
    ///
    /// Every [`SetupContext`](crate::SetupContext) builder is bounded on it, so
    /// one participant physically cannot construct handles from two API
    /// revisions - which is what makes the `api` field of its embedded
    /// `.phoxal_meta` record a true statement about the process rather than a
    /// declaration nothing checks.
    #[doc(hidden)]
    type ContractApi: crate::bus::ApiVersion;
    /// The participant's typed config (`robot.yaml` input).
    type Config: ParticipantConfig;
    /// Mutable runtime state, owned only by the serialized event loop.
    type State: Send + 'static;
    /// Bus-facing handles used by lifecycle behavior.
    type Api: Send + 'static;

    /// Construct the role marker. Role attributes accept unit structs only.
    #[doc(hidden)]
    fn __new() -> Self;

    /// Read a byte out of this participant's embedded `.phoxal_meta` /
    /// `__DATA,__phoxal_meta` metadata static, so it is reachable from the
    /// process entry point.
    ///
    /// On ELF (Linux), `--gc-sections` drops any section unreachable from
    /// `main` at final link time - even one carrying `#[used]`, which only
    /// protects the static from *this compilation unit's* own dead-code
    /// elimination, not from the linker's whole-program reachability pass
    /// (`#[used(linker)]` would say "keep this through the linker" directly,
    /// but is unstable: rust-lang/rust#93798). `phoxal::run`/`run_async` call
    /// this once before doing anything else, which makes the static
    /// genuinely reachable and defeats that GC - without a linker flag,
    /// `RUSTFLAGS`, or a build script, none of which reach a plain `cargo
    /// install <pkg> --registry phoxal` run in an arbitrary environment.
    /// Mach-O (macOS) never drops the section in the first place, so this is
    /// a harmless extra read there - one code path, not gated on
    /// `target_os`. `#[doc(hidden)]`: generated by
    /// `#[phoxal::service]`/`driver`/`simulator`/`tool`, never called
    /// directly.
    #[doc(hidden)]
    fn __retain_embedded_metadata();
}

/// Participant lifecycle behavior.
///
/// One runner task owns `State` and serializes step, query, reset, and
/// shutdown access. `Api` is separate and shared immutably with behavior.
#[expect(
    async_fn_in_trait,
    reason = "the runner awaits these directly and is the only caller, so the trait needs no `Send` bound and boxing the futures would be pure overhead"
)]
pub trait Participant: ParticipantSpec {
    /// Build initial mutable state and bus-facing handles.
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        config: Self::Config,
    ) -> crate::Result<(Self::State, Self::Api)>;

    /// Run one scheduled step.
    async fn step(
        &self,
        _api: &Self::Api,
        _step: StepContext,
        _state: &mut Self::State,
    ) -> crate::Result<()> {
        Ok(())
    }

    /// Reset state derived from a replaced simulation timeline.
    async fn reset(
        &self,
        _ctx: ResetContext,
        _api: &Self::Api,
        _state: &mut Self::State,
    ) -> crate::Result<()> {
        Ok(())
    }

    /// Gracefully park, stop, flush, or publish a final externally observable
    /// action before the bus closes.
    ///
    /// Override this only when teardown has an effect outside the state that
    /// the runner is about to drop. Ordinary participants omit it. The runner
    /// bounds the hook with the launch contract's shutdown grace period.
    async fn shutdown(&self, _api: &Self::Api, _state: &mut Self::State) -> crate::Result<()> {
        Ok(())
    }

    /// Cadence emitted alongside a `#[phoxal::step(hz = N)]` override.
    #[doc(hidden)]
    fn __step_schedule() -> Option<StepSchedule> {
        None
    }
}

/// A successful server reply: the encoded plain `Resp` body.
///
/// The reply carries no contract identity of its own, because the request
/// already arrived on `Resp`'s version-qualified topic key - a receiver that
/// got the reply knows what it asked for.
#[derive(Debug)]
pub(crate) struct ServerReply {
    /// MessagePack-encoded `Resp` body.
    pub payload: Vec<u8>,
}

/// What a query dispatcher returns: a [`ServerReply`] or a structured
/// [`QueryFailure`].
pub(crate) type ServerOutcome = std::result::Result<ServerReply, QueryFailure>;

/// One setup-time query binding, type-erased only after its request/response
/// types and handler have been checked at the `ctx.query(...)` call.
///
/// `topic` is a plain `String` rather than a typed
/// [`Topic`](crate::bus::Topic): erasure is the point of this type, and a
/// `Topic<ServeQuery<Req, Resp>>` still names `Req` and `Resp`, so it cannot
/// survive into the erased registration list the runner iterates. The key
/// string is all that is left to match an incoming query against, and it was
/// produced by the typed builder at the checked `ctx.query(...)` call site.
pub(crate) struct QueryRegistration<R: Participant> {
    topic: String,
    handler: Box<dyn ErasedQueryHandler<R>>,
}

impl<R: Participant> QueryRegistration<R> {
    pub(crate) fn new<Req, Resp, H>(topic: String, handler: H) -> Self
    where
        Req: ContractBody,
        Resp: ContractBody,
        H: for<'a> AsyncFn(
                &'a R,
                &'a R::Api,
                Req,
                &'a mut R::State,
            ) -> crate::bus::QueryResult<Resp>
            + Send
            + Sync
            + 'static,
    {
        Self {
            topic,
            handler: Box::new(TypedQueryHandler::<H, Req, Resp> {
                handler,
                _types: PhantomData,
            }),
        }
    }

    pub(crate) fn topic(&self) -> &str {
        &self.topic
    }

    pub(crate) async fn dispatch(
        &self,
        participant: &R,
        api: &R::Api,
        state: &mut R::State,
        request: Vec<u8>,
    ) -> ServerOutcome {
        self.handler
            .dispatch(participant, api, state, request)
            .await
    }
}

trait ErasedQueryHandler<R: Participant>: Send + Sync {
    fn dispatch<'a>(
        &'a self,
        participant: &'a R,
        api: &'a R::Api,
        state: &'a mut R::State,
        request: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = ServerOutcome> + 'a>>;
}

struct TypedQueryHandler<H, Req, Resp> {
    handler: H,
    _types: PhantomData<fn(Req) -> Resp>,
}

impl<R, H, Req, Resp> ErasedQueryHandler<R> for TypedQueryHandler<H, Req, Resp>
where
    R: Participant,
    Req: ContractBody,
    Resp: ContractBody,
    H: for<'a> AsyncFn(&'a R, &'a R::Api, Req, &'a mut R::State) -> crate::bus::QueryResult<Resp>
        + Send
        + Sync
        + 'static,
{
    fn dispatch<'a>(
        &'a self,
        participant: &'a R,
        api: &'a R::Api,
        state: &'a mut R::State,
        request: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = ServerOutcome> + 'a>> {
        Box::pin(async move {
            let request = MessagePack::decode::<Req>(&request).map_err(|error| {
                QueryFailure::invalid_argument(format!(
                    "decode query request for '{}': {error}",
                    Req::TOPIC
                ))
            })?;
            let response = (self.handler)(participant, api, request, state).await?;
            let payload = MessagePack::encode(&response).map_err(|error| {
                QueryFailure::internal(format!(
                    "encode query response for '{}': {error}",
                    Resp::TOPIC
                ))
            })?;
            Ok(ServerReply { payload })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{ParticipantConfig, QueryRegistration, meta::ConstSchema};
    use crate::api;
    use crate::bus::{Codec, MessagePack, QueryCode, QueryFailure};
    use crate::prelude::*;

    #[test]
    fn const_schema_exposes_only_the_pushed_prefix() {
        let schema = ConstSchema::from_str(r#"{"type":"#)
            .push_str(r#""string""#)
            .push_str("}");
        assert_eq!(schema.as_str(), r#"{"type":"string"}"#);
        assert_eq!(ConstSchema::new().as_str(), "");
    }

    /// The blanket impls this module owns compose a nested schema without
    /// allocating, so a config type never has to spell its own container
    /// schemas.
    #[test]
    fn blanket_config_schemas_compose_from_the_inner_schema() {
        assert_eq!(<() as ParticipantConfig>::SCHEMA_JSON, r#"{"type":"null"}"#);
        assert_eq!(
            <Option<bool> as ParticipantConfig>::SCHEMA_JSON,
            r#"{"anyOf":[{"type":"boolean"},{"type":"null"}]}"#
        );
        assert_eq!(
            <Vec<String> as ParticipantConfig>::SCHEMA_JSON,
            r#"{"type":"array","items":{"type":"string"}}"#
        );
        assert_eq!(
            <std::collections::BTreeMap<String, u32> as ParticipantConfig>::SCHEMA_JSON,
            r#"{"type":"object","additionalProperties":{"type":"integer","format":"uint32","minimum":0}}"#
        );
    }

    struct Api;

    #[derive(Default)]
    struct QueryState {
        calls: Vec<String>,
    }

    #[phoxal::service(id = "query-test", state = QueryState, api = Api)]
    struct QueryParticipant;

    impl Participant for QueryParticipant {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> Result<(Self::State, Self::Api)> {
            Ok((QueryState::default(), Api))
        }
    }

    impl QueryParticipant {
        async fn get(
            &self,
            _api: &Api,
            request: api::supervisor::asset::GetRequest,
            state: &mut QueryState,
        ) -> QueryResult<api::supervisor::asset::GetResponse> {
            state.calls.push(request.path.clone());
            if request.path == "ok" {
                Ok(api::supervisor::asset::GetResponse::Found {
                    bytes: vec![1, 2, 3],
                })
            } else {
                Err(QueryFailure::not_found("no such asset"))
            }
        }
    }

    #[tokio::test]
    async fn typed_query_dispatch_decodes_mutates_and_encodes() {
        let registration = QueryRegistration::new(
            "v0.1/supervisor/asset/get".to_string(),
            QueryParticipant::get,
        );
        let participant = QueryParticipant;
        let api = Api;
        let mut state = QueryState::default();

        let first = MessagePack::encode(&api::supervisor::asset::GetRequest {
            path: "ok".to_string(),
        })
        .unwrap();
        let reply = registration
            .dispatch(&participant, &api, &mut state, first)
            .await
            .unwrap();
        let response: api::supervisor::asset::GetResponse =
            MessagePack::decode(&reply.payload).unwrap();
        assert!(matches!(
            response,
            api::supervisor::asset::GetResponse::Found { .. }
        ));

        let second = MessagePack::encode(&api::supervisor::asset::GetRequest {
            path: "missing".to_string(),
        })
        .unwrap();
        let failure = registration
            .dispatch(&participant, &api, &mut state, second)
            .await
            .unwrap_err();
        assert_eq!(failure.code, QueryCode::NotFound);
        assert_eq!(state.calls, ["ok", "missing"]);
    }
}
