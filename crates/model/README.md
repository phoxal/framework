# phoxal-model

The canonical, validated Phoxal robot model.
Authored project documents belong to `phoxal-manifest`; persisted runtime
documents and their loader belong to `phoxal-bundle`. This crate contains only
the runtime semantic types and logical asset identities.

The model owns no bundle filesystem layout or participant asset resolver.
`phoxal-bundle` owns the schema-tagged runtime wire form and digest-checked
access to `<bundle>/assets`.
