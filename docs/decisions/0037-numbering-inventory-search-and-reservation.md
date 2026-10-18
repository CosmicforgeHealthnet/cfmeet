# ADR 0037 — Numbering: inventory search and reservation

Status: Accepted (Part IV Workstream H). Extends ADR 0033–0036.

## Context

Customers need to find a public number and claim it. A search must never present an
observation as a guarantee, must never leak another tenant's claims, must not let anyone
enumerate the inventory, and must never offer synthetic stock to production. A claim must
be durable, idempotent, concurrency-safe and eligibility-bound, and ending it must never
assume that a provider gave a hold back.

## Decisions

1. **Canonical ingestion.** Provider offers become Number Resources through G's
   provider-neutral `search`: one resource per provider object (the inventory reference
   maps every later observation to the same id); concurrent ingestion of a new number
   creates exactly one resource (E.164 uniqueness decides, the loser observes the
   winner's resource and records no duplicate capability evidence); an older
   observation is appended but never makes a resource look fresher; a resource that left
   inventory is observed, never changed. A malformed offer is skipped and counted.
2. **Search answers from Numbering's own state.** The caller's environment decides the
   class (SANDBOX / TEST synthetic only, PRODUCTION real only); a real provider is asked
   only for an environment allowed live telecom. Only allocatable numbers offered by their
   newest observation, with no current reservation and no open assignment, appear —
   reserved, assigned, quarantined or uncertain numbers never do. Capability filters use
   the E resolver (catalog AND evidence; real evidence must be VERIFIED); each result
   carries capability provenance (synthetic / carrier-confirmed), restriction flags, the F
   KYC indicator (from the requirement sets in force; UNKNOWN when none), cost evidence
   and its observation freshness (`AVAILABLE` / `STALE`, `guaranteed: false`).
3. **Bounded refresh and enumeration.** The first page of a query may refresh providers,
   claimed atomically per (provider, normalized query) in `number_inventory_refreshes`,
   at most once per interval (backing off after a failure); a failed refresh is reported
   (`PROVIDER_UNAVAILABLE`) and cached observations are never presented as live stock.
   A country is required, pages are ≤ 50 with keyset cursors bound to the query digest,
   and every application environment has a per-minute quota in `number_search_windows`
   shared by all instances (429 with `Retry-After`).
4. **Reservations are eligibility-bound provider holds.** The basis is recorded and
   immutable: an ELIGIBLE decision of the reserving scope for this number's country /
   type / class and capabilities, or — where policy permits (SANDBOX / TEST only;
   PRODUCTION refused in configuration and by a CHECK) — a pre-eligibility hold that never
   bypasses eligibility before provisioning. One transaction writes the scoped
   idempotency record, the PENDING reservation (RESERVING) and the durable `RESERVE`
   command; the row lock and the one-current-reservation index let exactly one of
   concurrent claims win, and a losing claim is `NOT_ALLOCATABLE` (no oracle). A replay
   returns the original reservation; a different request with the key is refused.
5. **Ending a reservation never assumes a release.** A HELD reservation that is cancelled
   or expires is ended with its release PENDING and the number stays RESERVED; a new
   `RELEASE_HOLD` command (lifecycle-affecting, one in flight per number) gives the hold
   back and only the provider's evidence returns the number to inventory. A refused or
   unanswered release is read back (UNKNOWN until then, never reallocated); a hold that
   the provider confirms after the reservation expired is recorded and released. A
   reservation whose hold is still being requested or is uncertain is never ended under
   the provider (`RESERVATION_BUSY` / `RECONCILIATION_REQUIRED`). Expiry runs as leased
   `reservation.expire` jobs scheduled by a sweep, which also resubmits owed releases
   (bounded, then an operator incident).
6. **Served API description.** `GET /openapi.json` serves the checked-in contract
   `cfmeet.numbering.internal-api` byte for byte (compiled in); a test proves served =
   contract = registered routes.

## Consequences

Search and reservation work against the deterministic provider with no live carrier.
Live-provider certification of holds and releases remains an external dependency.
