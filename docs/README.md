# Framework docs

Workspace-level design docs for `phoxal/framework`. Per-crate detail lives in each
crate's own `README.md`; org-level vision, architecture, and process live in
[`phoxal/organization`](https://github.com/phoxal/organization).

- [CONTRACTS.md](./CONTRACTS.md) - the cross-cutting contract discipline the
  `phoxal_api` tree follows (one API version per graph, plain wire bodies with
  metadata-carried produce time, typed reasons, query results, revision linkage,
  large products, API evolution).
- [CONVENTIONS.md](./CONVENTIONS.md) - bus, topic, logical-time, runtime
  authoring, and component conventions inside this workspace.
- [VALIDATION.md](./VALIDATION.md) - validation layers, scenario surfaces, tiers,
  and the delivery-phase gates.

Org-level context:
[VISION](https://github.com/phoxal/organization/blob/master/docs/product/VISION.md),
[ARCHITECTURE](https://github.com/phoxal/organization/blob/master/docs/product/ARCHITECTURE.md),
[BLUEPRINT](https://github.com/phoxal/organization/blob/master/docs/product/BLUEPRINT.md),
and the [AI assistant guide](https://github.com/phoxal/organization/blob/master/docs/AI_ASSISTANT_GUIDE.md).
