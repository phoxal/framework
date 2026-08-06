# phoxal-runtime-contract

Shared, transport-neutral process-boundary contracts for the Phoxal runtime.
This crate owns participant launch data, embedded metadata, identities, and
execution-origin types. Application participants normally use these contracts
through the `phoxal` facade.

Compatibility between a `phoxal-cli` and a built participant is declared in one
place: the `ParticipantMetadata` document each binary embeds at compile time,
carrying the API revision and the five document schemas it speaks (bus, launch,
robot, component, simulation). There is no Cargo package-metadata table, no
version file, and no framework-SemVer floor anywhere in the contract. The
authoritative identifier constants live in the crate that owns each contract
and are re-exported together as `phoxal::__private::compatibility`.
