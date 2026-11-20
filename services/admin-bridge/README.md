# services/admin-bridge (`cfmeet.admin-bridge`)

The only doorway from the CosmicForge central admin (HQ) into CF Meet
([ADR 0068](../../docs/decisions/0068-hq-managed-admin-bridge.md),
[design](../../docs/admin/cfmeet-admin-bridge.md)).

**Phase A0 (this commit): contract only.** No server, no database, nothing deployed.

## Contract HQ pins

`docs/contract/` is generated from the capability catalog (`src/contract/catalog.rs`), the
single source of truth for the future runtime route table. Never edit those files by hand.

```bash
cargo run --bin admin-bridge-contract -- generate               # rewrite docs/contract/
cargo run --bin admin-bridge-contract -- check                  # drift check (CI)
cargo run --bin admin-bridge-contract -- compat --base <dir>    # breaking-change gate vs main (CI)
cargo test                                                      # contract rules, compat gate, test vectors
```

| File | Content |
|---|---|
| `openapi.json` | Every route; `operationId` = capability id |
| `CFMEET_ADMIN_CAPABILITY_MATRIX.json` | One entry per capability, the 17 agreed fields |
| `capabilities.v1.json` | Karryam-shape registry HQ's connector loads (query shapes, path params, roles, purposes, phase) |
| `bridge-schemas.v1.json`, `schemas/` | Closed result schemas (HQ's strict validator subset) |
| `access-policy.v1.json` | Ceiling of the signed runtime access policy: role → permission → purposes |
| `datasets.v1.json` | The 8 HQ datasets |
| `event-adapters.v1.json` | The 13 owner event adapters (fields from phase A7) |
| `fixtures/` | HMAC v2, request digest, staff assertion and approval test vectors, produced by `src/wire.rs` |
| `signing-profile.md` | Transport, signing, assertion, approval, query shapes, purposes, roles, error codes |
| `SHA256SUMS` | sha256 of every file above |

Contract rules: a capability is never removed and its method, path, parameters, query
shape, permission, risk and approval never change; a published schema is immutable (add a
new name). Breaking changes need a major `CONTRACT_VERSION` bump agreed with HQ. Files are
LF-only (`.gitattributes`) because HQ pins their exact bytes.
