# ADR 0006: Canonical Call aggregate and state machine

- **Status:** Accepted
- **Date:** 2026-10-07
- **Scope:** CF Meet Part III, Part C §15–§19, implemented in `services/calling/src/calls/`

## Context

Part B gave the Calling service its platform trust foundation (tenants, applications,
environments, service accounts, credentials, scopes, limits, principals, internal service
authentication, contract authority, data ownership). Part C starts the call domain. Calls
will later be executed by application RTC, SIP, the public telephone network, privacy
relays, callback relays and enterprise infrastructure, sometimes with several network
legs and several provider attempts. Without a canonical model, each execution path would
invent its own notion of "the call", provider states would leak into business logic, and
retries or late provider callbacks could resurrect or corrupt finished calls.

## Decision

### 1. Calling owns the logical Call; it is provider-independent

* The **Call** is the aggregate root, owned by `services/calling` (table `calls`). Its
  identity is a CF Meet-generated UUID. A LiveKit room id, SIP dialog id, carrier call id
  or provider request id is never a Call identity and never a canonical field.
* References to records of other services (`communication_context_ref`,
  `privacy_relay_ref`, a leg's `execution_binding_ref`) are opaque UUIDs — never foreign
  keys into another service's database, never provider ids or telephone numbers.
* Call kinds are neutral (`VOICE`, `VIDEO`). There are no product-specific call types.

### 2. The ten §16 entities

`calls`, `call_participants`, `call_sessions` (+ `call_session_intervals`),
`call_routes`, `call_legs`, `call_attempts`, `call_authorizations`, `call_events`
(+ `calling_outbox_events`), `call_quality_records`, `call_usage_records` — all in
Calling's own database, all carrying `tenant_id / application_id / environment_id`,
all linked to their parents by composite foreign keys so no row can reference a Call,
principal, credential, session, leg or route of another scope. Ownership columns are
immutable (trigger). Routes, legs, attempts, quality and usage are domain foundations:
persistence, ownership, states and primitives only — no routing algorithm, failover,
provider execution, analytics or billing.

### 3. Canonical states and the transition graph

Exactly 17 states: `REQUESTED AUTHORIZED RESOLVING ROUTING DIALING RINGING ANSWERED
CONNECTED HELD TRANSFERRING ENDING` and terminal `ENDED DECLINED BUSY NO_ANSWER FAILED
CANCELLED`. The specification defines the states and rules but not every edge; this
conservative graph is the implementation decision:

```text
REQUESTED    → AUTHORIZED | FAILED | CANCELLED
AUTHORIZED   → RESOLVING | FAILED | CANCELLED
RESOLVING    → ROUTING | FAILED | CANCELLED
ROUTING      → DIALING | FAILED | CANCELLED
DIALING      → RINGING | ANSWERED | DECLINED | BUSY | NO_ANSWER | FAILED | CANCELLED
RINGING      → ANSWERED | DECLINED | BUSY | NO_ANSWER | FAILED | CANCELLED
ANSWERED     → CONNECTED | ENDING | FAILED | CANCELLED
CONNECTED    → HELD | TRANSFERRING | ENDING | FAILED
HELD         → CONNECTED | TRANSFERRING | ENDING | FAILED
TRANSFERRING → CONNECTED | HELD | ENDING | FAILED
ENDING       → ENDED | FAILED
```

No edges leave terminal states. Provider behaviour is mapped onto these semantics by the
adapters that observe it; the graph is not widened for a provider. The graph lives in
Rust (`CallState::allowed_targets`) and is repeated in the database
(`calling_call_transition_allowed`) as defence in depth; a test proves both agree on all
289 state pairs.

### 4. Actions, evidence and optimistic versioning

* Clients and services request **actions**; the server derives the target state. No API
  or domain method sets a state, and the public call API is read-only.
* Each Call has a `version` (1 at creation, +1 exactly once per transition). Every
  transition command carries `expected_version`; a stale command fails with
  `STALE_CALL_VERSION` and changes nothing. The transition runs in one transaction:
  lock the Call row (scope-confined), check version, check state and graph, verify the
  action's evidence, update the Call (state, version, first-entry timestamp, terminal
  fields), append the CallEvent and its outbox row, commit.
* **Authorization.** REQUESTED → AUTHORIZED requires the Call's effective APPROVED
  `call_authorizations` decision. There is no canonical DENIED state: a DENIED decision
  stays its own evidence and maps to FAILED with the normalized failure category `POLICY`
  only through `DenyByPolicy` (the generic `Fail` cannot use `POLICY`). Decisions
  supersede each other; exactly one is effective per Call. This is the persisted decision
  aggregate, not the policy engine (next workstream).
* **Paid public-network guard.** `require_public_network_authorization` produces the only
  proof (`PublicNetworkAuthorization`) that a Call may perform a paid public-network
  action: in scope, authorized and executable, effective decision APPROVED, environment
  allowed live telecom. Future telephony commands take this proof as an argument.
* **CONNECTED** requires positive, provider-independent evidence of an established
  bidirectional media path (connected sessions of two participants, two connected legs,
  or a connected session bridged to a connected leg) on every entry (answer, resume,
  completed transfer). "The provider accepted the request" is not evidence. ANSWERED
  (target accepted) never implies CONNECTED, and ANSWERED → FAILED is allowed when media
  establishment fails.
* **ENDING → ENDED** requires cleanup confirmed, verified end of all participation, or
  the documented reconciliation-timeout disposition; the evidence becomes the Call's
  `terminal_reason` and is recorded in the event. Reconciliation workers are later work;
  they will call this mechanism.
* **Terminal is final**: refused in Rust and by a database trigger, so late provider or
  realtime events can never resurrect a Call.
* **Failure categories** are a small neutral registry (`POLICY TARGET_UNAVAILABLE ROUTING
  TIMEOUT MEDIA NETWORK EXECUTION INTERNAL UNKNOWN`); provider error codes stay inside
  execution boundaries.

### 5. Participants are logical; sessions are connections

* A **CallParticipant** is a logical participant: a `ResolvedPrincipal` resolved in the
  caller's own tenant / application / environment (database-pinned, including the
  principal-kind snapshot). One participant per principal per Call for the Call's whole
  lifetime (`UNIQUE (call_id, principal_id)`); adding it again is idempotent. It owns
  identity, role and history. Display names are bounded presentation metadata, never
  identity or authorization input.
* A **CallSession** is one device / network participation with a server-generated
  media / signaling identity (`cfmid_` + 256 random bits; unique; never client input,
  never a provider participant id). A participant may hold many sessions at once (no
  uniqueness on `participant_id`); multi-device answer arbitration is §20 (ADR 0008).
* **Reconnect** is explicit: *renewal* opens connection interval n + 1 on the same
  session; *replacement* ends the session and creates a new one (new media identity,
  `replaces_session_id`) for the same participant. Neither touches the Call or the
  participant.
* **Participation time** is computed from `call_session_intervals` (one open interval
  per session, gap-free numbering, no overlap, closed intervals never reopen): per
  session the sum of its intervals, per participant the **union** of its sessions'
  intervals — concurrent devices are not double counted. Never from a single join /
  leave pair or the participant's lifetime.

### 6. Immutable events and the transactional outbox

Every mutation appends immutable `call_events` (gap-free per-call `sequence`, the Call
version, states for lifecycle events, safe actor / request / correlation references and
a bounded, privacy-safe payload) and one `calling_outbox_events` row per event in the
same transaction. The outbox row holds a `cfmeet.platform.event-envelope` whose payload
is the new contract `cfmeet.calling.call-lifecycle-event` (vocabulary in
`cfmeet.calling.call-common`). The database refuses event `UPDATE` / `DELETE` /
`TRUNCATE`, a new Call version without its state event, and an event without its outbox
row (deferred constraint triggers), so the authoritative state, its evidence and its
publication record commit together or not at all. Nothing is published yet; Kafka / NATS
/ webhooks / customer event delivery are later work.

### 7. Calling-side legs vs Telephony execution

Calling owns the canonical, provider-independent **CallLeg** (id, Call, route,
participant, direction, neutral kind, normalized state, timing, opaque
`execution_binding_ref`). The future Telephony service owns provider call bindings,
carrier / provider identifiers, SIP dialogs, provider callbacks, telephony commands and
reconciliation state in its own database and maps them to a `CallLegId`. Calling stores
no raw provider payload or carrier business state.

### 8. Read-only public API at this stage

`GET /v1/calls` (keyset pagination, bounded filters) and `GET /v1/calls/{call_id}` with
scope `calls.read`, scope taken only from the credential, foreign Calls answered exactly
like missing ones. Creation and control routes arrive with the policy / API / signaling
work; `create_requested_call` exists as a domain command (with race-safe
concurrent-call limit enforcement) for them.

## Consequences

* Execution paths added later (RTC, telephony, relay) must map onto the canonical states
  and evidence types; they cannot introduce provider states, set states directly or
  bypass the authorization guard.
* Every new kind of call fact is a new `CallEventType`, a contract change in
  `cfmeet.calling.call-common`, and an outbox envelope consumers can rely on.
* The database repeats the critical invariants (graph, terminal states, versioning,
  event immutability, transactional evidence, ownership), so even direct SQL cannot
  produce catastrophic invalid state.

## Deferred

Delivered since by Part C §20–§25: multi-device ringing and answer arbitration
(ADR 0008), application route / attempt orchestration, application voice / video /
group calling and the media provider contract (ADR 0007), call creation / control APIs,
realtime signaling and provider-enforced cancel / end / hold / transfer (ADR 0009).

Still deferred: a general routing engine and failover across networks; the call
authorization / policy engine beyond `APP_DIRECT_V1`; push delivery through a vendor,
customer webhooks and event delivery; Privacy Relay, Contact Vault, Telephony and
Numbering services.
