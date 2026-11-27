"""Organizations first-time activation for an EMPTY dataset (owner decision, 2026-11-13).

`org-cutover-first-activation` replaces the import -> shadow -> 7-day observation -> quiesce
sequence of organizations-cutover-runbook.md for exactly one case: production has never held
a first-party organization anywhere. With no legacy organization, no membership and no
organization-scoped state in any service there is nothing to reconcile and nothing a shadow
period could compare, so the observation window protects no data.

Owner decision 2026-11-13 (ADR 0059 Amendment 1): this first-time path does NOT require a
backup. There is no off-host copy check (`require_offsite`), no fresh local backup or restore
drill check, no `offsite-backup-verified` confirmation and no age key. The owner accepted the
increased recovery risk; the run records that decision (`backupGate`) and never reports a
backup as verified. The off-site tooling (`cfmeet-offsite-backup.yml`, offsite_backup.py) stays
available for optional use, and the standard non-empty cutover keeps its gate 1 unchanged.

Everything else stays:

* the owner's explicit record of this procedure: `empty-dataset`;
* the export, the import with reconciliation and the idempotence proof (they run on the empty
  export and are journalled as `org-cutover-export` / `org-cutover-import`, as always);
* the Meeting Service trust grants of the shadow step, its signing identity and private TLS route;
* the irreversible journal flag (runbook section 7: never fall back to an older writer).

It refuses, before changing anything, unless every one of these holds (counts only; no name,
email or slug leaves a database):

* no earlier `org-cutover-import`, `-shadow`, `-authoritative` or `-first-activation` is
  recorded as done or running, and the Meeting Service runs in `legacy` mode;
* Meeting Service: zero legacy organizations and memberships, and zero non-null
  `organization_id` values in every table (spaces, event inbox, member access, scope
  environments ...);
* Organizations: zero rows in every table except its migration history and service nonces
  (organizations, memberships, invitations, teams, policies, provisioning, platform scopes,
  bindings, audit, outbox, import runs, idempotency records);
* every other CF Meet database (Platform Access tenant mappings, Messaging, Calling,
  Communications ...): zero non-null `organization_id` / `organization_ref` values.

Quiescence is proven mechanically instead of by an ingress block: the same Meeting inventory
runs again after the switch, and the legacy membership table must still be empty and every
local organization row must be an authoritative projection. A legacy write in the window
restores `legacy` automatically while the Organizations Service is still empty (no user
write there, runbook section 7, last bullet); otherwise the Meeting Service stays
authoritative (fail closed) and the run fails for manual reconciliation.

A non-empty or inconsistent dataset must use the standard procedure (export, import, shadow,
7-day evidence, quiesce, `org-cutover-authoritative`), which is unchanged.
"""
import json
import subprocess

OPERATION = 'org-cutover-first-activation'
CONFIRMATIONS = ('empty-dataset',)
EXCLUSIVE = ('org-cutover-import', 'org-cutover-shadow', 'org-cutover-authoritative', OPERATION)
# A recorded export / import of an EMPTY dataset (e.g. by an earlier first-activation attempt that
# stopped before the switch) does not make this a non-first activation: only a running import, or
# any shadow / authoritative / first-activation step, does. Emptiness itself is proven by `inventory`.
DONE_ALLOWED = ('org-cutover-import',)
# Organizations tables that legitimately hold rows without any organization.
ORGANIZATIONS_NON_DATA = ('organization_service_nonces',)
REFERENCE_COLUMNS = ('organization_id', 'organization_ref')
MODE_LOG = 'Organizations authority mode: authoritative'
BACKUP_DECISION = {'required': False, 'offsiteBackupVerified': False,
                   'reason': 'owner decision 2026-11-13: first-time empty-dataset activation proceeds without a '
                             'verified backup; increased recovery risk accepted (ADR 0059 Amendment 1)'}

_COUNT = ("(xpath('/row/n/text()', query_to_xml(format('SELECT count(*) AS n FROM %%I.%%I%s', "
          "c.table_schema, c.table_name%s), false, true, '')))[1]::text::bigint")


def reference_sql(columns=REFERENCE_COLUMNS):
    """{table.column: rows with a non-null organization reference} for every base table."""
    count = _COUNT % (' WHERE %I IS NOT NULL', ', c.column_name')
    names = ', '.join("'%s'" % c for c in columns)
    return ("BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY;\n"
            "SELECT coalesce(json_object_agg(k, n), '{}') FROM (SELECT c.table_name || '.' || c.column_name k, "
            + count + " n FROM information_schema.columns c JOIN information_schema.tables t "
            "ON t.table_schema = c.table_schema AND t.table_name = c.table_name AND t.table_type = 'BASE TABLE' "
            "WHERE c.table_schema = 'public' AND c.column_name IN (" + names + ")) s;\nCOMMIT;\n")


def table_rows_sql(excluded=ORGANIZATIONS_NON_DATA):
    """{table: rows} for every public base table except migration histories and `excluded`."""
    count = _COUNT % ('', '')
    names = ', '.join("'%s'" % t for t in excluded) or "''"
    return ("BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY;\n"
            "SELECT coalesce(json_object_agg(c.table_name, " + count + "), '{}') FROM information_schema.tables c "
            "WHERE c.table_schema = 'public' AND c.table_type = 'BASE TABLE' AND c.table_name NOT LIKE '%migrations' "
            "AND c.table_name NOT IN (" + names + ");\nCOMMIT;\n")


# The receipts an import of an EMPTY export leaves behind (the import run row and its
# `migration.imported` audit event, both without any organization, membership or conflict). They are
# the procedure's own bookkeeping, not organization data; anything else in these tables still counts.
EMPTY_IMPORT_SQL = ("BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY;\n"
                    "SELECT json_build_object('organization_import_runs', (SELECT count(*) FROM organization_import_runs "
                    "WHERE organizations_count = 0 AND memberships_count = 0 AND imported = 0 AND unchanged = 0 "
                    "AND conflicts = '[]'::jsonb), 'organization_audit_events', (SELECT count(*) FROM "
                    "organization_audit_events WHERE action = 'migration.imported' AND target_type = 'import_run' "
                    "AND organization_id IS NULL AND (summary->>'imported') = '0' AND (summary->>'unchanged') = '0' "
                    "AND (summary->>'conflicts') = '0'));\nCOMMIT;\n")

PROJECTION_SQL = ("BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY;\n"
                  "SELECT json_build_object('memberships', (SELECT count(*) FROM organization_memberships), "
                  "'organizationIds', (SELECT coalesce(json_agg(id::text ORDER BY id), '[]') FROM organizations));\n"
                  "COMMIT;\n")
ORGANIZATION_IDS_SQL = ("BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY;\n"
                        "SELECT coalesce(json_agg(id::text ORDER BY id), '[]') FROM organizations;\nCOMMIT;\n")


def nonzero(counts):
    return {k: v for k, v in sorted(counts.items()) if v}


def confirmations(confirm):
    given = {c for c in (confirm or '').split(',') if c}
    missing = [c for c in CONFIRMATIONS if c not in given]
    if missing:
        raise RuntimeError('Owner confirmation missing for the empty-dataset first activation: ' + ','.join(missing)
                           + ' (confirm=empty-dataset)')


def first_time(cr):
    """No standard or first-time cutover has run or is running; Meeting is in legacy mode."""
    seen = sorted({e['operation'] for e in cr.load_journal()
                   if e['operation'] in EXCLUSIVE and (e['status'] == 'running' or
                                                       (e['status'] == 'done' and e['operation'] not in DONE_ALLOWED))})
    if seen:
        raise RuntimeError('Not a first-time activation: %s already recorded; use the standard cutover runbook'
                           % ', '.join(seen))
    mode = cr.env_of('meeting-service').get('ORGANIZATIONS_AUTHORITY_MODE') or 'legacy'
    if mode != 'legacy':
        raise RuntimeError('The Meeting Service runs in %s mode; the first-time procedure starts from legacy only'
                           % mode)


def inventory(cr):
    """Counts only. Raises unless every organization dataset in every service is empty."""
    legacy = json.loads(cr.meeting_psql(cr.INVENTORY_SQL))
    meeting_refs = json.loads(cr.meeting_psql(reference_sql()))
    organizations = json.loads(cr.release.db_sql(cr.SERVICES['organizations']['db'], table_rows_sql()))
    receipts = json.loads(cr.release.db_sql(cr.SERVICES['organizations']['db'], EMPTY_IMPORT_SQL)) \
        if organizations else {}
    data = {t: n - receipts.get(t, 0) for t, n in organizations.items()}
    others = {}
    for db in cr.core_databases():
        if db == cr.SERVICES['organizations']['db']:
            continue
        found = nonzero(json.loads(cr.release.db_sql(db, reference_sql())))
        if found:
            others[db] = found
    doc = dict(
        legacy={k: legacy[k] for k in ('organizations', 'memberships', 'orphanSpaceOrganizations',
                                       'ownerlessOrganizations', 'duplicateSlugs', 'duplicateLiveMemberships')},
        meetingReferences=nonzero(meeting_refs), meetingReferenceColumns=len(meeting_refs),
        organizationsService=nonzero(data), organizationsTablesChecked=len(organizations),
        emptyImportReceipts=nonzero(receipts),
        otherServices=others)
    cr.emit('firstActivationInventory', doc)
    problems = [k for k, v in doc['legacy'].items() if v]
    if doc['meetingReferences']:
        problems.append('meeting-service organization references')
    if not organizations:
        problems.append('Organizations tables not found')
    if doc['organizationsService']:
        problems.append('Organizations Service data')
    if others:
        problems.append('organization references in ' + ', '.join(sorted(others)))
    if problems:
        raise RuntimeError('The dataset is not empty (%s): the empty-dataset first activation is refused; '
                           'use the standard cutover runbook' % '; '.join(problems))
    return doc


def quiescence(cr):
    """After the switch: no legacy write happened in the window. Returns the problems found."""
    local = json.loads(cr.meeting_psql(PROJECTION_SQL))
    authoritative = set(json.loads(cr.release.db_sql(cr.SERVICES['organizations']['db'], ORGANIZATION_IDS_SQL)))
    strays = [i for i in local['organizationIds'] if i not in authoritative]
    problems = []
    if local['memberships']:
        problems.append('%d legacy memberships' % local['memberships'])
    if strays:
        problems.append('%d legacy organizations without an authoritative record' % len(strays))
    cr.emit('firstActivationQuiescence', {'legacyMemberships': local['memberships'],
                                          'localOrganizations': len(local['organizationIds']),
                                          'authoritativeOrganizations': len(authoritative),
                                          'strayLegacyOrganizations': len(strays)})
    return problems, len(authoritative)


def mode_in_effect(cr):
    """The running Meeting Service logged the authoritative mode at startup (the log is only
    searched, never printed: it can contain request data)."""
    container = cr.SERVICES['meeting-service']['container']
    proc = subprocess.run(['docker', 'logs', '--tail', '5000', container], capture_output=True, text=True)
    if proc.returncode or MODE_LOG not in proc.stdout + proc.stderr:
        raise RuntimeError('The Meeting Service did not report the authoritative Organizations mode at startup')
    if cr.env_of('meeting-service').get('ORGANIZATIONS_AUTHORITY_MODE') != 'authoritative':
        raise RuntimeError('The Meeting Service configuration is not authoritative')


def activate(cr, sha, run_id, confirm):
    confirmations(confirm)
    first_time(cr)
    cr.has_signing_identity('meeting-service')
    cr.tls_route_present(cr.SERVICES['organizations']['tls'])
    inventory(cr)
    # Owner decision (ADR 0059 Amendment 1): no backup prerequisite on this path. Recorded, never
    # reported as a verified backup.
    cr.emit('backupGate', BACKUP_DECISION)
    summary = cr.org_export(sha, run_id, confirm)
    if (summary['counts']['organizations'], summary['counts']['memberships']) != (0, 0):
        raise RuntimeError('The export is not empty; use the standard cutover runbook')
    cr.org_import(sha, run_id, confirm, backup_gate=False)
    inventory(cr)  # immediately before the switch: still empty everywhere
    late = {}

    def post():
        mode_in_effect(cr)
        problems, authoritative = quiescence(cr)
        if not problems:
            return
        if authoritative == 0:
            # Organizations has accepted no write: restoring legacy loses nothing (runbook 7).
            raise RuntimeError('Legacy organization write during the switch (%s); legacy restored'
                               % '; '.join(problems))
        late['problems'] = problems  # both sides written: stay authoritative (fail closed)

    cr.apply_activation(OPERATION, sha, run_id, [('meeting-service', {
        'ORGANIZATIONS_AUTHORITY_MODE': 'authoritative',
        'CF_ORGANIZATIONS_BASE_URL': cr.private_url(cr.SERVICES['organizations']['tls'])})],
        irreversible=True, post=post)
    if late:
        raise RuntimeError('Authoritative, but a legacy write happened in the window (%s): manual reconciliation '
                           'required (runbook 7); the Meeting Service stays authoritative'
                           % '; '.join(late['problems']))
    cr.emit('firstActivation', {'mode': 'authoritative', 'procedure': 'empty-dataset',
                                'shadowObservation': 'not applicable: no legacy organization data to compare',
                                'backupVerified': False})
