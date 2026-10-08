# ADR 0012: Temporary relay-number assignment, cooldown and inbound mapping

- **Status:** Accepted
- **Date:** 2026-10-08
- **Scope:** CF Meet Part D §34–§35; implemented in `services/privacy-relay/src/relay_numbers/`
  (Privacy Relay migrations 000009–000011), the operator CLI
  `privacy-relay-admin relay-number …`, and the contracts
  `cfmeet.privacy-relay.relay-number` and `.relay-routing`.

## Context

A protected public-network conversation needs a public number between the parties: the
caller sees CF Meet's relay number, never the other side's private number, and a call
back to that relay number must reach the right Relay Session — and only while that
session's authority lasts. Part III needs this before a Telephony service or a
numbering product exists, so it must stay a small, temporary piece of relay
infrastructure that a later Numbering capability can replace.

## Decision

### Temporary Part III scope, not Numbering

The relay-number pool lives inside Privacy Relay. It is not a numbering product: no
customer-owned or permanent numbers, no porting, no marketplace, no provisioning API,
no carrier SDK. Numbers are platform-owned PUBLIC relay identities managed by an operator
with `privacy-relay-admin relay-number add | activate | enable | drain | disable | list`
(no network admin API). A future Numbering service will own the permanent public-number
lifecycle; Telephony will own carrier legs and provider callbacks.

### Inventory

`relay_number_inventory`: canonical public E.164 (same parser as contacts; unique),
country / region, provisioned capabilities (VOICE_INBOUND, VOICE_OUTBOUND only — nothing
implies SMS or messaging), an opaque internal `provider_binding_ref` (unique, never
containing the number, never selected by any query, never in an API, event or log), the
operational status, a monotonic `generation`, and drain / cooldown / unavailability
stamps. Status graph (server-controlled, enforced by a trigger):

```
RESERVED → AVAILABLE → ASSIGNED → COOLDOWN → AVAILABLE
  ↘ DRAINING ↙ (from RESERVED / AVAILABLE / ASSIGNED / COOLDOWN) → UNAVAILABLE → RESERVED
  any of them → UNAVAILABLE (disable)
```

The generation advances by exactly one on AVAILABLE → ASSIGNED and never otherwise. A
number held by an assignment (ACTIVE or COOLDOWN) can never become AVAILABLE or RESERVED
(database trigger). `relay_number_events` is the append-only operational ledger of every
transition (no tenant data, no number); the number itself is shown only masked.

### Assignment lifecycle

`relay_number_assignments` lends one number to ONE Privacy Relay Session of one tenant /
application / environment / context, at the number's generation:

```
ACTIVE ──(release | session revoked / completed beyond grace / expired | context cascade
          | narrowing past now | own expiry | number disabled)──▶ COOLDOWN
COOLDOWN ──(cooldown elapsed, worker)──▶ RELEASED | REVOKED | EXPIRED   (terminal)
```

The end reason decides the terminal state. Release is atomic — routing authority ends and
the cooldown starts in the same transaction as the session change — so no separate
RELEASING state is observable. Terminal rows never change again; a new assignment is a
new row. The session's `relay_identity_binding_ref` is the assignment id (set once,
trigger-checked to name this session's own assignment): one relay identity per session
for its whole life.

### Deterministic, concurrency-safe allocation

Calling (`relay-number.assign`) requests an assignment for a session with country,
optional region and required capabilities. Privacy Relay locks the context, then the
session (serializing every request for that session: a replay with the same criteria
answers the same assignment, different criteria are IDEMPOTENCY_CONFLICT), checks
PRIVACY_RELAY_ENABLED and RELAY_NUMBER_POOL_ENABLED, an ACTIVE context and session
inside their windows, a public / callback relay strategy and that every protected contact
of the session is enabled and effectively VERIFIED, then claims the least recently
changed eligible number with `FOR UPDATE SKIP LOCKED` (never random-then-retry). Lock
order everywhere: context → session → assignment → inventory; allocation never waits on
an inventory row. The database independently guarantees one holder per number
(`uq_relay_assignments_holder` on ACTIVE / COOLDOWN), containment in the session's final
authority (insert trigger; narrowing clamps it in the same transaction; validity only
shrinks), the inventory row ASSIGNED at exactly that generation, and immutability.

### Cooldown

`RELAY_NUMBER_COOLDOWN_SECONDS`, default 259200 (72 h), accepted range 1 h – 90 days, zero
never accepted. A released number keeps receiving delayed callbacks, carrier retries and
call-backs to a remembered caller id; it is not offered to anyone until the cooldown
worker (multi-instance safe, `SKIP LOCKED`) finishes it. `RELAY_NUMBER_POOL_ENABLED=false`
(default) refuses NEW assignments only; release, expiry, cooldown and drain always run,
and app-only Privacy Relay keeps working.

### Inbound mapping (§35)

Telephony only (`relay-number.resolve-inbound`, no delegated context — the mapping is the
scope) presents the called relay number and safe provider metadata. Privacy Relay
canonicalizes it, requires exactly one inventory row and exactly one ACTIVE assignment
(more is a fail-closed incident logged to the ledger, never a choice), matching
assignment id / generation when the provider carries them, an event that is neither
stale (15 min) nor from the future (2 min skew) and does not originate before the
current assignment, an unexpired assignment bound to its session, and CALLBACK_RELAY
authority of that session and context now (the shared authorization evaluation). It
returns only references, versions, the protected contacts that may call in with their
allowed targets, and the validity boundary; everything else — unknown, cooldown,
released, revoked, expired, mismatched — is the generic RELAY_MAPPING_NOT_ACTIVE, which
reveals no previous destination. Every attempt is recorded in `relay_inbound_resolutions`
without any number.

### Delayed callback protection

Two independent layers: the cooldown (no session at all can be reached during it), and
generation / assignment / origin-time checks (an event of an earlier assignment cannot
reach the next session after the cooldown). Providers that only present the called
number rely on the cooldown alone; for them the cooldown is a mandatory provider-specific
operational input that must exceed the provider's certified delayed-callback / retry
window — Part E provider certification must prove it.

### Events

The tenant-visible part of the lifecycle is relay audit + outbox (`RELAY_NUMBER_ASSIGNED`,
`_RELEASED`, `_COOLDOWN_STARTED`, aggregate RELAY_SESSION) carrying the assignment id and
generation only; inventory-only transitions (`_AVAILABLE`, `_DRAINED`, …) stay in the
platform ledger.

## Consequences

- A public relay identity can be lent, recognized on the way back and retired safely
  before Telephony exists; nothing is dialled or received yet.
- Pool size and cooldown trade off: a long cooldown needs more numbers.
- One identity per session means a disabled number ends that session's public reach; the
  consumer opens a new session.
- Telephony (Part E) will originate with the relay number, call `resolve-inbound` and
  `protect-inbound-caller`, and certify each provider's callback window against the
  cooldown.
