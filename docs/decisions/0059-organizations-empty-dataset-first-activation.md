# ADR 0059 — Organizations first-time activation for an empty dataset

**Status:** Accepted (owner decision, 2026-11-13); **amended** the same day (Amendment 1: no backup prerequisite). The result of the production run is recorded in the activation report.
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

A dedicated operation, `org-cutover-first-activation` (`infra/calling/production/org_first_activation.py`), with the confirmation `empty-dataset` (Amendment 1; originally `offsite-backup-verified,empty-dataset`):

1. **First time only.** It refuses if any `org-cutover-import`, `-shadow`, `-authoritative` or `-first-activation` step is recorded as done or running, or if the Meeting Service is not in `legacy` mode. A failed attempt restores everything and may be retried.
2. **Empty everywhere.** Before anything changes, a counts-only, read-only inventory must show all of the following. Otherwise it refuses and points to the standard runbook:
   * Meeting Service: zero legacy organizations, zero memberships, and zero non-null `organization_id` in every table;
   * Organizations Service: zero rows in every table except the migration history and service nonces;
   * every other CF Meet database (Platform Access tenant mappings, Messaging, Calling, Communications and the rest): zero non-null `organization_id` / `organization_ref`.
3. ~~**Gate 1 is unchanged.**~~ **Removed by Amendment 1.** No backup is required on this path (see below).
4. **Export and import still run** on the empty export, with reconciliation and the idempotence proof. They are journalled as usual.
5. **Inventory again**, immediately before the switch.
6. **Switch.** The Meeting Service receives the shadow step's trust grants and moves directly to `ORGANIZATIONS_AUTHORITY_MODE=authoritative`. The step is journalled as irreversible, so `rollback` refuses it.
7. **Quiescence proven after the switch** instead of by an ingress block. The startup log must report the authoritative mode. The legacy membership table must still be empty, and every local organization row must be an authoritative projection. If a legacy write happened in the window while Organizations is still empty, `legacy` is restored automatically: nothing is lost (runbook §7). If both sides were written, the Meeting Service stays authoritative (fail closed) and the run fails for manual reconciliation.

## What is not changed

* The standard procedure for any non-empty or inconsistent dataset. The migration guards, the import's conflict refusal, the gate-1 check inside `org-cutover-import`, and `org-cutover-authoritative`'s `quiesced` confirmation all stay as they were.
* Authentication, authorization, tenant isolation, service identity and the fail-closed behaviour of the authoritative mode.
* `activate-enterprise-calling` still requires authoritative Organizations. It now accepts either cutover step as the prerequisite.

## Consequences

* The 7-day elapsed-time requirement does not apply to this case. The evidence that replaces it is the two empty inventories, the empty export and import reconciliation, and the post-switch quiescence check. (Amendment 1: no backup or drill reference is part of the evidence.)
* After the switch, the only way back is a reverse migration (runbook §7). The same holds after the standard cutover.

## Amendment 1 — no backup prerequisite (owner decision, 2026-11-13)

The owner removed the encrypted off-site backup requirement as a blocking prerequisite of this first-time activation and accepted the increased recovery risk. On the `org-cutover-first-activation` path:

* `require_offsite(...)` is not called, and no fresh local backup, restore test or drill is checked (`fresh_backup` is not called either). The owner explicitly declined a replacement local-backup gate.
* The empty-dataset import runs with `org_import(..., backup_gate=False)`, so the standard import's gate 1 is not invoked.
* The only confirmation is `empty-dataset`. `offsite-backup-verified` is no longer required (it is ignored if still given).
* `CFMEET_BACKUP_AGE_RECIPIENTS` / `CFMEET_BACKUP_AGE_IDENTITY` are not needed, and `cfmeet-offsite-backup.yml` is not run as part of activation.
* The run emits `backupGate: {required: false, offsiteBackupVerified: false, reason: …}` and `firstActivation.backupVerified: false`. It never records or reports a backup as verified.
* In the coordinated release gate, the backup drill (`backup-restore-drill.yml`) and `cfmeet-offsite-backup.yml` are advisory: they still run and are reported, but they do not block a release.

Unchanged: the empty-everywhere inventories (before and right before the switch), the refusal of any non-empty dataset (no data is deleted or overwritten to make it empty), the empty export, import reconciliation and idempotence proof, trust grants, single-writer authority, the irreversible journal flag and the post-switch quiescence check. The standard (non-empty) procedure keeps gate 1, including `org-cutover-import`'s off-host check and `org-cutover-authoritative`'s `offsite-backup-verified,quiesced`. The off-site tooling remains available for optional use.

**Accepted risk:** if the production database is lost before an off-host copy exists, organization data created after activation cannot be recovered from an off-host backup.
