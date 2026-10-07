# Service data ownership

Every deployable service under `services/<name>/` owns its source, its migrations, its
database and the meaning of its data. Other services reach that data only through the
owner's authenticated API or the owner's durable events. This page is the reference; the
rules are enforced by
[`scripts/service-boundaries/check_service_boundaries.py`](../../scripts/service-boundaries/check_service_boundaries.py)
(`make service-boundaries`, CI: `.github/workflows/service-boundaries.yml`) and by each
service's own architecture tests. Decision record:
[ADR 0005](../decisions/0005-contract-authority-and-runtime-boundaries.md).

## Owners

| Service | Database (configured by) | Owns | Must never |
|---|---|---|---|
| `meeting-service` | its own PostgreSQL (`DATABASE_URL` of that deployment) | meeting accounts / users and their sessions, organizations and memberships, meeting spaces, conference sessions and participants, scheduling and recurrence, chat, polls, Q&A, breakouts, transcripts, artifacts, webinar registrations, meeting webhooks and `api_keys`, its outbox | act as identity provider or data source for Calling or any later service |
| `calling` | its own PostgreSQL (`DATABASE_URL` of that deployment), tables `platform_*` | platform consumers: tenants, applications, application environments, service accounts, API credentials and their scopes, limit policy, request-rate windows, communication principals. Later (Part C): calls and call participants | read Meeting Service users / organizations / tables; receive plaintext protected contacts |
| `privacy-relay` *(planned — not created)* | its own | protected contact references, communication authorizations, masking decisions | expose plaintext contacts except `protected-contact.resolve-for-execution` to Telephony |
| `telephony` *(planned — not created)* | its own | call-leg execution state, provider / carrier interaction | own customer identity or contact policy |

Table ownership is **derived from migrations**, so it cannot drift: the guard collects
every `CREATE TABLE` and SeaORM `X::Table` in `services/<name>/**/migration/**`. Two
services may each own a table with the same name in their own databases (for example an
outbox); that is not shared data.

Identity is also owned:

* customer platform identity (tenant → application → environment → service account →
  credential) is Calling's (ADR 0003);
* communication principals are resolved by Calling within that scope (ADR 0003);
* service identity is a signed service token verified against the receiver's trust
  bundle (ADR 0004) — never a database lookup in another service.

## How to use another service's data

| You need | Do |
|---|---|
| An answer now (authorize, resolve, execute) | Call the owner's `/internal/v1/` endpoint with `InternalServiceClient`; the owner grants the permission in **its** trust bundle |
| To react when something changes | Consume the owner's durable event (`cfmeet.platform.event-envelope`) |
| To query it often | Maintain your own projection fed by the owner's events; you own the projection, not the source data |
| Shared code | Put it in `packages/<name>` (no dependency on any service) |

Never: a second database connection to another service, a read replica of another
service's database, a "temporary" view or foreign table, or copying another service's
entities / migrations into your crate.

## Machine-enforced rules

| Rule | Fails when |
|---|---|
| `CARGO_CROSS_SERVICE_DEPENDENCY` | a Cargo manifest of service S has any path (dependency of any kind, `[patch]`, workspace member, bin / lib path) into `services/T` |
| `PACKAGE_DEPENDS_ON_SERVICE` | a manifest under `packages/` has a path into `services/` |
| `CARGO_PATH_OUTSIDE_REPOSITORY` | a manifest path leaves the repository |
| `CROSS_SERVICE_SOURCE_INCLUDE` | `include!` / `include_str!` / `include_bytes!` / `#[path]` (also via `CARGO_MANIFEST_DIR`) in S or a package resolves into `services/T` |
| `CROSS_SERVICE_PATH_LITERAL` | Rust source of S (including tests) names a path inside `services/T` |
| `CROSS_SERVICE_DATABASE_URL` | code, tests or deployment configuration of S name another service's database location, e.g. `MEETING_DATABASE_URL`, `LEGACY_MEETING_DB_URL`, `TELEPHONY_DATABASE_URL` (known services = `services/*` plus identities registered in `packages/service-auth`) |
| `CROSS_SERVICE_TABLE_ACCESS` | production SQL of S names a table only another service's migrations create |
| `CROSS_SERVICE_DB_HELPER` | production code of S defines or calls a helper named for another service's data store, e.g. `load_user_from_meeting_db`, `telephony_pool` |

Comments are ignored (documentation may discuss the rules); the data rules apply to
production code with `#[cfg(test)]` items removed, except database-location names, which
are refused in tests and configuration too. Calling's
`tests/architecture_boundaries.rs` additionally pins Calling-specific rules (own
`platform_*` tables only, no Meeting Service crate or `ActorContext`, no customer
credential machinery in internal service auth, no implicit conversions between context
types).
