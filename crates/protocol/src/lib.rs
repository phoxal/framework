//! The framework-owned wire-contract catalogue: every payload that crosses a
//! Phoxal process boundary and the semantic endpoint that carries it, grouped
//! into families.
//!
//! Payload structs, enums, implementations, and tests are ordinary Rust items
//! in family-first modules. One `protocol_tree!` catalogue declares their
//! endpoints and generates deterministic descriptors and typed topic builders.
//! Payloads own serde shape, construction invariants, and domain behavior; they
//! do not know their topic or delivery policy.
//!
//! A **family** is the first path segment of every catalogue path and the
//! leading segment of every key it declares: it names a semantic namespace,
//! not a revision. There are three:
//!
//! - [`robot`] - the robot domain a participant authors against. `phoxal::api`
//!   re-exports this family and only this one.
//! - [`runtime`] - facts a running Phoxal process emits about itself: its log
//!   events, its bus and step telemetry, and the authoritative simulation
//!   clock. Any process publishes here; the family names no collector.
//! - [`supervisor`] - the wire vocabulary a supervisor speaks. The
//!   framework-owned `phoxal-supervisor` process owns supervisor state and
//!   behavior; this crate owns only what an answer looks like.
//!
//! Compatibility is owned entirely by the framework train version each
//! participant binary embeds, compared by the compatibility line the two
//! trains belong to, so no key, descriptor, or body carries a per-API version.
//! The one exception is [`supervisor::connect`], the frozen bootstrap two
//! binaries exchange before they know whether their trains agree; it reports
//! the exact version, and the reader decides.
//!
//! Endpoint semantics are fixed by the declaration: `State`, `Sample`, `Event`,
//! `Stream`, `Setpoint`, or bounded query. Source identity, robot/capture time,
//! ordered positions, loss, gaps, and terminal evidence remain bus metadata and
//! never become generated fields in a domain payload.

phoxal_macros::protocol_tree! {
    path robot / component(instance) / accelerometer(capability);

    sample: Sample<Sample>;

    path robot / component(instance) / battery(capability);

    state: State<State>;

    path robot / component(instance) / camera(capability);

    frame: Sample<Frame>;

    path robot / component(instance) / depth(capability);

    frame: Sample<Frame>;

    path robot / component(instance) / emergency_stop(capability);

    state: State<State>;

    path robot / component(instance) / encoder(capability);

    sample: Sample<Sample>;

    path robot / component(instance) / gnss(capability);

    sample: Sample<Sample>;

    path robot / component(instance) / gyroscope(capability);

    sample: Sample<Sample>;

    path robot / component(instance) / imu(capability);

    sample: Sample<Sample>;

    path robot / component(instance) / led(capability);

    command: Setpoint<Command>;

    path robot / component(instance) / lidar(capability);

    scan: Sample<Scan>;

    path robot / component(instance) / magnetometer(capability);

    sample: Sample<Sample>;

    path robot / component(instance) / microphone(capability);

    frame: Sample<Frame>;

    path robot / component(instance) / mmwave(capability);

    scan: Sample<Scan>;

    path robot / component(instance) / motor(capability);

    command: Setpoint<Command>;

    path robot / component(instance) / range(capability);

    sample: Sample<Sample>;

    path robot / component(instance) / speaker(capability);

    stream: Stream<Chunk, In>;

    path robot / drive;

    target: Setpoint<Target>;
    state: State<State>;

    path robot / frame;

    tree: State<Tree>;
    static_transforms: State<StaticTransforms>;
    lookup: Query<LookupRequest, LookupResponse>;

    path robot / joint(joint);

    state: Event<JointState>;

    path robot / localize;

    state: State<LocalizationState>;

    path robot / map;

    revision: State<Revision>;
    submap: Query<SubmapRequest, SubmapResponse>;

    path robot / motion;

    manual: Setpoint<ManualCommand>;
    state: State<State>;

    path robot / navigation;

    state: State<State>;
    progress: State<Progress>;
    result: Event<Result>;
    candidate: State<Candidate>;
    start: Query<StartRequest, StartResponse>;
    cancel: Query<CancelRequest, CancelResponse>;
    next_frontier: Query<FrontierRequest, FrontierResponse>;

    path robot / odometry;

    state: State<State>;

    path robot / perception;

    detections: State<Detections>;
    state: State<State>;

    path robot / safety;

    constraints: State<MotionConstraints>;
    state: State<State>;

    path robot / video;

    open: Query<OpenRequest, OpenOutcome>;

    path runtime / logs;

    self: Stream<Event, Out>;

    path runtime / simulation;

    clock: Event<Clock>;

    path runtime / telemetry;

    self: Stream<Rollup, Out>;

    path supervisor / bundle;

    get: Query<GetRequest, GetResponse>;

    path supervisor / command;

    self: Query<Request, Reply>;

    path supervisor / connect;

    self: Query<ConnectRequest, ConnectReply>;

    path supervisor / logs;

    snapshot: Query<SnapshotRequest, Snapshot>;
    follow: Stream<Follow, Out>;

    path supervisor / snapshot;

    self: Stream<Update, Out>;
    current: Query<CurrentRequest, Current>;

    path supervisor / telemetry;

    snapshot: Query<SnapshotRequest, Snapshot>;
    follow: Stream<Follow, Out>;

    path supervisor / execution;
}
