# Phoxal API Contracts

Phoxal API modules are domain-first. Public topic payloads live at paths such as
`phoxal::api::drive::Target`, with the exact versioned wire data under
`phoxal::api::drive::v1::Target`.

The contract enum is the wire body and the single version authority. Declare it
with the `contract!` macro (defined in `src/api/mod.rs`), which generates the
derives, the serde framing, and a uniform wire-equality `PartialEq`:

```rust
contract! {
    pub enum Target {
        V1(v1::Target),
    }
}
```

The macro expands to a `#[serde(tag = "v", content = "data", rename_all =
"lowercase")]` enum. The **wire version token is the lowercased variant name**
(`V1` → `"v1"`), so a contract declares only its variants — there is no separate
token to keep in sync. The serialized payload is the tagged shape, for example
`{"v":"v1","data":{...}}`. Because the token is derived from the variant name,
**never rename an existing variant** — that would change the wire (see the
append-only rule below).

Topic keys and schemas are versionless. Zenoh encoding carries the schema family
name from `topic.schema()` and guards against publishing or decoding a different
contract family on the same key. Per-version compatibility is expressed only by
the contract enum variant.

Variants are append-only. Never reorder, remove, **rename**, or otherwise retag
an existing variant. Retired variants stay in the enum, marked deprecated, so old
recordings still decode. Add a new variant (`V2`, `V3`, …) for any change that is
not an additive, ignorable field with a semantic default. Unknown newer variants
fail loudly during decode; mixed-version compatibility policy is a separate
post-stable-v1 concern.
