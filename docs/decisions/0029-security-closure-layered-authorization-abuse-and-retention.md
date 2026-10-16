# ADR 0029 — Part H security closure: layered authorization, abuse containment, callback security, regulatory boundary and retention ownership

Status: Accepted (certification: `docs/architecture/part-h-security-certification.md`)

## Context

Parts B–G built the platform. Part H (§§71–80) is a cross-cutting audit-and-close pass.
It adds no new service: every control belongs to the existing owner (Calling, Privacy
Relay, Telephony, shared packages, infrastructure). Existing decisions are not repeated:
trust model ADR 0003, service auth ADR 0004, Contact Vault ADR 0011, routing ADR 0013,
Telephony core ADR 0014, edge ADR 0015, recording ADR 0024, API / webhooks / limits ADR 0028.

## Decisions

1. **Layered authorization, each layer independent.** Credential → tenant / application /
   environment status → principal (scoped resolution *and* category) → Privacy Relay
   decision → routing / destination / compliance policy → spend / velocity → feature
   permission → enterprise admin scope. Passing one layer implies nothing about another.
2. **Suspension is not deletion and not lock-out from safety.** A SUSPENDED tenant,
   application or environment authenticates into a *safety-only* authority: an explicit
   (method, route-template) allow-list — end a Call, stop a recording, disable forwarding,
   delete a recording or voicemail, disable a webhook — and nothing else. A revoked or
   expired credential or an inactive service account gets nothing. Inbound PSTN and
   enterprise SIP offers check the owning scope's status before a Call exists; queued
   paid originations re-check it before dispatch. Suspending a principal stops new
   activity; **already-connected Calls are not terminated automatically** (operators may
   end them) — a policy choice so a safety-sensitive call is never cut by an
   administrative action.
3. **Principal categories carry capability.** Service-account and telephone-endpoint
   principals cannot initiate application calls; service-account principals cannot be
   targets. Knowing a UUID never authorizes.
4. **Block / opt-out lives in Calling** (`calling_block_rules`): principal → principal,
   principal → ContactReference, application → ContactReference. ContactReferences are
   opaque Privacy Relay ids — never numbers. Blocks are checked before Privacy Relay
   authorization, routing and spend, refuse without revealing which side blocked
   (`TARGET_UNAVAILABLE` / `DESTINATION_BLOCKED`), skip blocked enterprise targets, and never
   terminate existing Calls. Repeated blocked attempts and repeated route denials become
   deduplicated Calling incidents; nothing auto-suspends on a heuristic.
5. **Abuse reports are a foundation, not a moderation platform**
   (`calling_abuse_reports`): reporter must be a participant of the Call, bounded category
   and code (no free text, no number, no provider payload), idempotent per reporter and
   Call, burst → incident.
6. **Compliance eligibility is a typed, server-authoritative hook**
   (`platform_compliance_eligibility`): status, covered countries, opaque evidence
   reference, expiry. Routing requires an ELIGIBLE, unexpired record for any destination
   country listed by the environment's routing policy **or** the platform overlay
   `CALLING_COMPLIANCE_REQUIRED_COUNTRIES`, which a customer policy cannot remove. No legal
   conclusion is encoded; no KYC vendor is integrated.
7. **Callback security** (Telephony): authentication before mutation (unchanged);
   provider identifiers validated against their storage rule before any transaction
   (400, never 500); invalid callbacks — unknown provider, malformed, oversized, replay
   bursts — counted in 60 s windows and raised as one deduplicated incident per
   category / provider (`CALLBACK_UNKNOWN_PROVIDER`, `CALLBACK_MALFORMED`,
   `CALLBACK_REPLAY_ABUSE`, alongside the existing `CALLBACK_AUTH_FAILURES`), threshold
   `TELEPHONY_CALLBACK_INVALID_INCIDENT_THRESHOLD` (default 20 / min). Attacker-supplied
   provider ids never become a window subject (`unknown`). Request logs carry the route
   template, never a path token. Provider callback secrets overlap during rotation
   (`<credential_ref>.previous`). A valid callback is evidence, never authorization.
8. **Pre-toll destination classification** (Telephony, `telecom_destination_classes`):
   longest-prefix class per country; blocked classes
   (`TELEPHONY_BLOCKED_DESTINATION_CLASSES`, default premium / shared-cost / satellite /
   high-risk) and, optionally, unclassified destinations
   (`TELEPHONY_UNCLASSIFIED_DESTINATION_POLICY`, default `ALLOW` because no range data
   ships) are refused before any provider action. Classification is never guessed.
9. **Regulatory boundary unchanged and evidenced.** CF Meet is not a carrier; public caller
   identity is only the relay number Privacy Relay lent to that session and bound to that
   interconnect; no API accepts a caller id. Telephony now persists the relay assignment
   reference per leg.
10. **Retention ownership is per service with one vocabulary** (data class, action
    Delete / Minimize / StatusOnly, configurable bound, hold, audit). Workers delete only
    under a transaction-local flag (`SET LOCAL <service>.retention_worker = 'on'`) that the
    no-delete triggers honour; application code never sets it. Durations are
    configuration, not legal claims; holds use neutral reason codes. Implemented in
    Privacy Relay (contact erasure after disable, verification events, grants, access
    audit, relay evidence), Telephony (provider events, resolved incidents, holds,
    audit) and Calling (ADR 0030: call display minimization, webhook history, incidents,
    provider evidence, operator audit, holds, principal tombstones).
11. **Operator actions are audited.** Every mutating command of `platform_admin` and
    `telephony-admin` requires an explicit operator identity (`--operator` /
    `CF_OPERATOR_ID`) and writes an append-only operator-audit row with a correlation id in
    the same transaction (Calling `platform_operator_audit`, Telephony
    `telephony_operator_audit`). `privacy-relay-admin` operates on keyring files and on
    Contact Vault rows whose effects are recorded in the append-only contact access audit
    (`REENCRYPT`, erasure) and the retention audit; it takes no operator identity. There is
    no network admin API and no bypass.
12. **Secrets**: no production fallback (unchanged); repository / image secret scan
    (`make secret-scan`, CI job) added; rotation runbook and deployment checklist document
    every secret class honestly, including the rotations that need a restart.

## Consequences

* New failure codes: `PRINCIPAL_NOT_ELIGIBLE`, `DESTINATION_BLOCKED`, `CALLER_BLOCKED`,
  `SCOPE_SUSPENDED`, `COMPLIANCE_ELIGIBILITY_REQUIRED`, `DESTINATION_CLASS_BLOCKED`,
  `DESTINATION_UNCLASSIFIED`.
* Webhook signing secrets are sealed under a versioned key ring (`v2.<kid>`), rotated with
  a current + previous overlap and an audited bounded reseal; an unknown key id fails
  closed. Sensitive Calling secrets accept `*_FILE` delivery.
* SIP peers are identified by network address only (`endpoint_identifier_order = ip`);
  the transmit fence re-checks route, provider, circuit and destination class immediately
  before any toll-generating command.
* Edge-event attribution of unknown channels was deliberately left at its Part E
  behaviour: a stricter attribution quarantined legitimate carrier RFC 4733 DTMF and was
  reverted; SIP certification guards it.
* Real-carrier certification remains pending (ADR 0016); nothing here claims it.
