# phoxal-runtime-contract

Shared, transport-neutral process-boundary contracts for the Phoxal runtime.
Application participants normally use these contracts through the `phoxal` facade.

There is no crate-root facade here: each contract is a module and each public type has exactly one path.

- `identity` - the execution, participant, producer, and timeline identities that reach process boundaries.
- `version` - the framework train version two binaries compare to establish that they speak the same contracts.
- `metadata` - the record every participant binary embeds at compile time, and its strict parser.
- `emit` - the one sanctioned writer of that record, in both of its evaluation modes.
- `origin` - the boot-anchored origin of one real execution.
- `rendezvous` - shared execution-root mapping, the supervisor socket and
  lifetime lock, and advisory lock operations used by both sides of that host
  process boundary.

The framework train version is the one compatibility identity. Each participant
embeds its exact train in `ParticipantMetadata`, and the supervisor reports its
exact train through `supervisor/connect`. Two binaries speak the same contracts
when those versions share a compatibility line.
There is no Cargo package-metadata table and no version file anywhere in the contract.
The `schema` tag on a persisted document is a format discriminator, not a second compatibility identity.

`ParticipantId` is the validated topology role selected from a persisted
`runtime.json`; it is distinct from `ProducerId`, which is minted by the
transport for one process-session incarnation.
