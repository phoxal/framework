//! Runtime artifact record family.
//!
//! Owns the inert contract records (`RuntimeRecord`, `InputRecord`,
//! `OutputRecord`, `PortKind`, `InputKind`, `OutputKind`, `PortSignature`)
//! and manifest-safe summaries (`ArtifactSummary`, `DescriptorSummary`).
//!
//! Native binary inspection (`ArtifactContract`, `DescriptorInfo`,
//! `inspect_file`, `inspect_bytes`) and connection validation remain in
//! `phoxal-project`'s tool layer.
