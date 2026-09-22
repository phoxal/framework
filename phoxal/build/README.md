# phoxal-build

Build-time support for service-owned Protobuf contracts.

The crate packages the `phoxal/api.proto` modifiers and generates ordinary Prost messages plus inert typed call and observation descriptors from owned Protobuf service declarations.

Contract imports are supplied as the exact `FileDescriptorSet` exported by direct Cargo build dependencies and mapped with Prost `extern_path` entries.
The generator does not locate or compile dependency-owned source trees.
It validates dependency descriptor pools independently, accepts identical transitive diamonds, and rejects conflicting file paths or fully qualified symbols before invoking Protobuf generation.

It is a build dependency only.
It does not read a robot project, start a runtime, or perform transport I/O.
