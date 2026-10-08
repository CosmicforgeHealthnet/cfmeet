# Part E contract inventory (Stage 0 architecture freeze)

The internal contracts Part E (§37–§51) adds, and the single authority behind each one.
No contract here duplicates an authority another service already owns. Every hop is
authenticated with `packages/service-auth` (Ed25519 service tokens, receiver-authoritative
permissions), and no customer credential (`cfmk_…`), credential verifier or pepper is ever forwarded.

## Authorities (unchanged by Part E)

| Concept | Authority | Others hold |
|---|---|---|
| Logical Call, participants, sessions, canonical CallLeg, CallAttempt, CallRoute / RoutePlan, call authorization, call-level usage, spend reservations | **Calling** | opaque ids only |
| Communication Context, Relay Session, Contact Vault, verification, Contact Resolution Grants, relay-number inventory / assignments, inbound relay mapping | **Privacy Relay** | opaque ids only |
| Public-network execution: TelephonyLeg, SIP dialogs, provider bindings, provider events, telephony commands, bridges, provider health / capacity / circuit, unknown outcomes, reconciliation, provider usage / cost evidence | **Telephony** (new, `services/telephony`) | opaque `telephony_leg_id` (Calling's `call_legs.execution_binding_ref`) |

## 1. Calling → Telephony (commands; audience `cfmeet.telephony`)

| Route | Permission | Purpose |
|---|---|---|
| `POST /internal/v1/legs/originate` | `telephony.leg.originate` | durable ORIGINATE of one authorized public leg (`commands/v1` OriginateCommand) |
| `POST /internal/v1/legs/{id}/terminate` | `telephony.leg.terminate` | TERMINATE |
| `POST /internal/v1/legs/{id}/hold`, `/resume` | `telephony.leg.hold`, `telephony.leg.resume` | HOLD / RESUME primitives |
| `POST /internal/v1/bridges`, `DELETE /internal/v1/bridges/{id}` | `telephony.bridge.manage` | BRIDGE / UNBRIDGE (public legs and the application-media attachment) |
| `GET /internal/v1/legs/{id}`, `GET /internal/v1/routes/availability` | `telephony.status.read` | normalized leg status; route health / capacity for the Routing Engine |

Every command carries `schemaVersion`, `commandId`, `idempotencyKey`, Call / CallLeg /
attempt / route ids, the call-authorization evidence, Privacy Relay decision evidence,
the spend reservation, the correlation id and the expected generation. Telephony refuses
a missing or stale proof. A repeated `commandId` has exactly one provider effect.

## 2. Telephony → Calling (normalized execution facts; audience `cfmeet.calling`)

| Route | Permission | Purpose |
|---|---|---|
| `POST /internal/v1/telephony/events` | `calls.telephony-event.apply` | normalized leg / bridge events (`telephony/events/v1`), delivered from Telephony's outbox; Calling validates and applies them through its own state machine |
| `POST /internal/v1/telephony/reconciliations` | `calls.telephony-reconciliation.apply` | normalized reconciliation results |
| `POST /internal/v1/telephony/inbound-calls` | `calls.telephony-inbound.offer` | an authenticated inbound public call with its RESOLVED relay mapping and the caller's ContactReference; Calling creates the ONE canonical Call |

Telephony never writes Calling state. Calling dedupes by event id and never lets an
event resurrect a terminal Call.

## 3. Telephony → Privacy Relay (existing Part D contracts, Telephony-only)

| Route | Permission |
|---|---|
| `POST /internal/v1/contacts/{ref}/resolve-for-execution` | `protected-contact.resolve-for-execution` (returns the destination E.164 **and** the session's authorized relay caller identity, selected server-side) |
| `POST /internal/v1/relay-routing/resolve-inbound` | `relay-number.resolve-inbound` |
| `POST /internal/v1/relay-routing/protect-inbound-caller` | `protected-contact.protect-inbound` |

The only Part E evolution: resolution also returns the presented caller identity (the relay
number of the session's ACTIVE assignment). Clients can never choose a caller ID.

## 4. Calling → Privacy Relay (existing)

`communication.authorize` (now with `PUBLIC_RELAY` / `CALLBACK_RELAY` routes and
ContactReference subjects), `protected-contact.grant-for-execution`, `relay-number.assign`.

## 5. Provider adapter boundary (inside Telephony only)

`services/telephony/src/providers/`: trait `TelecomInterconnectProvider` (capabilities,
originate, terminate, query, health, capacity, verify_event, normalize_event) returning
`ACCEPTED | DEFINITE_FAILURE | UNKNOWN_OUTCOME`. Provider ids, payloads and status strings
never leave this directory except as restricted Telephony evidence.

## 6. SIP / media edge boundary (inside Telephony only)

`services/telephony/src/sip/`: trait `SipMediaEdge` (originate, accept inbound, terminate,
bridge, unbridge, hold, resume, DTMF, query, list, health). The control-plane adapter (ARI)
is private to that directory. Calling sees only an opaque **media join code** for attaching
application RTC media to a bridge. Its media provider adapter turns that code into a
provider-specific SIP participant; no SIP types exist in Calling.
