# phoxal-manifest

Compiler for authored Phoxal project manifests. It parses project source files,
validates and resolves them, and produces a canonical `phoxal-model` robot plus
participant and asset declarations for tooling such as `phoxal-cli`.

`schema::generate` generates Draft 2020-12 editor schemas from the exact authored
serde DTOs. They provide portable YAML completion and inspection only;
`phoxal validate` remains authoritative for strict YAML, semantic, cross-file,
and project-resolution validation.
