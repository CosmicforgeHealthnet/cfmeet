# CF Meet Calling service

`services/calling/` is the Rust/Axum service for the CF Meet **Calling** domain. It is a
peer of `services/meeting-service/` and is independently buildable and deployable: it
shares no source, models, migrations or database with the Meeting Service.

## What is implemented

**Part B §7–§14 (platform trust foundation), Part C §15–§19 (Calling core), Part C §20–§25 (application calling), Calling's side of Part D §26–§30 (Privacy Relay ingress, protected calls, active-call enforcement), Calling's side of Part E §37–§51 (Routing Engine, public-network calls through Telephony) and Part F §52–§62 (enterprise calling and continuity — see [below](#enterprise-calling-part-f-5262)).** The
service is the canonical authority for the **logical Call**: a CF Meet object with a CF
Meet-generated UUID that stays the same whatever later executes it (application RTC,
SIP, the public network, a relay, enterprise infrastructure, several legs or attempts).
Provider room / dialog / carrier / request identifiers are never Call identities.

| Area | Status |
|---|---|
| §7–§10 Platform consumer model, first-party / external consumers, API credential security, `PlatformAuthContext` vs `ResolvedPrincipal` | Implemented |
| §11 Internal service authentication (`packages/service-auth`, `/internal/v1/*`, delegated context, internal client) | Implemented (internal route: Privacy Relay enforcement) |
| §12 Contract authority: `packages/contracts/calling/**` describes exactly what is implemented; conformance tests | Implemented |
| §13 Data ownership: own database only | Enforced (`architecture_boundaries`, `make service-boundaries`) |
| §14 Runtime direction: events only through the platform event envelope | Implemented for call events (transactional outbox; no publisher yet) |
| **§15** Calling service responsibility: canonical logical Call, provider-independent, HTTP-independent domain service | **Implemented** (`src/calls/`) |
| **§16** Required entities: Call, Call Participant, Call Session (+ connection intervals), Call Leg, Call Attempt, Call Route, Call Authorization, Call Event (+ outbox), Call Quality Record, Call Usage Record | **Implemented** (persistence, ownership, states, domain primitives) |
| **§17** Canonical state machine: the 17 states, transition graph, terminal states, optimistic versioning | **Implemented** (Rust + database defence in depth) |
| **§18** Transition rules: actions not client-set states, stale-command protection, no terminal resurrection, AUTHORIZED needs an APPROVED decision, paid public-network guard, CONNECTED / ENDED evidence, normalized failure categories | **Implemented** |
| **§19** Participant / session model: principal-scoped participants, server media identities, multi-device sessions, explicit reconnect, interval-based participation time | **Implemented** |
| Concurrent-call limit (Part B foundation) | **Enforced** at Call creation (race-safe) |
| Read API: `GET /v1/calls`, `GET /v1/calls/{call_id}` (`calls.read`) | **Implemented** |
| **§20** Multi-device ringing and arbitration: endpoint registry, signaling credentials, invitations with server expiry, single winner per participant (DB constraint), one-device decline, DECLINED / NO_ANSWER, recovery API, WebSocket signaling, cross-instance fan-out | **Implemented** (ADR 0008) |
| **§21** Call legs, attempts and routes: one APPLICATION route, RTC leg, idempotent attempt, deterministic media resource, private media binding | **Implemented** |
| **§22** App-to-app voice: `POST /v1/calls`, APP_DIRECT_V1 authorization, join-only credentials, CONNECTED on verified media evidence of two parties, usage finalization | **Implemented** |
| **§23** App-to-app video and group calls (≤ 16 participants), per-participant permissions, individual session removal with provider enforcement | **Implemented** |
| **§24** Calling media provider boundary: `CallMediaProvider` trait + capabilities, LiveKit adapter with its own config, media connection gateway (DB admission of every connection, edge-sealed credentials, refreshed-token re-sealing), signed callbacks as evidence only, dedupe, reconciliation, real-LiveKit certification incl. refreshed-token re-entry | **Implemented** (ADR 0007) |
| **§25** Call control: accept, decline, cancel, end (ENDING → ENDED on cleanup evidence), hold / resume enforced at the provider, application transfer on the same Call with failure recovery, `expectedVersion`, `Idempotency-Key`, available actions | **Implemented** (ADR 0009) |
| **Part D §26–§30** Privacy Relay ingress (`/v1/privacy-relay/...`), `PRIVACY_RELAY_V1` protected calls and transfers (fail closed), `POST /internal/v1/privacy-relay/enforcement` | **Implemented** (ADR 0010; the business authority is `services/privacy-relay`) |
| **Part E §37–§51** Routing Engine and route plans, public-network calls executed by `services/telephony` (ContactReference parties, durable command outbox, normalized events, bridges, spend reservations, failover) | **Implemented** (ADRs 0013–0017). **Real lawful telecom interconnect certification remains an external blocker (ADR 0016): no PSTN result is claimed** |
| **Part F §52–§62** Enterprise calling and continuity on the ONE canonical Call | **Implemented** (ADRs 0018–0027; summary below) |
| **Part G §63–§70** Durable event publication, customer webhooks, `call.route_changed`, usage facts and `GET /v1/usage`, tenant → application → credential limits, `GET /v1/limits`, limit notices | **Implemented** (ADR 0028; summary below) |

**Deferred (later sections / services):** push delivery through a push vendor (a
transport-neutral push-ready payload is produced), a message-broker transport for
platform events, quality metrics from providers that do not offer them, Numbering,
billing / invoicing / pricing, and the Part F items listed below.

HTTP surface (contract:
[`packages/contracts/calling/http/v1.openapi.json`](../../packages/contracts/calling/http/v1.openapi.json),
bodies in [`calling/control`](../../packages/contracts/calling/control/v1.schema.json),
signals in [`calling/realtime`](../../packages/contracts/calling/realtime/v1.schema.json)):
`GET /health`; the call API `GET /v1/calls`, `GET /v1/calls/{call_id}`,
`GET /v1/calls/{call_id}/actions`; call creation and control `POST /v1/calls`,
`POST /v1/calls/{call_id}/accept|decline|cancel|end|hold|resume|transfer`,
`POST /v1/calls/{call_id}/sessions/{session_id}/media-credentials|remove`; endpoints
`POST|GET /v1/calling/endpoints`, `DELETE /v1/calling/endpoints/{endpoint_id}`,
`POST /v1/calling/signaling-credentials` (all platform API credential); the endpoint's
own `GET /v1/calling/signaling` (WebSocket), `GET /v1/calling/invitations`,
`POST /v1/calling/invitations/{id}/accept|decline`,
`POST /v1/calling/sessions/{session_id}/media-credentials` (signaling credential only);
the media connection gateway `GET /v1/calling/media/rtc[/v1][/validate]` (edge-sealed
media credential only); and `POST /internal/provider/media/events` (media provider
signature only).
The customer authentication middleware and `PlatformAuthContext` extractor, and the
internal service authentication middleware and `InternalCaller` extractor, are in place.
Every path under `/internal/v1/` — including unknown ones — requires a valid service
token and answers `401 INTERNAL_AUTHENTICATION_FAILED` otherwise; customer API
credentials are never accepted there, and service tokens are never accepted on customer
routes.

## Calling core (Part C §15–§19)

ADR: [0006 — Canonical Call aggregate and state machine](../../docs/decisions/0006-canonical-call-aggregate-and-state-machine.md).

### Entities

```text
Call (aggregate root: kind, state, version, first-entry timestamps, terminal reason / failure category)
├── CallParticipant      a trusted principal of the Call's scope (one per principal per Call)
│   └── CallSession      one device / network participation; server-generated media identity
│       └── interval     one connected period (interval 1, 2, … = reconnects)
├── CallRoute            provider-independent routing decision (class, state, policy version, snapshot)
├── CallLeg              canonical Calling-side leg (direction, neutral kind, normalized state, timing,
│                        opaque execution_binding_ref UUID only)
├── CallAttempt          gap-free per-call number, previous-attempt link, explicit UNKNOWN outcome
├── CallAuthorization    APPROVED / DENIED decision evidence; exactly one effective per Call
├── CallEvent            immutable, gap-free per-call sequence (+ one outbox row each)
├── CallQualityRecord    bounded numeric, provider-independent metrics
└── CallUsageRecord      idempotent consumption facts with set-once finalization (not billing)
```

Every row carries `tenant_id / application_id / environment_id` and references its parent
through composite foreign keys, so no child can point at a Call (or principal, session,
leg, route, credential) of another scope; ownership columns are immutable.
`communication_context_ref` / `privacy_relay_ref` / `execution_binding_ref` are opaque
UUIDs of records owned by other services — never foreign keys, never provider ids or
telephone numbers.

### State machine

States: `REQUESTED AUTHORIZED RESOLVING ROUTING DIALING RINGING ANSWERED CONNECTED HELD
TRANSFERRING ENDING` and terminal `ENDED DECLINED BUSY NO_ANSWER FAILED CANCELLED`.

| From | To |
|---|---|
| REQUESTED | AUTHORIZED, FAILED, CANCELLED |
| AUTHORIZED | RESOLVING, FAILED, CANCELLED |
| RESOLVING | ROUTING, FAILED, CANCELLED |
| ROUTING | DIALING, FAILED, CANCELLED |
| DIALING | RINGING, ANSWERED, DECLINED, BUSY, NO_ANSWER, FAILED, CANCELLED |
| RINGING | ANSWERED, DECLINED, BUSY, NO_ANSWER, FAILED, CANCELLED |
| ANSWERED | CONNECTED, ENDING, FAILED, CANCELLED |
| CONNECTED | HELD, TRANSFERRING, ENDING, FAILED |
| HELD | CONNECTED, TRANSFERRING, ENDING, FAILED |
| TRANSFERRING | CONNECTED, HELD, ENDING, FAILED |
| ENDING | ENDED, FAILED |
| terminal | — |

* **Actions, not states.** `CallService::transition(cmd, call_id, expected_version,
  action)` takes an action (`Authorize`, `DenyByPolicy`, `BeginResolution`,
  `BeginRouting`, `BeginDialing`, `MarkRinging`, `Accept`, `ConfirmConnected`, `Hold`,
  `Resume`, `BeginTransfer`, `CompleteTransfer`, `RequestEnd`, `ConfirmEnded`,
  `Decline`, `MarkBusy`, `MarkNoAnswer`, `Fail`, `Cancel`); the target state is derived
  by the server. No API sets a state.
* **Optimistic versioning.** Version 1 at creation, +1 exactly once per transition; a
  command whose `expected_version` is stale gets `STALE_CALL_VERSION` and changes nothing.
* **Evidence.** AUTHORIZED needs the Call's effective APPROVED authorization; a DENIED
  decision can only lead to FAILED / `POLICY` (`DenyByPolicy` — there is no canonical
  DENIED state). Every entry into CONNECTED needs positive evidence of a bidirectional
  media path (`RtcMediaPathEstablished`: connected sessions of two participants;
  `NetworkLegEstablished`: two connected legs; `MixedBridgeEstablished`: a connected
  session and a connected leg) — ANSWERED never implies CONNECTED. ENDING → ENDED needs
  `CleanupConfirmed`, verified `AllParticipationEnded`, or the `ReconciliationTimeout`
  disposition (ENDING for at least 5 minutes); the evidence is the Call's
  `terminal_reason` and is recorded in the event.
* **Terminal is final.** Rust refuses every action on a terminal Call; the database
  refuses any write that changes a terminal state, skips the graph, or changes the
  version without a state change. Late lifecycle, session and leg commands cannot
  resurrect a Call. Terminal transitions close open connection intervals
  (`CALL_TERMINATED`) and end sessions and participants.
* **Failure categories** are neutral: `POLICY TARGET_UNAVAILABLE ROUTING TIMEOUT MEDIA
  NETWORK EXECUTION INTERNAL UNKNOWN`. Provider error codes never enter the call domain.
* **Paid public-network guard.** `require_public_network_authorization(scope, call)`
  returns a `PublicNetworkAuthorization` proof only for an authorized, executable
  (not ENDING / terminal) Call whose effective decision is APPROVED in a live-telecom
  environment. It executes nothing; future execution commands require the proof.

### Participants, sessions and participation time

* A participant is a logical identity — a `ResolvedPrincipal` of the caller's own scope
  (foreign principals are reported as not found; the database pins principal scope and
  kind snapshot). Exactly one participant per principal per Call: adding it again is
  idempotent; concurrent additions cannot duplicate it. Display names are bounded,
  normalized presentation metadata (no control / bidi characters, no telephone-number
  content) and never identify or authorize anyone.
* A session is one device / network participation. Its media identity is generated by
  the server (`cfmid_` + 256 random bits) and never accepted as input. A participant may
  hold several sessions at once.
* Reconnect is explicit: `reconnect_session` renews the same session with connection
  interval n + 1; `replace_session` ends a session whose device / connection identity
  changed and creates a new session for the same participant. Neither changes the Call
  or the participant.
* Participation time comes from intervals: per session the sum, per participant the
  **union** across its sessions (concurrent devices are not double counted), open
  intervals up to the query instant.

### Events and the transactional outbox

Every mutation appends immutable `CallEvent`s (`CALL_REQUESTED`, `CALL_STATE_CHANGED`,
`CALL_AUTHORIZATION_RECORDED`, participant / session / route / leg / attempt / quality /
usage facts) and one `calling_outbox_events` row per event, in the same transaction as
the mutation. The outbox row is a `cfmeet.platform.event-envelope` (event type
`calling.<event-type>`, e.g. `calling.call-state-changed`) whose payload is a
[`cfmeet.calling.call-lifecycle-event`](../../packages/contracts/calling/events/call-lifecycle/v1.schema.json).
The database refuses `UPDATE` / `DELETE` / `TRUNCATE` of events, sequence gaps, events
that misdescribe the Call, a new Call version without its state event and an event
without its outbox row. The Part G publisher (ADR 0028) claims `PENDING` rows with a
lease, hands them to the broker-neutral `EventTransport` (today: projection into customer
webhooks) and marks them `PUBLISHED`, or `DEAD_LETTER` after bounded retries (operator
replay returns them to `PENDING`).

### Concurrent-call limit

Creating a Call takes a transaction-scoped advisory lock per application (preceded by the
tenant's lock when the tenant configures a ceiling) and counts non-terminal Calls against
the effective per-credential limit and, when configured, the application-wide and
tenant-wide limits. Terminal Calls stop counting.

### Read API

`GET /v1/calls` (keyset pagination by `createdAt DESC, callId DESC`, `limit` 1–100,
opaque `cursor`, filters `state`, `createdAfter`, `createdBefore`,
`initiatorPrincipalId`; unknown or repeated parameters → 400) and
`GET /v1/calls/{call_id}` (participants, session summaries, durations). Both require
`calls.read`; scope comes only from the credential; a foreign or malformed id answers
exactly like a missing one (404). Responses are `Cache-Control: no-store` and never
contain media identities, execution bindings, provider data, credentials, principal
references or telephone numbers.

## Application calling (Part C §20–§25)

### Flow

1. The application backend registers each device as an endpoint
   (`POST /v1/calling/endpoints`) and mints a short-lived signaling credential for it
   (`POST /v1/calling/signaling-credentials`); the device opens
   `GET /v1/calling/signaling` with it (header, or subprotocols
   `cfmeet.signaling.v1, bearer.<credential>` in browsers). Devices never see the API
   credential.
2. `POST /v1/calls` (`calls.write`, `Idempotency-Key`) with `kind`, the initiator's
   principal and endpoint and the target principals. Calling authorizes (APP_DIRECT_V1),
   resolves targets in scope, records one APPLICATION route / RTC leg / attempt,
   ensures the Call's media resource `cfc-<call id>` at the provider, authorizes the
   initiator's session and rings every eligible endpoint (`call.incoming`; offers expire
   on the server, default 45 s).
3. A device accepts (`…/invitations/{id}/accept`, or the backend with
   `/v1/calls/{id}/accept`): exactly one device per participant wins; the others get
   `call.answered_elsewhere` and no session. The first winner moves the Call to
   ANSWERED.
4. Each authorized session asks for a media credential (gateway URL, join-only token
   sealed by Calling's media edge, expiry, capabilities) and connects through Calling's
   media connection gateway (`/v1/calling/media/rtc…`), which admits the exact session
   generation against the database before LiveKit is contacted (fail closed); Calling
   then applies the permissions it derives. Tokens LiveKit refreshes are re-sealed, so
   no client ever holds a token LiveKit accepts directly (ADR 0007 §5). CONNECTED needs
   admitted connections of two participants.
5. Hold / resume / transfer / session removal / end are commands with `expectedVersion`;
   the provider enforces them. END reaches ENDED only on cleanup evidence; usage is
   finalized.

Signals are sent only after the authoritative transaction commits; a reconnecting
endpoint recovers with `GET /v1/calling/invitations`. Expiry and media reconciliation
workers run on every instance (row leases / `SKIP LOCKED`).

### Tables (migrations 000010–000014)

`call_endpoints`, `call_invitations`, `call_answer_claims` (UNIQUE (call, participant):
the single-winner guarantee), `call_participant_media_permissions`,
`call_media_sessions` (private media binding), `call_commands` (idempotency),
`call_transfers`, `call_media_provider_events` (verified, deduplicated callback
evidence); `call_sessions` gained endpoint, media access (NONE → AUTHORIZED → REVOKED),
media generation, admission and removal.

### Configuration

| Variable | Default | Meaning |
|---|---|---|
| `CALLING_SIGNALING_SIGNING_KEY` | — (required, ≥ 32 chars) | Key of the signaling credentials (separate from every other key) |
| `CALLING_SIGNALING_CREDENTIAL_TTL_SECONDS` | 600 (≤ 3600) | Signaling credential lifetime |
| `CALLING_MEDIA_PROVIDER` | `none` | `livekit` to enable application media; `none` answers `CAPABILITY_UNAVAILABLE` |
| `CALLING_LIVEKIT_URL` / `CALLING_LIVEKIT_API_URL` | — | LiveKit signaling URL (gateway upstream only, never given to clients) and server API URL; TLS required except loopback |
| `CALLING_MEDIA_GATEWAY_URL` | — | Public URL of Calling's media connection gateway handed to clients (`wss://<calling host>/v1/calling/media`) |
| `CALLING_MEDIA_EDGE_SIGNING_KEY` | — (≥ 32 chars) | Seals client media credentials; LiveKit never knows it; must differ from the LiveKit secret |
| `CALLING_LIVEKIT_API_KEY` / `CALLING_LIVEKIT_API_SECRET` | — | Calling's own LiveKit key pair (secret ≥ 32 chars; never logged or returned) |
| `CALLING_LIVEKIT_API_TIMEOUT_MS` | 5000 | Provider API timeout |
| `CALLING_MEDIA_CREDENTIAL_TTL_SECONDS` | 300 (30–900) | Media join credential lifetime |
| `CALLING_INVITATION_TTL_SECONDS` | 45 (10–300) | Ring offer lifetime |
| `CALLING_MAX_GROUP_PARTICIPANTS` | 16 (2–64) | Participants per Call, initiator included |
| `CALLING_MAX_ENDPOINTS_PER_PARTICIPANT` | 10 (1–50) | Devices rung per participant |
| `CALLING_MEDIA_RECONCILE_INTERVAL_SECONDS` | 15 (2–300) | Media reconciliation interval |

Invalid or partial media / signaling configuration stops the service at startup (exit 2).

## Enterprise calling (Part F §52–§62)

Enterprise calling lives in this service (`src/enterprise/` configuration,
`src/calls/app/enterprise/` runtime): an enterprise call is an ordinary canonical Call
routed over `ENTERPRISE_INTERNAL`. There is no PBX service, no second Call model and no
Meeting Service dependency; Telephony only executes edge primitives.

| Section | What | Decision |
|---|---|---|
| §52–§53 | Typed enterprise call policy, departments, admin audit, enterprise scopes, kill switches; Tenant is the organization boundary | ADR 0018 |
| §54 | Extensions (scoped namespace), extension-to-extension Calls, enterprise SIP desk devices offered by Telephony | ADR 0018, 0020 |
| §55 | Ring groups: SIMULTANEOUS / ORDERED, group single winner, bounded fan-out, fallback | ADR 0019 |
| §56 | Queues: FIFO, authoritative positions, one offer per entry / agent, timeouts, overflow, abandonment | ADR 0019 |
| §57 | Declarative versioned IVR (keys never stored or logged), built-in prompts, RTC keypad and SIP DTMF | ADR 0020 |
| §58 | Business hours (IANA zones, DST, exceptions), typed forwarding, loop / hop protection; public forwarding only through every Part E gate | ADR 0021 |
| §59 | Hold / resume / transfer to enterprise targets on the same Call | ADR 0022 |
| §60 | Voicemail: mailboxes, edge (SIP) / LiveKit Egress (RTC) capture, protected object storage (`packages/object-storage`), READY, authorized signed retrieval, read state, deletion, retention | ADR 0023, 0027 |
| §61 | Call recording: separate policy, server-issued notice, per-participant consent, immutable decisions, bridge (SIP) / audio-only room-composite (RTC) capture, protected artifacts, audited access | ADR 0024, 0027 |
| §62 | Missed calls and history derived per logical participant from Call evidence; `GET /v1/calls?principalId=`, `GET /v1/calling/history` | ADR 0025 |
| cross-cutting | Durable `ENTERPRISE_ROUTING_STEP` outbox events, `calling_incidents`, bounded metrics, races, failure injection, load | ADR 0026 |

Kill switches (default off, stop NEW effects only — never termination, cleanup,
finalization, deletion or retention): `ENTERPRISE_CALLING_ENABLED`, `VOICEMAIL_ENABLED`,
`CALL_RECORDING_ENABLED`. Scopes: `enterprise.config.read|write`, `enterprise.calls.use`,
`enterprise.voicemail.read|manage`, `enterprise.recording.read|manage` — never implied by
`calls.*`.

**Application (RTC) capture** (ADR 0027): `CALLING_CAPTURE_PROVIDER=livekit-egress`
(default `none`) captures application-only Calls through LiveKit Egress — voicemail as a
track egress of the caller's microphone only, recording as an audio-only room composite —
into the private staging prefix `CALLING_CAPTURE_STAGING_PREFIX` (default
`capture-staging`); a worker pass collects, verifies and stores it through the same
protected-artifact path as SIP captures. With `none`, RTC voicemail / recording are
refused (`CAPTURE_UNAVAILABLE`), never faked. Recording length is the policy's
`maxRecordingSeconds` (60–1440), bounded by the single-artifact delivery ceiling.

**Not offered / pending (recorded, never faked):** public forwarding to a real carrier
and real PSTN results (Part E external blocker); transfer of an established Call to a
public contact; keypad consent from SIP desk phones (policies that need it deny
recording).
Real-stack certification (`tests/enterprise_sip_certification.rs`, workflow
[`enterprise-calling-certification.yml`](../../.github/workflows/enterprise-calling-certification.yml))
drives a real Asterisk edge, SIPp desk phones, the real Telephony binary and an
S3-compatible store; it certifies internal enterprise SIP only. Results:
[Part F certification report](../../docs/architecture/part-f-certification.md).

## Platform events, webhooks, usage and limits (Part G §63–§70)

* **Durable events** — the outbox publisher (lease, bounded retry, `DEAD_LETTER`, replay)
  runs in the service loop with the webhook delivery worker; Privacy Relay pushes its own
  outbox to `POST /internal/v1/platform/events` (`platform-event.publish`).
* **Customer webhooks** — `/v1/webhooks` (create / list / get / update / delete /
  rotate-secret, `webhooks.read` / `webhooks.manage`), `/v1/webhooks/{id}/deliveries`,
  `/v1/webhook-deliveries/{id}/replay`. Events are the public catalog
  ([`platform/webhooks`](../../packages/contracts/platform/webhooks/v1.schema.json)),
  projected from committed domain events, signed `CF-Webhook-Signature: v1=…`
  (HMAC-SHA256 over `v1:{timestamp}:{delivery id}:{body}`), retried with backoff,
  dead-lettered and replayable; endpoints are https-only and private / rebinding
  addresses are refused at connection time. The signing secret is shown once.
* **Usage** — `GET /v1/usage` (`usage.read`): final facts (application media,
  public-network legs with KNOWN / ESTIMATED / UNKNOWN cost evidence, voicemail /
  recording artifacts); filters `from`, `to`, `usageType`, `resourceClass`; keyset paging.
  Not a bill ([`platform/usage`](../../packages/contracts/platform/usage/v1.schema.json)).
* **Limits** — see *Limits*; `GET /v1/limits` (`usage.read`).
* **Configuration** — `CALLING_WEBHOOK_SECRET_KEY` (32 bytes, base64, from the secret
  manager; without it webhook creation is refused) and
  `CALLING_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS` (honoured only when `CF_RUNTIME_ENVIRONMENT=local`).

## Privacy Relay integration (Part D §26–§30)

Privacy Relay (`services/privacy-relay`, [ADR 0010](../../docs/decisions/0010-communication-context-and-privacy-relay-session-architecture.md))
is the authority for Communication Contexts and Privacy Relay Sessions. Calling holds
none of that data and never reads Privacy Relay's database; it talks to it over signed
internal HTTP (`CF_PRIVACY_RELAY_BASE_URL`; without it every privacy operation fails
closed with 503).

* **Ingress** (`src/privacy_relay/http.rs`, contract
  [`privacy-relay/http`](../../packages/contracts/privacy-relay/http/v1.openapi.json)):
  thin handlers for `/v1/privacy-relay/contexts…` and `/v1/privacy-relay/sessions…`.
  Scopes `privacy.contexts.read|write`, `privacy.sessions.read|write`;
  `Idempotency-Key` on every mutation; named principals must be ACTIVE principals of the
  caller's environment; the request is forwarded with a delegated platform context —
  never the customer credential. Privacy Relay's public error codes are relayed; internal
  or unknown ones become `SERVICE_UNAVAILABLE`.
* **Protected calls** (`src/calls/app/privacy.rs`): `POST /v1/calls` with
  `privacyRelaySessionId` (VOICE) asks Privacy Relay `communication.authorize`
  (`CALL_CREATE`); a transfer of a protected call asks again for the new target
  (`CALL_TRANSFER`). Denied → 403 `CALL_NOT_AUTHORIZED`; unknown session → 404;
  unreachable / malformed → 503 `PRIVACY_AUTHORIZATION_UNAVAILABLE`. The decision relied
  on is recorded in `call_privacy_authorizations` (ids and versions only).
* **Enforcement** (`src/privacy_relay/enforcement.rs`):
  `POST /internal/v1/privacy-relay/enforcement` requires a Privacy Relay service token
  with `communication-authority.enforce` (granted in Calling's trust bundle) and a
  delegated scope. Commands are deduplicated by `enforcementEventId`
  (`call_privacy_enforcements`); matching protected calls of that scope (decision at or
  below the command's version, affected principal involved) are cancelled before answer
  and ended (ENDING → cleanup → ENDED) after. An advisory lock per context serializes
  enforcement with protected-call creation and transfer, and a decision already
  superseded by a received enforcement is refused.

Migration 000015 adds the two evidence tables.

## Internal service authentication

```text
customer request ─► PlatformAuthContext ─► delegated_context() ─► DelegatedPlatformContext
                                                                     │ signed into
peer service ◄─ X-CF-Service-Authorization: Bearer <EdDSA JWS> ◄──────┘ the service token
                X-CF-Request-Id (new per hop) / X-CF-Correlation-Id (stable per operation)
```

* Calling signs as `cfmeet.calling` with its own key (`CF_SERVICE_AUTH_KEY_ID`,
  `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`); tokens are bound to exactly one audience and one
  runtime environment, live 120 s by default (≤ 300 s) and carry no permissions.
* Inbound, Calling verifies against its trust bundle (`CF_SERVICE_TRUST_BUNDLE_FILE`):
  pinned algorithm and token type, known issuer and `kid`, exact audience, runtime
  environment, lifetime, and header ids equal to the signed ids. Permissions come only
  from Calling's own bundle (`InternalCaller::require_permission`). Calling grants
  `operational-callback.deliver` at most, and it is never granted
  `protected-contact.resolve-for-execution` by any receiver.
* Outbound helpers: `internal_auth::delegation::{delegated_context,
  outbound_for_customer, outbound_next_hop}` and `ServiceAuthRuntime::client()` (HTTPS in
  staging / production, no redirects, no proxies, `/internal/v1/` paths only).

ADR: [0004 — Internal service authentication](../../docs/decisions/0004-internal-service-authentication.md).

## Model

```text
Tenant (FIRST_PARTY | EXTERNAL; ACTIVE | SUSPENDED)
└── Application (ACTIVE | SUSPENDED) ── allowed scopes ── limit policy
    └── Environment (SANDBOX | TEST | PRODUCTION; ACTIVE | SUSPENDED; live_telecom_allowed)
        ├── Service account (ACTIVE | SUSPENDED | REVOKED)
        │   └── API credential (ACTIVE | REVOKED; validity window; rotation links) ── granted scopes ── limit policy
        └── Principal (7 categories, opaque reference; ACTIVE | SUSPENDED | DEACTIVATED)
```

* Every child row carries its whole ownership chain and references its parent with a
  composite foreign key on `(…ownership…, id)`, so a row can never point at a parent of
  another tenant / application / environment. Ownership columns are immutable (trigger).
* An application has exactly one environment of each kind. SANDBOX and TEST can never
  have `live_telecom_allowed = true` (validated and `CHECK`-enforced); PRODUCTION starts
  with it disabled.
* FIRST_PARTY is a trust classification only. CosmicForge products and external
  customers are ordinary applications; privileges exist only as explicit scopes.

## API credentials

Format: `cfmk_<sbx|test|live>_<public id: 24 hex>_<secret: 64 hex>`.

* The secret is 32 bytes from the OS CSPRNG. The environment tag is convenience only; it
  grants nothing and is bound into the verifier, so an edited tag never verifies.
* The database stores `HMAC-SHA-256(API_CREDENTIAL_PEPPER, context ‖ tag ‖ public id ‖
  secret)` plus algorithm and pepper version — never the secret. Verification is
  constant time. The raw credential is returned once, at creation or rotation.
* Authentication (`Authorization: Bearer <credential>`) loads the credential's full
  authority chain in one query and requires: verifier match, credential ACTIVE, not
  revoked, inside its validity window and rotation overlap, and service account,
  environment, application and tenant all ACTIVE. No caches: suspension and revocation
  apply to the next request. Every failure returns the same `AUTHENTICATION_FAILED` 401.
* Scopes come from a server-side registry, must be allowed for the application
  (foreign-key enforced) and are loaded from the database at authentication; requests
  cannot add scopes. `PlatformAuthContext::require_scope` is the check for handlers.
* Rotation issues an independent successor for the same service account (scopes and
  credential limits preserved) and limits the predecessor to `min(original expiry,
  now + overlap)`. Revocation is immediate and idempotent.

## Principals

`PlatformAuthContext` says which application / service account is calling.
`PrincipalResolver::resolve(ctx, reference)` resolves *who takes part* — only inside the
context's tenant, application and environment; a principal elsewhere is reported exactly
like a nonexistent one. Authentication never creates or implies a principal; a service
account is a principal only if explicitly registered as a linked SERVICE_ACCOUNT
principal.

Categories: `NATIVE_CF_USER`, `EXTERNAL_APPLICATION_PRINCIPAL`, `ENTERPRISE_MEMBER`,
`SERVICE_ACCOUNT`, `TELEPHONE_ENDPOINT`, `TEMPORARY_EXTERNAL_PARTICIPANT`,
`BOT_OR_SERVICE`. References are opaque; no plaintext contact data is stored —
E.164-style and `tel:`/`sip:` values are refused, and TELEPHONE_ENDPOINT references must
be opaque UUID handles (for the future Contact Vault).

## Limits

Tenant-, application- and credential-level limits (requests per minute, concurrent
communications, daily / monthly telecom spend in minor units of the tenant's billing
currency) are set by operators only (`platform_admin limits set-tenant | set-application |
set-credential`). The effective policy takes the most restrictive configured value
(tenant → application → credential), otherwise the platform default; a configured tenant
or application level is also an aggregate ceiling. Telecom spend defaults to zero and is
always zero where live telecom is not allowed. The request-rate limit is enforced (shared
PostgreSQL one-minute windows, per credential, application and tenant), the
concurrent-communication limit when a Call is created (see *Concurrent-call limit*) and
telecom spend when a public leg is reserved. `GET /v1/limits` shows every level and the
binding one; `limit.warning` / `limit.exceeded` webhooks are deduplicated per window
(ADR 0028).

## Configuration

See [`.env.example`](.env.example). Required: `DATABASE_URL`, `CALLING_SIGNALING_SIGNING_KEY`, `API_CREDENTIAL_PEPPER`
(≥ 32 random bytes, e.g. `openssl rand -base64 48`; placeholder values are rejected and
there is no fallback), `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_AUTH_KEY_ID`,
`CF_SERVICE_AUTH_PRIVATE_KEY_FILE` and `CF_SERVICE_TRUST_BUNDLE_FILE`. The service exits
at startup if its identity, key, trust bundle or runtime environment is missing or
inconsistent. The Calling service owns its own database.

## Running locally (no Docker required)

```bash
cp .env.example .env              # set DATABASE_URL, API_CREDENTIAL_PEPPER, CALLING_SIGNALING_SIGNING_KEY
# A local signing key (outside the repository) and a trust bundle with no peers yet:
mkdir -p ~/.cfmeet && cargo run -q --manifest-path ../../packages/service-auth/Cargo.toml \
  --bin cf-service-keygen -- generate --kid calling-local-1 --out ~/.cfmeet/calling-local-1.pem
echo '{"bundle_version":1,"receiver":"cfmeet.calling","runtime_environment":"local","trusted_issuers":[]}' \
  > ~/.cfmeet/calling-trust-bundle.json
# then set CF_SERVICE_AUTH_PRIVATE_KEY_FILE / CF_SERVICE_TRUST_BUNDLE_FILE in .env
cargo run -p calling-migration -- up
cargo run --bin calling
curl http://127.0.0.1:8090/health
```

From the repository root: `make calling-migrate`, `make calling`.

## Administration

`platform_admin` uses the same domain services as future administration APIs. There are
no public credential-management endpoints.

```bash
A="cargo run -q --bin platform_admin --"
$A tenant create --slug acme --name "Acme" --classification external
$A application create --tenant acme --slug dispatch --name "Dispatch"
$A environment create --tenant acme --application dispatch --environment production
$A service-account create --tenant acme --application dispatch --environment production --name backend
$A scope grant --tenant acme --application dispatch --scope principals.read
$A credential create --tenant acme --application dispatch --environment production \
   --service-account backend --scope principals.read      # prints the secret ONCE
$A credential rotate --credential <public id> --overlap-minutes 60
$A credential revoke --credential <public id>
$A principal register --tenant acme --application dispatch --environment production \
   --kind external-application-principal --reference user-123
$A limits set-credential --credential <public id> --requests-per-minute 120
$A limits effective --credential <public id>
```

`$A --help` lists every command (suspend / reactivate for tenants, applications,
environments, service accounts and principals; `environment set-live-telecom`; scope
registry; credential show / list). Output is JSON on stdout; audit lines go to stderr.

## Tests

```bash
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test --workspace --no-fail-fast
```

Each test clones its own database from a migrated template (no Docker needed). Without
`TEST_DATABASE_URL` the database suites fail, unless `CFMEET_SKIP_DB_TESTS=1` is set
(explicit SKIPPED lines). Suites: `migrations`, `tenant_isolation`,
`first_party_external`, `credential_storage`, `authentication`, `environment_isolation`,
`scopes`, `rotation`, `secret_leakage`, `principals`, `limits`, `admin_cli`,
`architecture_boundaries`, `internal_auth` (service-token authentication on
`/internal/v1/*`), `outbound` (delegated context and the internal HTTP client over
loopback HTTP), `http_contract` (conformance with `packages/contracts`, including call
responses, call vocabularies and every outbox envelope / lifecycle payload) and the
Calling core suites `call_state_machine`, `call_concurrency` (repeated races),
`call_authorization`, `call_participation`, `call_evidence` (events, outbox, injected
failures), `call_records` and `call_api`; application calling: `app_calls` (voice /
video end to end), `app_ringing` (endpoints, signaling credentials, offers, expiry,
decline semantics, group arbitration), `app_races` (repeated concurrent accepts, group
contention, accept vs cancel, duplicate commands, the DB-level single-winner mutation,
multi-application load, fan-out / credential / reconciliation bursts), `app_media`
(provider contract on the test provider, credential security, callbacks as evidence,
reconciliation, privacy of stored evidence), `app_admission` (the gateway's admission
authority for every token case, fail closed), `app_control` (cancel / end / hold /
resume / transfer, idempotency, versions, provider failures) and `signaling` (real
WebSocket, subprotocol auth, expiry close 4001, recovery, two-instance LISTEN / NOTIFY
fan-out); Part D: `privacy_protected_calls` (protected creation / transfer, fail
closed, superseded decisions), `privacy_enforcement` (authentication, cancel / end,
replays and concurrent duplicates, stale versions, scope isolation),
`privacy_relay_gateway` (the ingress against a mock Privacy Relay that verifies service
tokens: forwarding, delegated scope, no customer credential, edge validation, error
relay, outages) and `privacy_relay_e2e` (against the real `privacy-relay` binary with
its own database; skipped unless `PRIVACY_RELAY_BIN` / `PRIVACY_RELAY_MIGRATION_BIN`
are set — `make privacy-relay-integration`, CI job `calling-e2e` in
[`.github/workflows/privacy-relay.yml`](../../.github/workflows/privacy-relay.yml)).
Part E: `routing_engine`, `public_*`, `telephony_e2e`. Part F: `enterprise_foundation`,
`enterprise_extensions`, `enterprise_ring_groups`, `enterprise_queues`, `enterprise_ivr`,
`enterprise_hours_forwarding`, `enterprise_transfer`, `enterprise_voicemail`,
`enterprise_recording`, `call_history`, `enterprise_operations` (outbox events,
incidents, metrics, failure injection), `enterprise_security` (scope matrix, contract
scans), `enterprise_load` (measured throughput with burst invariants; scale with
`ENTERPRISE_LOAD_SCALE`), `enterprise_certification` (§212 organization isolation) and the real-stack `enterprise_sip_certification` (skipped
unless `ENTERPRISE_SIP_CERT_REQUIRED=1` and the Telephony binaries / Asterisk / SIPp are
present).

Real-provider certification (`rtc_certification`) runs against a real LiveKit server and
real WebRTC clients (`tests/rtc/rtc_client.py`); it is skipped unless the
`CALLING_RTC_CERT_*` variables are set (`CALLING_RTC_CERT_REQUIRED=1` turns a missing
server into a failure). CI:
[`.github/workflows/calling-rtc-certification.yml`](../../.github/workflows/calling-rtc-certification.yml);
locally see [`infra/livekit/README.md`](../../infra/livekit/README.md) (no Docker needed).

CI: [`.github/workflows/calling.yml`](../../.github/workflows/calling.yml).

## Container image

```bash
# from the repository root: the image also needs packages/service-auth
docker build -f services/calling/Dockerfile -t cfmeet-calling .
```

The image runs as a non-root user and contains `calling`, `calling-migration` and
`platform_admin`. All configuration is supplied at runtime.

## Decisions

* [ADR 0003 — Calling platform consumer, credential and principal trust model](../../docs/decisions/0003-calling-platform-trust-model.md)
* [ADR 0004 — Internal service authentication](../../docs/decisions/0004-internal-service-authentication.md)
* [ADR 0005 — Contract authority and runtime boundaries](../../docs/decisions/0005-contract-authority-and-runtime-boundaries.md)
* [ADR 0006 — Canonical Call aggregate and state machine](../../docs/decisions/0006-canonical-call-aggregate-and-state-machine.md)
* [ADR 0007 — Calling media provider boundary](../../docs/decisions/0007-calling-media-provider-boundary.md)
* [ADR 0008 — Endpoints, ringing and answer arbitration](../../docs/decisions/0008-calling-endpoints-ringing-and-answer-arbitration.md)
* [ADR 0009 — Call control, idempotency and transfer](../../docs/decisions/0009-calling-call-control-idempotency-and-transfer.md)
* [ADR 0010 — Communication Context and Privacy Relay Session architecture](../../docs/decisions/0010-communication-context-and-privacy-relay-session-architecture.md)
* [ADR 0013 — Routing Engine and route plans](../../docs/decisions/0013-routing-engine-and-route-plans.md) … [ADR 0017 — Provider failover](../../docs/decisions/0017-provider-failover.md) (Part E)
* [ADR 0018 — Enterprise calling architecture](../../docs/decisions/0018-enterprise-calling-architecture.md) … [ADR 0026 — Enterprise operations](../../docs/decisions/0026-enterprise-operations-events-incidents-metrics.md), [ADR 0027 — RTC call-audio capture](../../docs/decisions/0027-rtc-call-audio-capture-livekit-egress.md) (Part F)
* [ADR 0028 — Platform API, events, webhooks, usage and limits](../../docs/decisions/0028-platform-api-events-webhooks-usage-and-limits.md) (Part G)
* [Service data ownership](../../docs/architecture/service-data-ownership.md)
