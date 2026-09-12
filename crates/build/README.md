# phoxal-build

Build-time support for service-owned Protobuf contracts.

The crate packages the shared `phoxal/port.proto` method option and generates
ordinary Prost messages plus inert typed `phoxal-port` constants from owned
Protobuf service declarations.

It is a build dependency only.
It does not read a robot project, start a runtime, or perform transport I/O.
