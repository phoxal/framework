# Phoxal API Contracts

Phoxal API modules are domain-first. Public topic payloads live at paths such as
`phoxal::api::drive::Target`, with the exact versioned wire data under
`phoxal::api::drive::v1::Target`.

The contract enum is the wire body and the single version authority:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "v", content = "data")]
pub enum Target {
    #[serde(rename = "1")]
    V1(v1::Target),
}
```

The serialized payload is the tagged enum shape, for example
`{"v":"1","data":{...}}`. The `serde(rename = "1")` token is the wire
version authority. The Rust variant name is only an identifier and may not be
used as the compatibility signal.

Topic keys and schemas are versionless. Zenoh encoding carries the schema family
name from `topic.schema()` and guards against publishing or decoding a different
contract family on the same key. Per-version compatibility is expressed only by
the contract enum variant.

Variants are append-only. Never reorder, remove, or retag an existing variant.
Retired variants stay in the enum, marked deprecated, so old recordings still
decode. Add a new variant for any change that is not an additive, ignorable
field with a semantic default. Unknown newer variants fail loudly during decode;
mixed-version compatibility policy is a separate post-stable-v1 concern.
