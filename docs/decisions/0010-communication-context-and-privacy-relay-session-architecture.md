# ADR 0010: Communication Context and Privacy Relay Session architecture

- **Status:** Accepted
- **Date:** 2026-10-08
- **Scope:** CF Meet Part D §26–§30; implemented in `services/privacy-relay/` (new peer
  service, migrations 000001–000005), `services/calling/src/privacy_relay/`,
  `services/calling/src/calls/app/privacy.rs` (Calling migration 000015) and the
  `cfmeet.privacy-relay.*` contracts in `packages/contracts/privacy-relay/`.
  Contact Vault (§31–§33), relay-number inventory and telephony are **not** part of
  this decision and are not implemented.

## Context

Consumers of the platform (a delivery marketplace, a clinic, a support desk, …) need
their users to talk inside a bounded business relationship — an order, an appointment,
a ticket — without either side learning the other's identity details, and with the
consumer able to reassign, suspend or end that relationship at any time. That authority
must be generic (no consumer's product model in CF Meet), versioned, auditable, and
enforced on calls that are already in progress, while staying out of Calling's data
and Calling staying out of its data.

## Decision

### A separate peer service with its own database

`services/privacy-relay` is a Rust/Axum/SeaORM service with its own PostgreSQL
database (migration table `privacy_relay_seaql_migrations`). It depends only on
`packages/service-auth` and the contracts; it never reads Calling's principals, calls,
credentials or tables, and Calling never reads its tables (repository guard,
architecture tests of both services). It exposes **only** a service-authenticated
`/internal/v1/...` API plus `/health`.

### Customers reach it through Calling (§28)

Customer requests (`/v1/privacy-relay/...`, contract `cfmeet.privacy-relay.http-api`)
are served by **thin** handlers in Calling: platform-credential authentication, the
public scopes `privacy.contexts.read|write` and `privacy.sessions.read|write`,
`Idempotency-Key` on every mutation, path / query shape, and validation that every
named principal is an ACTIVE principal of the caller's own application environment
(principal identity is Calling's domain). Calling then forwards the request over
signed internal HTTP with a **delegated platform context** (tenant, application,
environment, service account) derived from the authenticated credential. The customer
credential, its verifier and the pepper are never forwarded. Privacy Relay's answer is
relayed with Calling's request id; internal or unknown error codes become
`SERVICE_UNAVAILABLE`, and an unreachable or unconfigured Privacy Relay is 503.

Responsibility split: the consumer decides **who** belongs to the relationship and
**when** it ends (participants, channels, directions, validity, lifecycle commands);
CF Meet decides **whether a specific communication is authorized now** and enforces it.
CF Meet never infers a relationship from a consumer's product data.

### Communication Context (§27)

A context is scoped to (tenant, application, environment) and keyed by an opaque
`externalContextReference` (unique per scope; never logged). It carries participants
(subject references: a CF Meet principal id or an opaque ContactReference id — never a
telephone number, e-mail address or SIP URI), allowed channels (VOICE executable;
MESSAGING declarative), a direction policy (BIDIRECTIONAL or EXPLICIT directed edges,
optionally per channel), validity (`startsAt`, `expiresAt`, optional `graceExpiresAt`),
the allowed relay strategies and a fail-safe policy (terminate active calls on revoke
and on participant removal by default; not on suspend; no new calls in grace).

Status is ACTIVE → SUSPENDED ⇄ ACTIVE → COMPLETED / REVOKED / EXPIRED. REVOKED and
EXPIRED are terminal. Authority is evaluated **at read time** from status and validity
(`Window::authority`), so an expired context denies immediately even before the expiry
worker records EXPIRED.

### Updates, reassignment and revocation (§29)

Every mutation carries `expectedVersion` (409 `CONTEXT_VERSION_CONFLICT` otherwise) and
an idempotency key. Updates are typed (participants / directions replace the whole set,
never a merge patch); reassignment keeps the **same** context and bumps its version —
revocation never requires creating a new context. Database triggers forbid mutation of
identity columns, illegal lifecycle transitions and reassignment of terminal contexts.

Idempotency: a transaction-scoped advisory lock on (scope, operation, key) serializes
duplicates; `privacy_relay_commands` stores the request fingerprint and response.
Same key + same request replays (`Idempotent-Replayed: true`); a different request is
409 `IDEMPOTENCY_CONFLICT`.

Audit: every change appends a `relay_audit_events` row, gap-free per context (written
under the context row lock), and an outbox row in the platform event envelope
(`cfmeet.privacy-relay.event`). Payloads are safe documents (a database function
refuses keys that look like secrets or contact values).

### Privacy Relay Sessions (§30)

A session is a communication instance under a context with a relay strategy. It may
only **narrow** its context: participants, channels, directions and validity are
INHERITED or NARROWED subsets (a containment trigger enforces validity). Every context
change re-derives every live session's snapshot; narrowed scopes are intersected with
the new context, so removing a context participant removes them from every session.
Terminal context states cascade to sessions. `executableRoutes` lists only routes CF
Meet can actually execute today (APPLICATION); PUBLIC_RELAY / CALLBACK_RELAY are
policy only until relay-number and telephony infrastructure exists.

### Authorization: policy `PRIVACY_RELAY_V1`

`POST /internal/v1/communication/authorize` (permission `communication.authorize`,
received by Privacy Relay, grantable to Calling only) evaluates context ∩ session under
`FOR SHARE` locks: both windows, participant membership of source and every target,
direction, channel and strategy. Every decision (AUTHORIZED or DENIED with a reason) is
recorded in `relay_authorization_decisions` and returned with the context and session
versions and `validUntil`.

Calling creates a **protected call** when `POST /v1/calls` carries
`privacyRelaySessionId` (VOICE only). It asks Privacy Relay with the caller's delegated
scope, **fails closed** (denied → 403 `CALL_NOT_AUTHORIZED`; unknown session → 404;
unavailable / malformed → 503 `PRIVACY_AUTHORIZATION_UNAVAILABLE`) and records the
decision it relied on in `call_privacy_authorizations` (append-only; ids and versions
only). A transfer of a protected call is re-authorized for the new target
(`CALL_TRANSFER`). Ordinary calls are unaffected.

### Active-call enforcement

Privacy Relay never terminates calls. A mutation that narrows authority creates a
durable `privacy_relay_enforcement_jobs` row in the same transaction (unique per context
or session version and reason): revoke and participant removal (per the context policy,
on by default), suspend (only when the policy enables it), completion without remaining
grace, expiry, and session revoke / suspend / expiry. It is delivered
synchronously first and then by a retry worker (`FOR UPDATE SKIP LOCKED`, backoff
min(2ⁿ, 300) s) to Calling's `POST /internal/v1/privacy-relay/enforcement`
(permission `communication-authority.enforce`, received by Calling, grantable to
Privacy Relay only; contract `cfmeet.privacy-relay.enforcement-command`). The mutation
response reports `enforcement.status` PENDING until Calling confirms, then APPLIED —
never claimed before confirmation. Without a configured Calling URL jobs stay PENDING
while authority is still revoked locally.

Calling deduplicates by `enforcementEventId` (`call_privacy_enforcements`), serializes
with protected-call creation / transfer through an advisory lock per context, matches
only calls of the delegated scope whose recorded decision is at or below the command's
context (and session) version and involves an affected principal, and terminates them
through the ordinary call commands: CANCEL before answer, END (→ ENDING → provider
cleanup → ENDED) after. A decision already superseded by a received enforcement is
refused at creation time, closing the authorize / enforce race.

### Feature flag

`PRIVACY_RELAY_ENABLED` (default **false**) refuses new contexts, sessions, resumes and
authorizations with `PRIVACY_RELAY_DISABLED`; restrictive operations (suspend, revoke,
complete, updates, expiry, enforcement) keep working so turning the feature off never
widens access.

## Consequences

* Two services, two databases, one direction of authority: Calling asks
  (`communication.authorize`), Privacy Relay tells (`communication-authority.enforce`);
  trust bundles on each receiver decide.
* No consumer-specific model, no product-name branching (architecture tests).
* No telephone number or contact value exists anywhere in Privacy Relay's schema, APIs,
  events or logs (contact-safety tests, migration smoke in CI). Contact Vault, relay
  numbers and telephony routes will be designed in their own sections and ADRs.
* Cross-service behaviour is verified against the real binary
  (`services/calling/tests/privacy_relay_e2e.rs`, CI job `calling-e2e` in
  `.github/workflows/privacy-relay.yml`).
