# ADR 0040 — Numbering: routing grants

Status: Accepted (Part IV Workstream K). Extends ADR 0033–0039 and ADR 0004.

## Context

An ACTIVE DCN number (ADR 0039) must actually carry traffic: Telephony must learn where
a received call goes, the SMS Gateway where a message goes, and an outbound call or
message may present a number only if the customer owns it. Each of these decisions must
come from Numbering's canonical state — never from a provider identifier or a number a
request supplied — and must stop the moment that state changes. Calling and Telephony
are integrated later (Workstreams L–O); their acknowledgements and behaviour must not be
fabricated here.

## Decisions

1. **A separate credential.** A routing grant is a compact JWS of type
   `cf-number-grant+jwt` (EdDSA pinned, `kid`, no other header parameter) signed with
   Numbering's DEDICATED grant key (`NUMBERING_GRANT_KEY_ID` /
   `NUMBERING_GRANT_PRIVATE_KEY_FILE`; inline keys refused; the service-auth key — same
   `kid` or same public key — refused at startup). Its closed claim set (contract
   `cfmeet.numbering.routing-grant`): `use` NUMBER_ROUTING_GRANT, the grant kind, one
   audience, the runtime environment, a lifetime of at most 300 s (default 60 s), the
   number, the ACTIVE assignment, the number's ROUTING REVISION, the customer scope, the
   principal, and either the routing target (inbound) or the one operation (outbound).
   It carries no telephone number. Service tokens and contact access tokens are never
   grants and vice versa (`typ`). `cf-service-auth::number_grant` holds the signer, the
   receiver trust bundle (`grant_kinds` per issuer) and the verifier.
2. **Inbound resolution** (`POST /internal/v1/routing/inbound-resolutions`,
   `number-route.resolve`, Telephony for VOICE and the SMS Gateway for SMS only —
   `CONSUMER_MISMATCH` otherwise): the received number resolves to its ACTIVE
   assignment's owner scope, ownership class and routing target only when the number is
   ACTIVE, the inbound capability is in the assignment's snapshot and usable, and the
   family's route is READY and acknowledged by that very consumer; the answer carries an
   INBOUND grant (voice: audience Calling, which delivers it; SMS: the SMS Gateway).
   Otherwise a stable outcome: NOT_ROUTED (assigned, but the family routes nowhere — e.g.
   an offboarded line) or UNAVAILABLE (UNKNOWN_NUMBER, NOT_ACTIVE, NOT_ASSIGNED,
   CAPABILITY_NOT_USABLE, ROUTE_NOT_READY, SYNTHETIC_NOT_ROUTABLE). No grant then.
3. **Outbound identity, no spoofing.** `POST …/caller-identity-grants`
   (`number-caller-identity.authorize`, Calling; audience Telephony) and
   `POST …/sms-sender-grants` (`number-sms-sender.authorize`, SMS Gateway) require the
   signed DELEGATED customer context and name a Number Resource id and one operation —
   never a number. The number must be the delegated scope's ACTIVE assignment (other
   scopes: NOT_FOUND), the outbound capability usable, and the initiating principal
   permitted: the owner of a personal / professional number, the routed employee of a
   direct line (`PRINCIPAL_NOT_PERMITTED`). One grant per operation
   (`OPERATION_ALREADY_AUTHORIZED`).
4. **Authoritative verification, immediate revocation.** The audience presents the
   grant to `POST …/grant-verifications` (`number-grant.verify`): signature (the grant
   key only), type, lifetime, audience = presenter, then CURRENT state — recorded and not
   revoked, the routing revision unchanged, the assignment still the number's ACTIVE one,
   the capability usable. A routing change (including offboarding), a transfer or the
   assignment leaving ACTIVE revokes every outstanding grant of the number in the same
   transaction (recorded reason); a capability revocation fails verification at once.
   An outbound grant is consumed exactly once for its operation (atomic; concurrent
   attempts: one succeeds); only then does its audience learn the number to present.
   Refusals are `valid: false` with a stable reason, never details.
5. **Records, not tokens.** `number_routing_grants` records every grant (token id,
   kind, number, assignment, revision, scope, audience, requester, principal, target or
   operation, lifetime, consumption, revocation); immutable but for one consumption and
   one revocation, kept until a day after expiry, then purged by the sweep.
6. **Fail closed, production-safe.** Without a grant key every grant route answers 503.
   Synthetic test inventory is never resolved or authorized in a protected runtime
   (staging / production): `SYNTHETIC_NOT_ROUTABLE`. No customer route returns a grant;
   signing material never leaves Numbering.

## Consequences

Migration `m20261023_000009_routing_grants` (DOWN refuses while unexpired grants exist).
New internal permissions (`number-route.resolve`, `number-caller-identity.authorize`,
`number-sms-sender.authorize`, `number-grant.verify`) are granted per receiver trust
bundle. Telephony / Calling / the SMS Gateway adopting resolution, presentation and
verification is Workstreams L–O; nothing here simulates them.
