"""Offline tests: the empty-dataset Organizations first activation (org_first_activation.py)."""
import json
import unittest
from types import SimpleNamespace
from unittest.mock import patch

import coordinated_release as cr
import org_first_activation as fa

SHA = 'a' * 40
EMPTY_LEGACY = dict(organizations=0, memberships=0, orphanSpaceOrganizations=0, ownerlessOrganizations=0,
                    duplicateSlugs=0, duplicateLiveMemberships=0, organizationsByStatus={},
                    membershipsByStatusRole={}, slugsOutsideContract=0)
ORG_TABLES = {'organizations': 0, 'organization_memberships': 0, 'organization_invitations': 0,
              'organization_outbox': 0, 'organization_teams': 0}


class FakeCr:
    """The slice of coordinated_release the procedure uses; every effect is recorded."""

    def __init__(self, **over):
        self.calls = []
        self.emitted = []
        self.journal = over.pop('journal', [])
        self.meeting_env = over.pop('meeting_env', {})
        self.legacy = dict(EMPTY_LEGACY, **over.pop('legacy', {}))
        self.meeting_refs = over.pop('meeting_refs', {'meeting_spaces.organization_id': 0})
        self.org_tables = dict(ORG_TABLES, **over.pop('org_tables', {}))
        self.refs = over.pop('refs', {})
        self.export_counts = over.pop('export_counts', {'organizations': 0, 'memberships': 0})
        self.after_switch = over.pop('after_switch', {'memberships': 0, 'organizationIds': []})
        self.authoritative_ids = over.pop('authoritative_ids', [])
        self.receipts = over.pop('receipts', {'organization_import_runs': 0, 'organization_audit_events': 0})
        self.import_writes_receipts = over.pop('import_writes_receipts', True)
        self.INVENTORY_SQL = cr.INVENTORY_SQL
        self.SERVICES = {'organizations': {'db': 'cfmeet_organizations', 'tls': 8450},
                         'meeting-service': {'container': 'cfmeet-production-backend'}}
        self.release = SimpleNamespace(db_sql=self.db_sql)
        self.restored = False

    def emit(self, kind, value):
        self.calls.append(('emit', kind))
        self.emitted.append((kind, value))

    def load_journal(self):
        return self.journal

    def env_of(self, name):
        return dict(self.meeting_env)

    def meeting_psql(self, sql):
        if sql == self.INVENTORY_SQL:
            return json.dumps(self.legacy)
        if sql == fa.PROJECTION_SQL:
            return json.dumps(self.after_switch)
        return json.dumps(self.meeting_refs)

    def db_sql(self, db, sql):
        if sql == fa.ORGANIZATION_IDS_SQL:
            return json.dumps(self.authoritative_ids)
        if sql == fa.EMPTY_IMPORT_SQL:
            return json.dumps(self.receipts)
        if db == 'cfmeet_organizations':
            return json.dumps(self.org_tables)
        return json.dumps(self.refs.get(db, {}))

    def core_databases(self):
        return ['cfmeet_calling', 'cfmeet_messaging', 'cfmeet_organizations', 'cfmeet_platform_access']

    def has_signing_identity(self, name):
        self.calls.append(('signing', name))

    def tls_route_present(self, port):
        self.calls.append(('tls', port))

    def fresh_backup(self, hours, required):
        self.calls.append(('fresh_backup', hours, tuple(required)))
        raise RuntimeError('No verified backup with restore tests within 24 h; run backup')

    def require_offsite(self, manifest):
        self.calls.append(('offsite', manifest['runId']))
        raise RuntimeError('Backup has no verified off-host copy and restore drill')

    def org_export(self, sha, run_id, confirm):
        self.calls.append(('export', run_id))
        return {'counts': self.export_counts}

    def org_import(self, sha, run_id, confirm, backup_gate=True):
        self.calls.append(('import', run_id))
        if self.import_writes_receipts:  # like the real CLI: import + idempotent re-import of an empty export
            for table in ('organization_import_runs', 'organization_audit_events'):
                self.org_tables[table] = self.org_tables.get(table, 0) + 2
                self.receipts[table] = self.receipts.get(table, 0) + 2
        if backup_gate:  # the standard import's gate 1 (real behaviour without any backup record)
            self.require_offsite(self.fresh_backup(24, ('meeting-service', 'organizations')))

    def private_url(self, port):
        return 'https://api.cfmeet.com:%d' % port

    def apply_activation(self, operation, sha, run_id, changes, irreversible=False, post=None):
        self.calls.append(('activation', operation, irreversible, json.dumps(changes)))
        self.meeting_env = dict(self.meeting_env, **changes[0][1])
        try:
            post()
        except Exception:
            self.restored = True
            self.meeting_env = {}
            raise


CONFIRM = 'empty-dataset'


def run(fake, confirm=CONFIRM):
    with patch.object(fa, 'mode_in_effect', lambda c: None):
        fa.activate(fake, SHA, '9-1', confirm)


class FirstActivation(unittest.TestCase):
    def test_empty_dataset_switches_to_authoritative_after_export_and_import_without_any_backup(self):
        fake = FakeCr()
        run(fake)
        steps = [c[0] for c in fake.calls if c[0] != 'emit']
        self.assertEqual(steps, ['signing', 'tls', 'export', 'import', 'activation'])
        activation = [c for c in fake.calls if c[0] == 'activation'][0]
        self.assertEqual(activation[1], 'org-cutover-first-activation')
        self.assertTrue(activation[2], 'the switch is journalled as irreversible')
        self.assertIn('"ORGANIZATIONS_AUTHORITY_MODE": "authoritative"', activation[3])
        self.assertIn('https://api.cfmeet.com:8450', activation[3])
        self.assertEqual(fake.meeting_env['ORGANIZATIONS_AUTHORITY_MODE'], 'authoritative')
        self.assertEqual([c[1] for c in fake.calls if c[0] == 'emit'].count('firstActivationInventory'), 2)

    def test_only_the_empty_dataset_confirmation_is_required(self):
        for confirm in ('', 'offsite-backup-verified', 'quiesced,offsite-backup-verified'):
            fake = FakeCr()
            with self.assertRaisesRegex(RuntimeError, 'confirmation missing'):
                run(fake, confirm)
            self.assertEqual(fake.calls, [])
        self.assertEqual(fa.CONFIRMATIONS, ('empty-dataset',))
        # A leftover offsite-backup-verified word is harmless and changes nothing.
        fake = FakeCr()
        run(fake, 'offsite-backup-verified,empty-dataset')
        self.assertFalse([c for c in fake.calls if c[0] in ('fresh_backup', 'offsite')])

    def test_nonempty_or_inconsistent_datasets_are_refused_before_any_change(self):
        cases = [dict(legacy={'organizations': 1}), dict(legacy={'memberships': 2}),
                 dict(legacy={'orphanSpaceOrganizations': 1}),
                 dict(meeting_refs={'meeting_spaces.organization_id': 3}),
                 dict(meeting_refs={'organization_event_inbox.organization_id': 1}),
                 dict(org_tables={'organizations': 1}), dict(org_tables={'organization_outbox': 4}),
                 dict(org_tables={'organization_invitations': 1}),
                 dict(refs={'cfmeet_platform_access': {'platform_organization_scopes.organization_ref': 1}}),
                 dict(refs={'cfmeet_messaging': {'conversations.organization_id': 1}})]
        for over in cases:
            fake = FakeCr(**over)
            with self.assertRaisesRegex(RuntimeError, 'not empty', msg=over):
                run(fake)
            self.assertFalse([c for c in fake.calls if c[0] in ('export', 'import', 'activation')], over)

    def test_empty_import_receipts_are_not_organization_data(self):
        # Production run 43473874426: the inventory right before the switch saw the empty import's
        # own receipts (2 import runs, 2 migration.imported audit events) and refused.
        fake = FakeCr()
        run(fake)
        self.assertEqual(fake.meeting_env['ORGANIZATIONS_AUTHORITY_MODE'], 'authoritative')
        inventories = [v for k, v in fake.emitted if k == 'firstActivationInventory']
        self.assertEqual(inventories[1]['organizationsService'], {})
        self.assertEqual(inventories[1]['emptyImportReceipts'],
                         {'organization_audit_events': 2, 'organization_import_runs': 2})

    def test_receipts_with_data_or_other_audit_rows_still_count(self):
        # An import run that imported anything is not an empty-import receipt: still refused.
        fake = FakeCr(org_tables={'organization_import_runs': 1, 'organization_audit_events': 3},
                      receipts={'organization_import_runs': 0, 'organization_audit_events': 1})
        with self.assertRaisesRegex(RuntimeError, 'not empty'):
            run(fake)
        self.assertFalse([c for c in fake.calls if c[0] in ('export', 'import', 'activation')])

    def test_retry_after_an_attempt_that_stopped_after_the_empty_import(self):
        # The journal holds the failed attempt's export and import; Organizations holds its receipts.
        fake = FakeCr(journal=[{'operation': 'org-cutover-export', 'status': 'done'},
                               {'operation': 'org-cutover-import', 'status': 'done'}],
                      org_tables={'organization_import_runs': 2, 'organization_audit_events': 2},
                      receipts={'organization_import_runs': 2, 'organization_audit_events': 2})
        run(fake)
        self.assertEqual(fake.meeting_env['ORGANIZATIONS_AUTHORITY_MODE'], 'authoritative')

    def test_missing_organizations_schema_is_refused(self):
        fake = FakeCr()
        fake.org_tables = {}
        with self.assertRaisesRegex(RuntimeError, 'not found'):
            run(fake)

    def test_only_a_first_time_activation_from_legacy_mode(self):
        for journal in ([{'operation': 'org-cutover-import', 'status': 'running'}],
                        [{'operation': 'org-cutover-shadow', 'status': 'running'}],
                        [{'operation': 'org-cutover-authoritative', 'status': 'done'}],
                        [{'operation': 'org-cutover-first-activation', 'status': 'done'}]):
            with self.assertRaisesRegex(RuntimeError, 'Not a first-time activation'):
                run(FakeCr(journal=journal))
        for mode in ('shadow', 'authoritative'):
            with self.assertRaisesRegex(RuntimeError, 'mode'):
                run(FakeCr(meeting_env={'ORGANIZATIONS_AUTHORITY_MODE': mode}))
        # A failed earlier attempt was fully restored: it may be retried; a recorded export is fine.
        run(FakeCr(journal=[{'operation': 'org-cutover-first-activation', 'status': 'failed'},
                            {'operation': 'org-cutover-export', 'status': 'done'}],
                   meeting_env={'ORGANIZATIONS_AUTHORITY_MODE': 'legacy'}))

    def test_no_backup_prerequisite_and_no_backup_claim(self):
        # Owner decision 2026-11-13 (ADR 0059 Amendment 1): neither the off-host record, a fresh
        # local backup, nor a restore drill is consulted; the decision is recorded, never a pass.
        fake = FakeCr()
        run(fake)
        self.assertFalse([c for c in fake.calls if c[0] in ('fresh_backup', 'offsite')])
        decision = dict(fake.emitted)['backupGate']
        self.assertEqual((decision['required'], decision['offsiteBackupVerified']), (False, False))
        self.assertIn('owner decision', decision['reason'])
        self.assertFalse(dict(fake.emitted)['firstActivation']['backupVerified'])
        self.assertNotIn('offsiteBackup', dict(fake.emitted))
        self.assertNotIn('offsite_backup', fa.activate.__code__.co_names)

    def test_works_without_backup_keys_in_the_environment(self):
        with patch.dict('os.environ', {}, clear=False):
            for key in ('CFMEET_BACKUP_AGE_RECIPIENTS', 'CFMEET_BACKUP_AGE_IDENTITY'):
                __import__('os').environ.pop(key, None)
            run(FakeCr())

    def test_standard_import_keeps_gate_one(self):
        with patch.object(cr, 'fresh_backup', side_effect=RuntimeError('No verified backup')):
            with self.assertRaisesRegex(RuntimeError, 'Owner gate 1'):
                cr.org_import(SHA, '9-1', '')
            with self.assertRaisesRegex(RuntimeError, 'No verified backup'):
                cr.org_import(SHA, '9-1', 'offsite-backup-verified')

    def test_first_activation_import_skips_every_backup_check(self):
        import tempfile
        with tempfile.TemporaryDirectory() as d, \
                patch.object(cr, 'fresh_backup', side_effect=AssertionError('backup consulted')), \
                patch.object(cr, 'require_offsite', side_effect=AssertionError('off-host consulted')), \
                patch.object(cr, 'CUTOVER', cr.Path(d)):
            # Past the (absent) gate the real import reads the latest export record.
            with self.assertRaises(FileNotFoundError):
                cr.org_import(SHA, '9-1', 'empty-dataset', backup_gate=False)

    def test_a_nonempty_export_is_refused(self):
        fake = FakeCr(export_counts={'organizations': 1, 'memberships': 1})
        with self.assertRaisesRegex(RuntimeError, 'export is not empty'):
            run(fake)
        self.assertFalse([c for c in fake.calls if c[0] in ('import', 'activation')])

    def test_legacy_write_in_the_window_restores_legacy_while_organizations_is_empty(self):
        fake = FakeCr(after_switch={'memberships': 1, 'organizationIds': ['x']})
        with self.assertRaisesRegex(RuntimeError, 'legacy restored'):
            run(fake)
        self.assertTrue(fake.restored)

    def test_both_sides_written_stays_authoritative_and_fails_for_reconciliation(self):
        fake = FakeCr(after_switch={'memberships': 0, 'organizationIds': ['stray', 'real']}, authoritative_ids=['real'])
        with self.assertRaisesRegex(RuntimeError, 'manual reconciliation'):
            run(fake)
        self.assertFalse(fake.restored)
        self.assertEqual(fake.meeting_env['ORGANIZATIONS_AUTHORITY_MODE'], 'authoritative')

    def test_authoritative_projections_are_not_legacy_writes(self):
        fake = FakeCr(after_switch={'memberships': 0, 'organizationIds': ['a']}, authoritative_ids=['a', 'b'])
        run(fake)
        self.assertFalse(fake.restored)

    def test_dispatcher_runs_it_after_org_events_and_it_unlocks_enterprise_calling(self):
        requires, _ = cr.ACTIVATIONS['org-cutover-first-activation']
        self.assertEqual(requires, ('activate-org-events',))
        grants = [g for g in cr.GRANTS if g[0] == 'org-cutover-first-activation']
        shadow = [g for g in cr.GRANTS if g[0] == 'org-cutover-shadow']
        self.assertEqual([g[1:] for g in grants], [g[1:] for g in shadow])
        journal = [{'operation': 'activate-provisioning', 'status': 'done'},
                   {'operation': 'activate-org-events', 'status': 'done'},
                   {'operation': 'activate-organization-bindings', 'status': 'done'},
                   {'operation': 'org-cutover-first-activation', 'status': 'done'}]
        seen = []
        with patch.object(cr, 'load_journal', lambda: journal), patch.object(cr.p, 'identity', lambda: None), \
                patch.object(cr, 'require_release', lambda sha: sha), \
                patch.object(cr, 'activate_enterprise_calling', lambda *a: seen.append(a)), \
                patch.dict(cr.ACTIVATIONS, {'activate-enterprise-calling': (
                    cr.ACTIVATIONS['activate-enterprise-calling'][0], lambda *a: seen.append(a))}), \
                patch.object(cr, 'WORK', cr.Path('/nonexistent')):
            with patch.object(cr.Path, 'mkdir', lambda *a, **k: None):
                cr.run('activate-enterprise-calling', SHA, '5-1')
        self.assertEqual(len(seen), 1)
        with patch.object(cr, 'load_journal', lambda: journal[:3]), patch.object(cr.Path, 'mkdir', lambda *a, **k: None):
            with self.assertRaisesRegex(RuntimeError, 'org-cutover-authoritative or org-cutover-first-activation'):
                cr.run('activate-enterprise-calling', SHA, '5-1')

    def test_inventory_sql_reports_counts_only(self):
        for sql in (fa.reference_sql(), fa.table_rows_sql(), fa.PROJECTION_SQL, fa.ORGANIZATION_IDS_SQL,
                    fa.EMPTY_IMPORT_SQL):
            self.assertIn('READ ONLY', sql)
            self.assertNotRegex(sql.lower(), r'\b(insert|update|delete|drop|alter|truncate)\b')
            for column in ('slug', 'email', 'name'):
                self.assertNotIn('select ' + column, sql.lower())
        self.assertIn("'organization_service_nonces'", fa.table_rows_sql())


if __name__ == '__main__':
    unittest.main()
