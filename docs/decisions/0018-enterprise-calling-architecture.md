# ADR 0018: Enterprise calling architecture — configuration and continuity inside Calling

- **Status:** Accepted
- **Date:** 2026-10-09
- **Scope:** CF Meet Part F §52–§62. Implemented in `services/calling/src/enterprise/`
  (configuration domain and the `/v1/enterprise/**` API) and
  `services/calling/src/calls/app/enterprise/` (runtime routing of enterprise Calls),
  Calling migrations 000020+, Telephony execution primitives in `services/telephony/`.
  Implementation map: [`docs/architecture/enterprise-calling-implementation-map.md`](../architecture/enterprise-calling-implementation-map.md).

## Context

Part F adds enterprise calling (extensions, departments, ring groups, queues, IVR,
business hours, forwarding, hold / resume / transfer, voicemail, call recording policy)
and call-history continuity. The platform already has one canonical, provider-independent
Call (ADR 0006), multi-device ringing with single-winner arbitration (ADR 0008), call
control and transfer on the same Call (ADR 0009), a Routing Engine with an
`ENTERPRISE_INTERNAL` route class that was representable but not executable (ADR 0013),
and Telephony as the execution plane for SIP / public-network legs (ADR 0014–0017).

The risk is a second architecture: a PBX service with its own "calls", queue calls,
voicemail calls, its own organization identity, or telephony business logic in the
dialplan.

## Decision

### One Call, one owner

Enterprise calling is configuration and routing behaviour of the existing Calling
service. An enterprise call is an ordinary canonical Call (`calls`), with its usual
participants, sessions, routes, legs, attempts, authorizations, events and outbox. There
is no `enterprise_calls` / `pbx_calls` / `voicemail_calls` table and no
`services/pbx`, `services/enterprise-calling`, `services/voicemail` or `services/ivr`
deployable (guarded by `architecture_boundaries` and `migrations`).

* `services/calling` owns enterprise configuration (policies, departments, extensions,
  ring groups, queues, IVR flows, business hours, forwarding, mailboxes and message
  metadata, recording policies / authorizations / recordings), the queue business state,
  IVR business state, history and missed-call semantics.
* `services/telephony` owns execution only: internal SIP legs of enterprise devices,
  answer, built-in prompt playback, DTMF capture (forwarded as normalized events), hold /
  resume, redirect, bridges, media capture of a leg or bridge and the transient capture
  file. It never decides routing, membership, schedules or recording permission.
* The Meeting Service is not involved: no Meeting Space, Conference Session, meeting
  participant, meeting permission or meeting recording policy is an enterprise entity.

### The organization boundary is the platform tenant

The platform tenant / application / environment triple (ADR 0003) is the organization
security boundary. Every enterprise row carries it and references
`platform_environments` with a composite foreign key; ownership columns are immutable
(trigger). Principals referenced by enterprise rows (department members, ring-group
members, queue members, mailbox owners) are referenced with the composite
`(tenant, application, environment, principal)` key, so a cross-tenant reference cannot
be written. No overlapping "organization" identity is created; nothing in the
requirements needs a namespace finer than an application environment.

### Extension namespace

An extension number is an internal short code — never a public telephone number. It is
unique among ACTIVE extensions of one `(tenant, application, environment)` (partial
unique index), normalized to lower case, and its format (numeric 2–6 digits, or
alphanumeric starting with a letter) is set by the enterprise policy. Numeric extensions
may not be prefixes of one another (overlap dialing would be ambiguous). Global
uniqueness is not required.

### Typed policy, explicit scopes, operator-granted high-risk capabilities

`EnterpriseCallPolicy` is typed: every hook (internal calling, external public calling,
public destination countries, forwarding, public forwarding, spend policy reference,
voicemail, transfer, ring-group fan-out, queue capacity / wait, routing hops, extension
format, administrator capabilities) is its own validated column. There is no policy
blob. An environment has at most one ACTIVE default policy; an extension may name
another one; without any the restrictive built-in default applies.

Administration requires explicit scopes (`enterprise.config.read|write`,
`enterprise.calls.use`, `enterprise.voicemail.read|manage`,
`enterprise.recording.read|manage`); none is implied by `calls.*` and none is granted
automatically. Administrator capabilities on the baseline policy are a second key;
`MANAGE_PUBLIC_FORWARDING` and `MANAGE_RECORDING` can only be granted by a CF Meet
operator (`platform_admin enterprise-capabilities set`), never through the customer API.

### Versioning and audit

Mutable configuration carries `version >= 1`; every change takes `expectedVersion` and a
stale one is `409 VERSION_CONFLICT` with no effect. The database refuses any UPDATE that
does not advance `version` by exactly one. Every administrative change appends an
`enterprise_admin_audit` row in the same transaction (actor kind, service account /
credential, scope, resource, operation, old / new version, request / correlation ids,
bounded safe details); the table is append-only.

### Routing

`ENTERPRISE_INTERNAL` becomes executable: an enterprise call whose every party is
internal is routed without any public-network leg, cost or provider spend fact (route
class `PRIVATE_NETWORK`). Public forwarding is the only enterprise path to the public
network and goes through every Part D / E gate (ContactReference, Privacy Relay
authority, routing policy, live telecom, destination country, spend, Telephony route) —
an administrator's configuration never bypasses them.

### Flags

`ENTERPRISE_CALLING_ENABLED`, `VOICEMAIL_ENABLED`, `CALL_RECORDING_ENABLED` default to
`false` (the kill-switch convention of Part E). They stop NEW effects only; call
termination, disabling extensions / forwarding, finalizing captured artifacts, deleting
recordings / voicemail and retention cleanup are never blocked. Configuration APIs work
with the flags off (an organization configures before enabling).

## Consequences

* The Call-level machinery (state machine, events, outbox, arbitration, reconciliation,
  history) applies to enterprise calls unchanged.
* Enterprise resources are never deleted: disabling preserves historical references.
* A future split into a separate deployable would need a new ADR; nothing in Part F
  requires one.
