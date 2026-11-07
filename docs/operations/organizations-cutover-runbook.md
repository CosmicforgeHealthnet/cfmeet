# Organizations cutover runbook (Blueprint Section 4)

**Status:** prepared on `staging`; nothing here has been run in production. **Decision record:** [ADR 0056](../decisions/0056-first-party-organizations-and-workspace-boundaries.md) (§2 one writer, §3 ingress, §6 fail closed, §9 backups first). **Internal contract:** [organizations-internal-v1](../api/organizations-internal-v1.md).

This runbook moves first-party organization authority from the meeting-service tables (`organizations`, `organization_memberships`) to the Organizations Service, with **exactly one writer at every moment**. Meeting-service selects its role with one setting:

| `ORGANIZATIONS_AUTHORITY_MODE` | Writer | Reads served from | First-party routes (`/api/v2/organizations*`, invitations, `GET /api/v2/admin/organizations`) |
|---|---|---|---|
| `legacy` (default) | meeting-service | meeting-service tables | `FEATURE_DISABLED` |
| `shadow` | meeting-service | meeting-service tables. A bounded background comparison with Organizations records mismatches only. | `FEATURE_DISABLED` |
| `authoritative` | Organizations Service | Organizations (`memberships/resolve`; fails closed). The local `organizations` rows become a read-only projection. | relayed to Organizations |

`shadow` and `authoritative` also need `CF_ORGANIZATIONS_BASE_URL` (HTTPS outside `local`/`test`), `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_AUTH_KEY_ID` and `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`. The optional `CF_ORGANIZATIONS_TIMEOUT_MS` defaults to 3000 and is clamped to 500–10000. A missing or invalid value stops startup. The startup log line `Organizations authority mode: …` shows the mode in effect. Secrets are never logged.

The Organizations trust bundle must grant `cfmeet.meeting-service` the permissions `organization.read`, `organization.write`, `organization.act-for-user` and `organization.administer`. `organization.migrate` goes only to the operator import identity used in step 3.

## What needs owner approval

Each of these is a separate, explicit owner decision. Engineering stops at each one:

1. **Backups first (ADR 0056 §9, ADR-A13).** Before step 3, you need a verified, encrypted, off-host backup of the meeting-service database (`cosmicforge-db`) and of the Organizations database, plus a passed isolated restore drill ([backup-and-restore](backup-and-restore.md)). Off-host storage is the owner's choice. Until it exists, this gate stays closed.
2. **Production release of the Organizations Service.** This covers its image, database, migrator/runtime roles, trust bundles and ingress (internal only, never public).
3. **Running the import in production** (step 3) and accepting the reconciliation report.
4. **Entering `shadow`** (step 4). This is a meeting-service configuration change and restart.
5. **Accepting the shadow evidence** (zero-mismatch criterion, step 5).
6. **The cutover window** (step 6, quiesce plus switch to `authoritative`), including the user-visible notice that organization changes pause for the window.
7. **Any re-sync** of a row that differs between the export and an earlier import (contract §5: such rows are reported, never overwritten, until the gate allows it).

## 1. Inventory (read-only, counts only)

Run against the meeting-service database in a read-only transaction. Every query returns **counts only**: no names, emails or slugs leave the database.

```sql
BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY;
-- Totals
SELECT status, count(*) FROM organizations GROUP BY status ORDER BY status;
SELECT status, role, count(*) FROM organization_memberships GROUP BY status, role ORDER BY status, role;
-- Duplicates (slug case-insensitively; more than one live membership per organization and user)
SELECT count(*) AS duplicate_slugs FROM (SELECT lower(slug) FROM organizations GROUP BY 1 HAVING count(*) > 1) d;
SELECT count(*) AS duplicate_live_memberships FROM (
  SELECT organization_id, user_id FROM organization_memberships WHERE status <> 'REMOVED'
  GROUP BY 1, 2 HAVING count(*) > 1) d;
-- Null creators (imported as createdBy null; allowed, but counted)
SELECT count(*) AS organizations_without_creator FROM organizations WHERE created_by IS NULL;
SELECT count(*) AS memberships_without_creator FROM organization_memberships WHERE created_by IS NULL;
-- Orphan meeting_spaces.organization_id (the FK should make this 0)
SELECT count(*) AS orphan_space_organizations FROM meeting_spaces s
  LEFT JOIN organizations o ON o.id = s.organization_id
 WHERE s.organization_id IS NOT NULL AND o.id IS NULL;
SELECT count(*) AS spaces_with_organization FROM meeting_spaces WHERE organization_id IS NOT NULL;
-- Ownerless organizations (no ACTIVE OWNER)
SELECT count(*) AS ownerless_organizations FROM organizations o
 WHERE NOT EXISTS (SELECT 1 FROM organization_memberships m
                    WHERE m.organization_id = o.id AND m.role = 'OWNER' AND m.status = 'ACTIVE');
-- Slugs outside the Organizations rule ^[a-z0-9](?:[a-z0-9-]{1,61}[a-z0-9])$
SELECT count(*) AS slugs_outside_contract FROM organizations
 WHERE slug !~ '^[a-z0-9](?:[a-z0-9-]{1,61}[a-z0-9])$';
-- Live memberships held by deactivated, suspended or guest accounts
SELECT count(*) AS live_memberships_of_inactive_accounts FROM organization_memberships m
  JOIN users u ON u.id = m.user_id
 WHERE m.status <> 'REMOVED' AND (u.status::text IN ('inactive', 'suspended') OR u.role::text = 'guest');
COMMIT;
```

Record the numbers in the release evidence. Before step 3, the owner decides the following. **Ownerless organizations**: import them as they are (Organizations reports them) or give each one an OWNER *before* the export, through the legacy route, while meeting-service is still the writer. **Slugs outside the contract**: the import refuses them, so they must be renamed in meeting-service before the export. **Live memberships of inactive accounts**: imported as is. They grant nothing, because the account authority refuses those accounts.

## 2. Export (meeting-service, read-only)

`export-organizations` ships in the meeting-service image (`src/bin/export_organizations.rs`). It reads one `REPEATABLE READ, READ ONLY` snapshot and never writes. It prints the document on stdout and only counts and the checksum on stderr:

```bash
docker exec -e DATABASE_URL="$(cat /path/to/meeting-readonly.url)" <meeting-service-container> \
  ./export-organizations > /secure/organizations-export.json      # 0600, removed after step 3
```

Use a read-only database role. The file holds organization names and user ids. Treat it like a backup: encrypt it at rest and never attach it to tickets. The checksum algorithm and test vector are in contract §5. The stderr summary must equal the step 1 totals for organizations and memberships.

## 3. Import and reconciliation (Organizations side)

1. `POST /internal/v1/migration/import` with the export, sent by the `organization.migrate` operator identity. The import refuses a different `schemaVersion`, a checksum mismatch, a count mismatch, and memberships whose organization is absent. It preserves every id, timestamp and `created_by`, and sets `creationSource=IMPORTED`, `joinedVia=IMPORTED`, `provisioningState=PENDING`. It returns `{imported, unchanged, conflicts[], checksum}`.
2. `GET /internal/v1/migration/reconciliation`: the counts and checksum must equal the export's, with zero mismatches.
3. Re-running the same import must report everything `unchanged`. That proves the import is idempotent.
4. Imports are repeatable while meeting-service is still the writer: re-export and re-import at any time before step 6. A row that changed since the previous import is reported as a conflict. It is re-synced only with owner approval (gate 7).

## 4. Shadow period

Set `ORGANIZATIONS_AUTHORITY_MODE=shadow` plus the client settings above, and restart meeting-service. Meeting-service stays **the only writer**. Every organization read is still answered from its own tables. For each directory read, a bounded background task (at most 8 in flight; extra ones are skipped and counted) asks `memberships/resolve` for the same user. It compares the set of ACTIVE organizations and the role in each. In shadow mode the first-party routes stay `FEATURE_DISABLED`, so no user can write to Organizations.

Signals on `GET /internal/metrics` (internal token; label-free counters):

| Counter | Meaning |
|---|---|
| `cfmeet_ops_organizations_shadow_comparisons` | comparisons completed |
| `cfmeet_ops_organizations_shadow_mismatches` | comparisons that differed |
| `cfmeet_ops_organizations_shadow_failures` | Organizations unreachable/refused during a comparison (the request was unaffected) |
| `cfmeet_ops_organizations_shadow_skipped` | comparisons skipped at the concurrency bound |

Every mismatch also produces a structured log line (`target=organizations_shadow`, message `organization shadow mismatch`). It carries the user id and the organization ids that are `missing_in_authority`, `extra_in_authority` or `role_mismatch`. It never carries emails or names.

Writes made in meeting-service during the shadow period (new organizations or membership changes) appear as mismatches until the next re-export and re-import. Repeat step 3 to bring Organizations up to date.

## 5. Zero-mismatch criterion

Cutover may be proposed only when all of these hold:

* the latest import reconciled with zero conflicts, and was taken after the last meeting-service organization write;
* for at least 7 consecutive days (or a period the owner sets), `shadow_mismatches` did not increase except for writes explained by a later re-import, and stayed flat for at least 24 hours after the last re-import;
* `shadow_comparisons` covers active usage (non-zero every day), `shadow_failures` is not more than 0.1 % of comparisons, and `shadow_skipped` is near zero (otherwise coverage is incomplete);
* Organizations `/ready` reports database ok and migrations current;
* the backup and restore evidence (gate 1) is no older than the cutover window.

## 6. Quiesce and switch (the only writer moves once)

1. Announce the window (gate 6). Organization changes pause; meetings, calls and messaging continue.
2. **Quiesce meeting-service writes.** Keep `shadow`, block the three legacy write routes at the ingress (`POST /api/v2/admin/organizations`, `PATCH /api/v2/admin/organizations/{id}`, `PUT /api/v2/organizations/{id}/members/{userId}`), and confirm from the access log that none succeeded after the block. The legacy reads keep working.
3. Final export → import → reconciliation (steps 2–3). There must be zero conflicts, and the checksum must equal the export's.
4. Set `ORGANIZATIONS_AUTHORITY_MODE=authoritative`, restart meeting-service, then lift the ingress block. From now on:
   * every membership read comes from `memberships/resolve`. Any failure grants no organization (ORGANIZATION_ONLY admission is denied; personal features are unaffected) and is counted in `cfmeet_ops_organizations_authority_failures`. MANAGER counts as a member; GUEST does not;
   * the legacy write routes are facades. The admin create and status change relay to `/internal/v1/admin/organizations…` with the platform administrator as the acting user (meeting-service checks `platform_admin` first). A member PUT becomes a `PATCH` of the existing membership. A user who is not a member gets `409 INVITATION_REQUIRED`;
   * `GET /api/v2/me/organizations` and `GET /api/v2/organizations/{id}/members` keep their fields. They add `organizationRole` (and `version` on members);
   * the local `organizations` row is upserted from the authoritative snapshot whenever an organization is referenced: space creation, `me/organizations`, and any relayed organization document. ARCHIVED is projected as SUSPENDED. The row exists for the `meeting_spaces.organization_id` foreign key, the pre-join name and the profile policy. The local `organization_memberships` table is no longer read or written. The messaging colleague directory reads Organizations' member lists.
5. Smoke checks: a member lists their organizations; an ORGANIZATION_ONLY join succeeds for a member and is denied for a non-member; `GET /api/v2/organizations/{id}` succeeds; `cfmeet_ops_organizations_relay_unavailable` and `…_authority_failures` stay flat.

## 7. Rollback: the no-split-brain rule

**Never let both sides accept writes, and never go back to a writer whose data is older than the other side's.**

* **Before step 6.4** (legacy or shadow): set `ORGANIZATIONS_AUTHORITY_MODE=legacy` and restart. Organizations has received no user writes, so nothing is lost. The import can be repeated later.
* **After step 6.4**, once Organizations has accepted any write (an invitation accepted, a role changed, an organization created): **do not** switch meeting-service back to `legacy` or `shadow`. The local tables are stale, and switching back would silently undo those changes and re-grant removed access. Instead:
  1. If Organizations is unhealthy, leave meeting-service `authoritative`. It fails closed, so organization access is denied rather than wrongly granted, while Organizations is restored from its backup or fixed.
  2. A return to meeting-service authority is a new, owner-approved migration in the reverse direction. Quiesce Organizations writes, reconcile its data back into the meeting-service tables with an audited script, verify counts and checksums, then switch. It is never a configuration flip.
* **Within the window, before any Organizations write** (6.4 done, smoke checks failing, the ingress block still in place, and the Organizations audit showing no user-initiated change): switching back to `shadow` is allowed. The data on both sides is still identical.

## Evidence to record

The step 1 counts; the export summary (counts and checksum); the import result and the reconciliation; daily shadow counter snapshots and the mismatch log volume; the quiesce confirmation; the final reconciliation; the mode switch time; the smoke-check results; the backup and restore drill references.
