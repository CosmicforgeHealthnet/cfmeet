# ADR 0008: Endpoints, ringing and per-participant answer arbitration

- **Status:** Accepted
- **Date:** 2026-10-07
- **Scope:** CF Meet Part C §20, §31–§33, §82, §86–§88; implemented in
  `services/calling/src/endpoints/`, `src/signaling/`, `src/calls/app/ringing.rs`,
  migrations 000010–000011

## Context

A principal may be signed in on several devices. An incoming application call must ring
them all, exactly one may answer for that person, the others must stop ringing and must
never obtain media, and a late or replayed answer must not change history. Devices
disconnect, lose signals and come back with stale state. Several Calling instances run
behind a load balancer.

## Decision

### Endpoints

`call_endpoints` registers an application endpoint (WEB, MOBILE, DESKTOP,
ENTERPRISE_APP) of one principal in one application environment, keyed by an opaque
client key (never a telephone number or tel / sip URI: Rust and CHECK constraint), with
audio / video capability, an optional opaque push target reference and status ACTIVE /
REVOKED. Registration, listing and revocation use the platform API credential with the
principal resolved inside the credential's scope (foreign: 404). Revocation cancels the
endpoint's live offers. **Presence (ONLINE / OFFLINE, last seen) only orders ringing;
it never authorizes or excludes.** Every application call needs audio: endpoints without
audio are not rung; audio-only endpoints are rung for VIDEO calls too.

### Signaling credentials

Devices never hold the platform API credential. The backend mints a short-lived Call
Signaling Credential (`cfsig_` + HS256 JWS with its own key
`CALLING_SIGNALING_SIGNING_KEY`, audience `cfmeet.calling.signaling`, TTL
`CALLING_SIGNALING_CREDENTIAL_TTL_SECONDS`, default 600, max 3600) for one ACTIVE
endpoint. Every use re-checks, in the database, the endpoint, the principal and the
minting API credential (effective, holding `calls.write`, its service account,
environment, application and tenant active). It authorizes only the endpoint's own
operations: the WebSocket channel `GET /v1/calling/signaling`, offer recovery
`GET /v1/calling/invitations`, accept / decline of its own offers, and media credentials
for its own sessions. API credentials are refused on these routes and signaling
credentials on every platform route. Time is checked on the service clock only.

### Invitations (ring offers)

`call_invitations`: one row per (Call, participant, endpoint, ring round), purpose CALL
or TRANSFER, status PENDING → RINGING → ACCEPTED | DECLINED | ANSWERED_ELSEWHERE |
CANCELLED | EXPIRED. Terminal statuses are final (transition trigger); one live offer per
Call and endpoint (partial unique index); new offers only for a non-terminal Call, an
ACTIVE endpoint and a non-ended participant (insert trigger). Expiry is server-owned
(`CALLING_INVITATION_TTL_SECONDS`, default 45, ≤ 10 min): an offer past `expires_at` is
refused with 410 `INVITATION_EXPIRED` even before the expiry worker runs, and nothing
revives it.

### Single winner per participant

Accept runs in one transaction holding the Call row lock: it re-validates the offer
(live, unexpired, own endpoint, expected version), creates the winning session and
inserts the claim

    INSERT INTO call_answer_claims … ON CONFLICT ON CONSTRAINT
      uq_call_answer_claims_participant DO NOTHING

`UNIQUE (call_id, participant_id)` is the database guarantee (proved by a mutation test
that drops it and observes two winners). An offer can become ACCEPTED only together with
its claim (trigger), and a RECIPIENT session can become media-AUTHORIZED only with a
claim on its lineage (trigger). The participant's other offers become
ANSWERED_ELSEWHERE in the same transaction; losing devices have no session and can never
receive a media credential.

### Decline, no answer and group calls

* A decline ends **only that endpoint's offer**; the participant's other devices keep
  ringing.
* A participant has declined when all of its offers are declined; the Call ends DECLINED
  when every pending target declined, NO_ANSWER when every live offer expired (or was
  cancelled) without an answer.
* Group calls (at most `CALLING_MAX_GROUP_PARTICIPANTS`, default 16, initiator
  included) arbitrate per participant: one winner each. The first winning participant
  moves the Call RINGING → ANSWERED; others join the same Call and media resource.
* After a ring outcome commits, a terminal Call's media resource is torn down
  immediately (reconciliation retries).

### Realtime signals, recovery and push

Signals (`cfmeet.calling.realtime-signal`: `call.incoming` … `call.invitation_expired`)
are derived from committed state and published **after** commit through the
`RealtimeSignalBus` (in-memory for a single process; PostgreSQL `LISTEN / NOTIFY`
across instances, payload-bounded and batched). Losing a signal never loses state: a
reconnecting endpoint calls `GET /v1/calling/invitations` (actionable only for a RINGING,
unexpired offer of an answerable Call). New offers to an endpoint with a push target
produce the transport-neutral `cfmeet.calling.push-invitation` payload for the
configured push dispatcher (no vendor integrated; no credential or number in it).
Expiry runs on every instance (`SKIP LOCKED` per Call).

## Consequences

* A device that missed signals cannot act on stale state: every command re-validates
  against the database and expected version.
* The signaling key is separate from the API credential pepper and the service-token
  key; rotating it invalidates outstanding signaling credentials only.
