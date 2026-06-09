# Framework docs

Workspace-level design docs for `phoxal/framework`. Per-crate detail lives in each
crate's own `README.md`; org-level vision, architecture, and process live in
[`phoxal/organization`](https://github.com/phoxal/organization).

- [CONTRACTS.md](./CONTRACTS.md) — the cross-cutting contract discipline every
  `phoxal::api::<name>` contract module follows (envelope/timestamp, query
  results, typed reasons, revisions, schema identity, large products, API
  evolution).
- [CONVENTIONS.md](./CONVENTIONS.md) — bus, topic, logical-time, runtime
  bootstrap, and component conventions inside this workspace.
- [VALIDATION.md](./VALIDATION.md) — validation layers, scenario surfaces, tiers,
  and the delivery-phase gates.

Org-level context:
[VISION](https://github.com/phoxal/organization/blob/master/docs/product/VISION.md),
[ARCHITECTURE](https://github.com/phoxal/organization/blob/master/docs/product/ARCHITECTURE.md),
[BLUEPRINT](https://github.com/phoxal/organization/blob/master/docs/product/BLUEPRINT.md),
and the [AI assistant guide](https://github.com/phoxal/organization/blob/master/docs/AI_ASSISTANT_GUIDE.md).
