# ADR 0036 — Numbering: provider contract, durable commands and simulator

Status: Accepted (Part IV Workstream G). Extends ADR 0033, ADR 0034 and ADR 0035.

## Context

Numbering must reserve, provision, configure, suspend, restore and release numbers at
carriers without letting any carrier's API shape, credentials or failure modes leak into
the canonical domain, and without ever repeating an irreversible, possibly billable
operation because a response was lost. No real carrier is integrated yet; the software
must still be complete and certifiable.

## Decisions

1. **One provider-neutral contract.** `NumberingProvider` covers search, inspect
   (read-back), reserve, provision, voice / SMS route configuration, suspend, restore,
   release, optional porting, callback verification / normalization and health /
   capability reporting. Every provider-affecting call answers definite success, definite
   failure (with whether anything was transmitted), unsupported, or UNKNOWN. Requests are
   canonical (CF Meet ids, canonical types and capabilities, the provider's opaque
   `external_ref`) and carry the durable command id as the provider-side idempotency key.
   A registry holds independent adapters; a synthetic adapter is refused in staging /
   production (at configuration and at registration) and serves synthetic inventory only.
2. **Durable commands.** Every provider-affecting operation is a
   `number_provider_commands` row written in the SAME transaction as the local intent it
   implies (the PENDING reservation and RESERVING, PROVISIONING, RELEASING), its dispatch
   job and outbox event. The command becomes DISPATCHING (committed) before the adapter is
   called; the response is appended as attempt evidence and the canonical transition is
   applied through the domain's own transition functions in one transaction — the
   command layer never holds lifecycle authority of its own. Repeating a request with the
   same idempotency key returns the command; at most one lifecycle command is in flight
   per number (unique index). Status edges are one set in Rust and SQL; completed commands
   and intents are immutable, attempts append-only, and no command may carry a secret.
3. **Never resend blindly.** A definite not-transmitted failure is retried with backoff;
   a definite failure reverts the intent; UNKNOWN moves the number to
   UNKNOWN_PROVIDER_STATE (where the lifecycle models the operation), raises a
   de-duplicated incident and schedules read-back reconciliation. A process interruption
   after DISPATCHING leaves the job parked UNKNOWN_OUTCOME (RECONCILE_ON_UNCERTAINTY); a
   sweep moves the command to UNKNOWN and reconciliation takes over.
4. **Reconciliation from authoritative evidence only.** An UNKNOWN command is finalized
   only by a read-back not older than the newest provider version already seen: happened
   (success transition by the RECONCILIATION actor with the read-back as evidence), did not
   happen (intent reverted), or drift (another owner, a vanished or half-applied object):
   the number stays fail-closed and an incident is raised. An unreachable provider or a
   stale read-back changes nothing. (Recurring inventory-wide reconciliation is
   Workstream V.)
5. **Authenticated, de-duplicated callbacks.** `POST /provider-callbacks/v1/{providerId}`
   is authenticated by the adapter (HMAC over the raw body, timestamp window); refusals
   are never stored and raise an incident. The stable provider event id de-duplicates
   redeliveries; events are correlated to the resource (inventory ref or binding) and
   command, appended with a disposition and published. An event older than the newest
   applied version is STALE; a released / quarantined number is never resurrected; an
   event never finalizes an uncertain command by itself (it requests reconciliation);
   capability changes update the E capability evidence at once (downgrades are immediate);
   unsolicited releases / revocations are drift: fail closed, incident.
6. **Deterministic provider.** An independent in-memory simulation of a provider (its own
   inventory, holds, provisioning, routes, versions and signed callbacks) with
   provider-side idempotency and scripted, per-instance faults (timeouts before / after
   execution, lost requests, already-owned, vanished inventory, hold conflicts, partial
   provisioning, stale read-back, outage, rejection). Its numbers are in the fictional
   `+1 555 01xx` range and not PSTN-routable.
7. **Reusable certification.** `providers::certification::certify` runs the same 19
   contract cases against any adapter through a harness; destructive cases need explicit
   opt-in (always for the synthetic adapter; a disposable account and
   `CFMEET_PROVIDER_LIVE_CERTIFICATION=1` for a real one) and fault cases need injection —
   skipped cases are reported as skipped, never as passed. The shared durable layers are
   certified once by the Numbering integration suite.

## Consequences

* Real carrier adapters are additions behind the same contract, registry restrictions
  and certification suite; their identifiers stay in internal tables only.
* Public search → reservation → activation APIs and real voice / SMS operation remain
  Workstreams H–O.
