# Service data ownership

Every deployable service under `services/<name>/` owns its source, its migrations, its
database and the meaning of its data. Other services reach that data only through the
owner's authenticated API or the owner's durable events. This page is the reference; the
rules are enforced by
[`scripts/service-boundaries/check_service_boundaries.py`](../../scripts/service-boundaries/check_service_boundaries.py)
(`make service-boundaries`, CI: `.github/workflows/service-boundaries.yml`) and by each
service's own architecture tests. Decision record:
[ADR 0005](../decisions/0005-contract-authority-and-runtime-boundaries.md).

## Owners (Part III §13)

This is the authoritative five-domain ownership model. Each domain is owned by exactly
one service; *planned* and *future* services are boundaries only — no directory, code,
schema or contract exists for them until their implementation phase begins.

| Service | Status | Owns | Must not |
|---|---|---|---|
| **Calling** (`services/calling`) | **Active.** Platform-consumer foundation implemented; call domain planned (Part C) | • Calls<br>• Participants<br>• Sessions<br>• Routes<br>• Authorizations<br>• Call records<br>• Call quality<br>• Call-level usage<br>• Platform-consumer foundation *(implemented)*: tenants, applications, application environments, service accounts, API credentials and their scopes, limit policy, request-rate windows, communication principals | • read/write Privacy Relay private tables directly<br>• read/write Telephony private tables directly<br>• read Meeting Service tables<br>• receive ordinary plaintext protected contacts |
| **Privacy Relay** (`services/privacy-relay`) | **Planned — not yet created** | • Communication Contexts<br>• Relay sessions<br>• Protected contact references<br>• Contact Vault metadata<br>• Relay authorization | • expose plaintext contacts to ordinary consumers<br>• become a general product/order database |
| **Telephony** (`services/telephony`) | **Planned — not yet created** | • Call legs<br>• Provider bindings<br>• Carrier events<br>• Telephony routes<br>• SIP/provider operational state | • independently decide whether a business user is authorized to call<br>• bypass Calling / Privacy Relay policy |
| **Meeting Service** (`services/meeting-service`) | **Current** | • Meeting Spaces<br>• Conference Sessions<br>• Meeting participants<br>• Meeting-specific media control | • act as the universal backend for calls or telephony |
| **Numbering** (future `services/numbering`) | **Future — not yet created; no code** | • Permanent public-number inventory<br>• Public-number assignment<br>• Public-number lifecycle | • be prematurely implemented as Part III relay-number logic |

Notes on the model:

* **Each service has its own database.** Calling's is configured by its own
  `DATABASE_URL`, and every Calling table is `platform_*` today; the call-domain tables
  arrive with Part C. The Meeting Service's database additionally holds the private
  records that support its domains (accounts, organizations and memberships,
  scheduling and recurrence, chat, polls, Q&A, breakouts, transcripts, artifacts,
  webinar registrations, meeting webhooks, `api_keys`, its outbox); no other service
  reads them.
* **Authorizations and relay authorization are separate records.** Calling owns
  Authorizations; Privacy Relay owns Relay authorization. Each service keeps its own
  records in its own database; Calling obtains a relay decision through Privacy Relay's
  authenticated API (`communication.authorize`, received by Privacy Relay) — never by
  reading its tables.
* **Routes and telephony routes are separate records.** Calling owns Routes; Telephony
  owns Telephony routes. Neither service reads the other's.
* **Plaintext contacts.** The only path to a plaintext contact is Privacy Relay's
  `protected-contact.resolve-for-execution`, which the permission registry in
  `packages/service-auth` allows to be granted to Telephony only, for executing a
  specific authorized call leg. Calling and every other consumer receive protected
  contact references, never plaintext.
* **Telephony executes, it does not authorize.** Telephony acts on call legs Calling
  asks it to execute (`call-leg.execute` is grantable to Calling only), within the policy
  Calling and Privacy Relay have already applied.
* **Numbering is future work.** Part III relay-number logic must not be built as, or
  grow into, the Numbering service; permanent public-number inventory, assignment and
  lifecycle are designed with Numbering itself.

Identity is also owned:

* customer platform identity (tenant → application → environment → service account →
  credential) is Calling's (ADR 0003);
* communication principals are resolved by Calling within that scope (ADR 0003);
* service identity is a signed service token verified against the receiver's trust
  bundle (ADR 0004) — never a database lookup in another service;
* the Meeting Service is not an identity provider for Calling or any other service
  (ADR 0005).

Table ownership is **derived from migrations**, so it cannot drift: the guard collects
every `CREATE TABLE` and SeaORM `X::Table` in `services/<name>/**/migration/**`. Two
services may each own a table with the same name in their own databases (for example an
outbox); that is not shared data. Planned services are already known to the guard
through the service identities registered in `packages/service-auth` (`privacy-relay`,
`telephony`), so Calling cannot read their database locations even before they exist;
Numbering becomes known when it is registered or created.

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
