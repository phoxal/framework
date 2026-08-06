# phoxal-model

The canonical, validated Phoxal robot model.
Authored and finalized project documents belong to `phoxal-manifest`, which is
also where the finalized-bundle loader that builds a `Robot` lives; this crate
contains only the runtime semantic types.

The model has no persisted wire form: there is exactly one representation of a
robot, built in memory from the finalized bundle's documents.
`ParticipantAssetResolver` is the participant-facing fence over
`<bundle>/assets`.
