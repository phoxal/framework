# phoxal-runtime-contract

Shared, transport-neutral process-boundary contracts for the Phoxal runtime.
Application participants normally use these contracts through the `phoxal` facade.

There is no crate-root facade here: each contract is a module and each public type has exactly one path.

- `identity` - the execution, participant, producer, and timeline identities that reach process boundaries.
- `version` - the version identities two binaries compare to establish that they speak the same contracts.
- `metadata` - the record every participant binary embeds at compile time, and its strict parser.
- `emit` - the one sanctioned writer of that record, in both of its evaluation modes.
- `launch` - the launch record and environment ABI a supervisor hands a participant process.
- `origin` - the boot-anchored origin of one real execution.

Compatibility between a `phoxal-cli` and a built participant is declared in one place: the `ParticipantMetadata` document each binary embeds at compile time, carrying the API revision and the five document schemas it speaks (bus, launch, robot, component, simulation).
There is no Cargo package-metadata table, no version file, and no framework-SemVer floor anywhere in the contract.

`ParticipantId` is the validated topology role selected from a persisted
`runtime.json`; it is distinct from `ProducerId`, which is minted by the
transport for one process-session incarnation.
