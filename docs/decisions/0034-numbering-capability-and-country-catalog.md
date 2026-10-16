# ADR 0034 — Numbering: capability and country catalog

Status: Accepted (Part IV Workstream E). Extends ADR 0033.

## Context

Workstream D recorded per-number capability evidence but had no authority describing what
a country and number type can support, no expiry or revocation of evidence and no single
place that answers "may this number do X now?". Later grant and routing consumers need
that answer with a revision they can compare, and nothing may be inferred: SMS does not
make a number mobile, inbound does not imply outbound, a country's support says nothing
about an individual number, and synthetic test data must never look like a carrier fact.

## Decisions

1. **Versioned catalog.** `number_country_catalog` and `number_type_catalog` are
   append-oriented (a change supersedes the CURRENT row at a new revision) and every change
   publishes one gap-free `number_catalog_revisions` row (serialized by an advisory lock,
   enforced by a trigger). Entries carry source, verification status, evidence,
   effective / expiry dates, restrictions, structured limitations, optional region and
   optional provider applicability. Real and synthetic inventory have separate catalogs
   (`inventory_class`); synthetic entries are `SYNTHETIC_TEST` / `SYNTHETIC` by
   construction. A VERIFIED fact needs evidence. Number-class attributes follow the type
   (database CHECK). The catalog ships empty: nothing real is claimed until an operator
   publishes evidence-backed entries.
2. **One resolver.** `resolve_capabilities` reads the resource, its active bindings, the
   catalog of its class (provider-specific before generic, regional before national),
   its current evidence and the latest inventory provider from one repeatable-read
   snapshot and returns, per capability, *supported* / *usable* with a normalized reason,
   effective limitations, provenance (`synthetic`, `carrierConfirmed`, catalog entry and
   revision, binding ref) and the resource / capability / binding / catalog revisions.
   Precedence: country, number class, type entry, evidence; routable capabilities are
   usable only on an ACTIVE number with an active binding serving them. Real numbers need
   VERIFIED catalog facts and VERIFIED evidence. `require_capability` maps a refusal to
   `CAPABILITY_NOT_SUPPORTED` / `CAPABILITY_NOT_USABLE` and never falls back to another
   number.
3. **Safe capability changes.** Records gain expiry, source observation time, confirming
   binding, change kind, actor and reason; statuses add EXPIRED and REVOKED. Every change
   advances `number_resources.capability_revision` and emits `number.capability.changed`
   with it. Stale observations (`STALE_EVIDENCE`) and stale revisions are refused; a
   lower-authority source never overrides a higher one's current truth
   (`SOURCE_AUTHORITY_CONFLICT`) while any source may downgrade; a provider confirmation
   names its ACTIVE binding and stops counting when that binding ends (binding
   replacement); REVOKED is re-enabled only by an operator or reconciliation. Expiry
   changes truth at read time; a sweep records it durably.
4. **No promises.** Limitations are a closed vocabulary; acceptance claims (WhatsApp,
   banks, OTP, "guaranteed") are refused. Every voice / SMS capability carries the fixed
   notice "Supports voice and SMS where provisioned; third-party verification
   compatibility varies by service and number type."
5. **Surfaces.** Internal `GET /internal/v1/number-resources/{id}/capabilities`
   (`number-resource.read`); customer catalog view by environment class and the
   capabilities of the caller's own assignment; administration (`numbers.admin` on a
   FIRST_PARTY application plus the audited operator in `X-CF-Operator`) to publish /
   withdraw catalog entries and revoke capabilities. Grant issuance (K) and recurring
   multi-provider reconciliation (V) consume these revisions later.
