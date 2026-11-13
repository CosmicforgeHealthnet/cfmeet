# ADR 0059 — Organizations first-time activation for an empty dataset

**Status:** Accepted (owner decision, 2026-11-13). The procedure is implemented. It still has to run in production; the result is recorded in the activation report.
**Date:** 2026-11-13
**Amends:** [organizations-cutover-runbook](../operations/organizations-cutover-runbook.md) §4–§6, for one case only. **Builds on:** [ADR 0056](0056-first-party-organizations-and-workspace-boundaries.md) §2 (one writer), §9 (backups first).

## Context

The cutover runbook moves organization authority from the Meeting Service tables to the Organizations Service. The steps are export, import with reconciliation, a shadow period of at least 7 days with zero mismatches, a quiesce window with an ingress block of the three legacy write routes, a final import, and then `authoritative`. The shadow period exists to prove that the imported copy matches what users see before the writer moves.

Live production has never held a first-party organization:

* the export of 2026-11-09 (run 42711430322) found 0 organizations and 0 memberships;
* the legacy write routes are platform-administrator only;
* self-service creation is `FEATURE_DISABLED` until the cutover.

With an empty dataset the shadow comparison has nothing to compare. Every comparison would return two empty sets, so 7 days of zero mismatches prove nothing about data. The quiesce window likewise has no rows to protect. The owner authorized replacing these steps when a verified empty-dataset procedure can safely take their place.

## Decision

A dedicated operation, `org-cutover-first-activation` (`infra/calling/production/org_first_activation.py`), with the confirmation `offsite-backup-verified,empty-dataset`:

1. **First time only.** It refuses if any `org-cutover-import`, `-shadow`, `-authoritative` or `-first-activation` step is recorded as done or running, or if the Meeting Service is not in `legacy` mode. A failed attempt restores everything and may be retried.
2. **Empty everywhere.** Before anything changes, a counts-only, read-only inventory must show all of the following. Otherwise it refuses and points to the standard runbook:
   * Meeting Service: zero legacy organizations, zero memberships, and zero non-null `organization_id` in every table;
   * Organizations Service: zero rows in every table except the migration history and service nonces;
   * every other CF Meet database (Platform Access tenant mappings, Messaging, Calling, Communications and the rest): zero non-null `organization_id` / `organization_ref`.
3. **Gate 1 is unchanged.** A backup no older than 24 hours must exist, with an encrypted off-host copy whose restore drill passed on a separate runner (`cfmeet-offsite-backup.yml`).
4. **Export and import still run** on the empty export, with reconciliation and the idempotence proof. They are journalled as usual.
5. **Inventory again**, immediately before the switch.
6. **Switch.** The Meeting Service receives the shadow step's trust grants and moves directly to `ORGANIZATIONS_AUTHORITY_MODE=authoritative`. The step is journalled as irreversible, so `rollback` refuses it.
7. **Quiescence proven after the switch** instead of by an ingress block. The startup log must report the authoritative mode. The legacy membership table must still be empty, and every local organization row must be an authoritative projection. If a legacy write happened in the window while Organizations is still empty, `legacy` is restored automatically: nothing is lost (runbook §7). If both sides were written, the Meeting Service stays authoritative (fail closed) and the run fails for manual reconciliation.

## What is not changed

* The standard procedure for any non-empty or inconsistent dataset. The migration guards, the import's conflict refusal, the gate-1 check inside `org-cutover-import`, and `org-cutover-authoritative`'s `quiesced` confirmation all stay as they were.
* Authentication, authorization, tenant isolation, service identity and the fail-closed behaviour of the authoritative mode.
* `activate-enterprise-calling` still requires authoritative Organizations. It now accepts either cutover step as the prerequisite.

## Consequences

* The 7-day elapsed-time requirement does not apply to this case. The evidence that replaces it is the two empty inventories, the empty export and import reconciliation, the off-host backup and drill references, and the post-switch quiescence check.
* After the switch, the only way back is a reverse migration (runbook §7). The same holds after the standard cutover.
