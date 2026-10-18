# ADR 0044 — Relay numbers under Numbering's authority (Part III → Part IV migration)

Status: Accepted (Part IV Part P §§119–123). Builds on ADR 0012 (temporary relay-number
assignment and cooldown) and ADR 0043.

## Context

Part III gave Privacy Relay its own relay-number inventory. Part IV makes Numbering the
authority over every public number. Privacy Relay must keep owning relay SESSIONS, their
ids, its relay-number assignments, their generations and the inbound routing of a relay
call to its session; existing sessions must keep routing exactly as before; and the change
must be reversible without data loss or a production migration nobody approved.

## Decisions

1. **Relay numbers are Number Resources of service class `PRIVACY_RELAY`.** The
   resource / binding / capability schema is reused; `number_relay_pool_members` keys each
   one by the Privacy Relay inventory id. Database triggers refuse to reserve or assign a
   relay-class number as a DCN, and search never offers one.
2. **Temporary leases** (`number_relay_leases`, permission `number-relay.lease`, Privacy
   Relay only): one ACTIVE lease per number, idempotent per holder key, bounded lifetime,
   refused when Numbering took the number out of service, the member is retired or the
   `RELAY_LEASES` switch is engaged; a compatibility resolution returns the current lease
   (holder = Privacy Relay assignment id, generation).
3. **A switch, not a dual write.** `RELAY_NUMBER_AUTHORITY=LOCAL` (default and rollback)
   keeps Part III behaviour; `NUMBERING` (with `NUMBERING_INTERNAL_BASE_URL`) asks
   Numbering for a lease on the assignment's own id and next generation before lending a
   number (a refused candidate is skipped; Numbering unavailable refuses the assignment).
4. **Verified cutover.** In NUMBERING mode Privacy Relay lends nothing until its
   idempotent backfill imported every inventory row and adopted every ACTIVE mapping and
   the compatibility check proved Numbering's view identical (same assignment, same
   generation). Inbound resolution keeps using Privacy Relay's own mapping, so routing is
   identical before and after cutover (certified across real processes).
5. **Durable release in every mode.** An assignment's end marks its lease
   RELEASE_PENDING in the same transaction; the worker sweep releases it at Numbering and
   keeps retrying while Numbering is unavailable — also after a rollback to LOCAL.
6. **Retirement of the duplicate authority is gated, not executed.** Privacy Relay's
   inventory stays as the mapping store and the rollback position; removing it is a later,
   explicitly approved step after production compatibility evidence. No historical data
   is removed.

## Consequences

Calling, Telephony and the Part III contracts are unchanged (no new cross-service
database access; Privacy Relay calls Numbering over the authenticated internal API only).
