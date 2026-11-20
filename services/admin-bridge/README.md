# services/admin-bridge (`cfmeet.admin-bridge`)

The only doorway from the CosmicForge central admin (HQ) into CF Meet
([ADR 0068](../../docs/decisions/0068-hq-managed-admin-bridge.md),
[design](../../docs/admin/cfmeet-admin-bridge.md)).

**Phase A1: the runtime, off by default.** It serves the four platform reads
(`bridge.capabilities.read`, `bridge.health.read`, `bridge.datasets.read`,
`hq.projections.status`); every other capability is refused (`CAPABILITY_DISABLED`)
until its phase. Not deployed by any workflow yet.

## Runtime

| Piece | Where |
|---|---|
| Configuration (every variable, all off by default) | `src/config.rs` |
| The one security chain (shape → certificate → HMAC → assertion → digest → revocation → replay → capability → authorization → rate limit → audit) | `src/chain/` |
| Signed access policy (verified at the pinned revision, refused outside the ceiling) | `src/policy.rs` |
| Listeners: private TLS 1.3 mTLS for HQ; loopback `/health`, `/ready`, `/metrics` | `src/server.rs` |
| Routes served in A1 | `src/routes.rs` |
| Store port, Postgres and in-memory | `src/store/` |
| Migrations (`admin_` tables only; append-only and hash-chain triggers) | `migration/` |
| Database and least-privilege roles | `deploy/db-roles.sql` |

`/ready` reports one readiness version: `contract_version`, `openapi_sha256` (equal to
the committed `docs/contract/openapi.json`) and the verified `policy_revision`.

## Enrolling HQ (HQ stage S3)

All writes need `--operator <name>`; nothing echoes a value.

```bash
admin-bridge-admin enroll-cert --fingerprint <sha256 of HQ client cert DER> --not-before … --expires … --operator …
admin-bridge-admin enroll-hmac --kid <X-HQ-Key-ID> --key-file /secure/<kid>.key --expires … --operator …   # stores only sha256
admin-bridge-admin enroll-staff-key --kid <jws kid> --public-key-hex … --issuer cosmicforge-hq --audience cfmeet-admin-bridge --expires … --operator …
admin-bridge-admin policy-ceiling --revision 1 > policy.json        # edit down to a READ-ONLY policy first
admin-bridge-admin policy-sign --key-file /secure/policy.pem --document policy.json   # offline
admin-bridge-admin policy-put --revision 1 --document policy.json --signature … --key-id … --public-key-hex … --operator …
admin-bridge-admin verify-audit-chain
```

Then set `ADMIN_BRIDGE_POLICY_REVISION=1`, `ADMIN_BRIDGE_ENABLED=true` and restart. The HMAC
key file goes in `ADMIN_BRIDGE_HMAC_KEY_DIR` as `<kid>.key`; revoke with
`revoke-principal` / `revoke-staff`.

## Contract HQ pins

`docs/contract/` is generated from the capability catalog (`src/contract/catalog.rs`), the
single source of truth for the future runtime route table. Never edit those files by hand.

```bash
cargo run --bin admin-bridge-contract -- generate               # rewrite docs/contract/
cargo run --bin admin-bridge-contract -- check                  # drift check (CI)
cargo run --bin admin-bridge-contract -- compat --base <dir>    # breaking-change gate vs main (CI)
cargo test                                                      # contract, chain, HQ simulator over real mTLS
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres cargo test   # + Postgres store (CI job `postgres`)
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
