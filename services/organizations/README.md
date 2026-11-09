# CF Meet Organizations Service

`services/organizations` (`cfmeet.organizations`) is the first-party **business
organization authority** of CF Meet (Blueprint v1.0 Sections 3–5,
[ADR 0056](../../docs/decisions/0056-first-party-organizations-and-workspace-boundaries.md)).
It owns organizations, profiles, memberships (with history), invitations, teams and team
members (Section 6), typed organization policies (Section 6), their audit events and
outbox, and organization ↔ Platform Access scope mappings with the provisioning saga
(Section 7, [ADR 0058](../../docs/decisions/0058-first-party-organization-scopes-and-provisioning.md);
off by default: `ORGANIZATIONS_PROVISIONING_ENABLED`).

* **Contract:** [`docs/api/organizations-internal-v1.md`](../../docs/api/organizations-internal-v1.md)
  (machine-readable: `packages/contracts/organizations/internal/v1.openapi.json`, served at
  `/openapi.json`). Events: [`docs/events/organization-event-contracts.md`](../../docs/events/organization-event-contracts.md).
* **Internal only.** Every `/internal/v1` route needs a `packages/service-auth` token whose
  issuer holds the route's `organization.*` permission in this service's trust bundle;
  replays are refused. Humans arrive through the Meeting Service relay, which forwards
  the verified acting user (`X-CF-Acting-User`, honoured only from
  `organization.act-for-user` holders).
* **Migration, not reconstruction.** The Meeting Service organizations are imported with
  ids and timestamps preserved (`POST /internal/v1/migration/import` or the
  `organizations-import` CLI). The Meeting Service stays the single writer until the gated
  cutover (Section 4); nothing here changes production.

## Layout

| Path | What |
|---|---|
| `src/http/` | router, signed-request middleware (auth, nonce replay refusal, actor), handlers |
| `src/domain.rs` | roles, capabilities, the default-deny decision |
| `src/import.rs` | export format, checksum, import, reconciliation |
| `src/store.rs` | rows, audit / history / outbox writers |
| `src/outbox.rs`, `src/worker.rs` | outbox lease protocol (delivery disabled), maintenance loop |
| `migration/` | own migration history (`organizations_seaql_migrations`) |
| `src/bin/organizations-import.rs` | operator import CLI |
| `docs/` | [API](docs/API.md), [operations](docs/OPERATIONS.md), [data ownership](docs/DATA_OWNERSHIP.md) |

## Develop

```bash
cd services/organizations
cargo fmt --all && cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --lib                                   # unit tests (no database)
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres cargo test --workspace
```

Integration tests need PostgreSQL (`TEST_DATABASE_URL` admin URL; each test clones a
migrated template database). `CFMEET_SKIP_DB_TESTS=1` skips them explicitly. CI:
`.github/workflows/organizations.yml`.
