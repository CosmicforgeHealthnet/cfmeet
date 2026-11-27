import copy
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

import audit_operations as ao
import coordinated_release as cr


class PersistentLogs(unittest.TestCase):
    def test_log_mount_survives_clones_without_exposing_other_host_paths(self):
        cfg = {'Env': ['APP_ENV=prod'], 'User': '10001:10002', 'HostConfig': {'Mounts': []}}
        with tempfile.TemporaryDirectory() as folder, patch.object(ao.p, 'ROOT', Path(folder)), \
                patch.object(ao.p, 'run') as run:
            ao.persistent_meeting_logs(cfg)
            before = copy.deepcopy(cfg)
            ao.persistent_meeting_logs(cfg)
            self.assertEqual(cfg, before)
            mount, = cfg['HostConfig']['Mounts']
            self.assertEqual(mount['Source'], str(Path(folder) / 'logs' / 'meeting-service'))
            self.assertEqual(mount['Target'], '/var/log/cfmeet')
            self.assertFalse(mount['ReadOnly'])
            self.assertIn('CFMEET_LOG_DIRECTORY=/var/log/cfmeet', cfg['Env'])
            self.assertEqual(run.call_args.args[:8], ('install', '-d', '-m', '0750', '-o', '10001', '-g', '10002'))

    def test_unrelated_or_readonly_log_mount_is_refused_before_mutation(self):
        for host in ({'Mounts': [dict(Target=ao.LOG_TARGET, Source='/etc', ReadOnly=False)]},
                     {'Binds': ['/etc:/var/log/cfmeet:rw']}):
            with patch.object(ao.p, 'run') as run:
                with self.assertRaisesRegex(RuntimeError, 'Conflicting'):
                    ao.persistent_meeting_logs({'HostConfig': host})
                run.assert_not_called()


class Capacity(unittest.TestCase):
    def test_actual_production_plan_fits_224_with_memory_and_operator_reserve(self):
        self.assertEqual(ao.capacity_target({'services': 189}, 100, 3, 5029, 15), 224)
        self.assertEqual(ao.capacity_target({'services': 189}, 224, 3, 512, 15), 224)
        with self.assertRaisesRegex(RuntimeError, 'memory'):
            ao.capacity_target({'services': 189}, 100, 3, 3000, 15)
        with self.assertRaisesRegex(RuntimeError, 'ceiling'):
            ao.capacity_target({'services': 300}, 100, 3, 16000, 15)

    def test_restart_is_refused_without_the_explicit_operation_confirmation(self):
        fake = Mock()
        with self.assertRaisesRegex(RuntimeError, 'restart-core-database'):
            ao.activate_database_capacity(fake, 'a' * 40, '1-1', '')
        fake.require_release.assert_not_called()

    def fake(self):
        entry = {'components': [], 'status': 'running'}
        fake = SimpleNamespace(require_release=Mock(), begin=Mock(return_value=entry), commit=Mock(), emit=Mock(),
            release=SimpleNamespace(db_sql=Mock(side_effect=['100', '3'])), running_envs=Mock(return_value={}),
            pool_plan=Mock(return_value={'services': 189}), mem_available_mib=Mock(return_value=5029),
            DATABASE_POOLS=cr.DATABASE_POOLS, POOL_SETTINGS=cr.POOL_SETTINGS, POOL_HEADROOM=15)
        return fake, entry

    def test_regression_failure_restores_the_old_database_capacity(self):
        fake, entry = self.fake()
        with patch.object(ao, 'set_capacity') as setting, patch.object(ao.p, 'regression', side_effect=RuntimeError('health')):
            with self.assertRaisesRegex(RuntimeError, 'health'):
                ao.activate_database_capacity(fake, 'a' * 40, '1-1', ao.CAPACITY_CONFIRM)
        self.assertEqual([c.args[1] for c in setting.call_args_list], [224, 100])
        self.assertEqual(entry['status'], 'failed')

    def test_success_records_the_capacity_without_changing_application_pools(self):
        fake, entry = self.fake()
        with patch.object(ao, 'set_capacity') as setting, patch.object(ao.p, 'regression'):
            ao.activate_database_capacity(fake, 'a' * 40, '1-1', ao.CAPACITY_CONFIRM)
        setting.assert_called_once_with(fake, 224)
        self.assertEqual(entry['databaseCapacity'], {'before': 100, 'after': 224})
        self.assertEqual(entry['status'], 'done')

    def test_rollback_refuses_a_capacity_smaller_than_live_pools(self):
        fake = SimpleNamespace(release=SimpleNamespace(db_sql=Mock(return_value='3')),
            pool_plan=Mock(return_value={'services': 189}), running_envs=Mock(return_value={}),
            pool_budget_errors=cr.pool_budget_errors)
        with patch.object(ao, 'set_capacity') as setting:
            with self.assertRaisesRegex(RuntimeError, 'Roll back application pools'):
                ao.restore_capacity(fake, {'before': 100, 'after': 224})
            setting.assert_not_called()


if __name__ == '__main__':
    unittest.main()


class RecordingReceipts(unittest.TestCase):
    def test_campaign_receipt_requires_all_real_acceptance_checks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            folder = root/'meeting-recording/acceptance/12345-1-audit-recording-enable'
            folder.mkdir(parents=True)
            report = dict(result='PASS', phase='deletion', checks={key: 'PASS' for key in ao.RECORDING_CHECKS})
            file = folder/'report.json'
            file.write_text(json.dumps(report))
            with patch.object(ao.p, 'ROOT', root):
                self.assertTrue(ao.recording_acceptance_proven())
                cr = SimpleNamespace(emit=Mock())
                ao.recording_receipts(cr)
                self.assertEqual(len(cr.emit.call_args.args[1]), 1)
                report['checks'].pop('authenticated_recording_deletion')
                file.write_text(json.dumps(report))
                self.assertFalse(ao.recording_acceptance_proven())

    def test_sdk_error_classes_survive_without_exception_messages(self):
        for error in ('ConnectError', 'PublishTrackError'):
            result = ao.recording_receipt(dict(result='FAIL', phase='synthetic_media',
                errorClass=error, message='private-token'), '12345-1')
            self.assertEqual(result['errorClass'], error)
            self.assertNotIn('private-token', json.dumps(result))

    def test_only_known_receipt_fields_are_emitted_and_private_files_are_not_read(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            folder = root / 'meeting-recording' / 'acceptance' / '12345-1'
            folder.mkdir(parents=True)
            (folder / 'credentials.private.json').write_text('private-token')
            (folder / 'report.json').write_text(json.dumps(dict(result='FAIL', phase='capture',
                errorClass='AssertionError', url='https://private-token', secret='private-token',
                checks={'real_rtc_synthetic_media': 'PASS', 'private-token': 'PASS'})))
            fake = SimpleNamespace(emit=Mock())
            with patch.object(ao.p, 'ROOT', root):
                ao.recording_receipts(fake)
            key, receipts = fake.emit.call_args.args
            self.assertEqual(key, 'meetingRecordingReceipts')
            self.assertEqual(receipts[0]['phase'], 'capture')
            self.assertEqual(receipts[0]['checks'], {'real_rtc_synthetic_media': 'PASS'})
            self.assertNotIn('private-token', json.dumps(receipts))

    def test_malformed_receipt_fields_cannot_escape_the_allowlist(self):
        value = ao.recording_receipt(dict(result='private-token', phase={'private-token': True},
                                       errorClass='private-token', checks=[]), '12345-1')
        self.assertNotIn('private-token', json.dumps(value))
        self.assertEqual(value['result'], 'INVALID')


class ControlledRecordingState(unittest.TestCase):
    def test_query_is_scoped_to_all_three_fixture_ids_and_drops_private_fields(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            ids = dict(recording='00000000-0000-4000-8000-000000000001',
                       conference='00000000-0000-4000-8000-000000000002',
                       space='00000000-0000-4000-8000-000000000003')
            (folder / 'cleanup.json').write_text(json.dumps(ids))
            cr = SimpleNamespace(meeting_psql=Mock(return_value=json.dumps(dict(
                status='FAILED', failureCode='RECORDING_START_TIMEOUT', providerAssigned=True,
                storageUri='private-token', failureDetail='private-token'))))
            result = ao.owned_recording_state(cr, folder)
            self.assertEqual(result, dict(status='FAILED', failureCode='RECORDING_START_TIMEOUT', providerAssigned=True))
            query = cr.meeting_psql.call_args.args[0]
            for value in ids.values():
                self.assertIn(value, query)
            self.assertNotIn('private-token', json.dumps(result))

    def test_invalid_fixture_ids_never_reach_the_database(self):
        with tempfile.TemporaryDirectory() as directory:
            folder = Path(directory)
            (folder / 'cleanup.json').write_text(json.dumps(dict(recording="injection", conference='x', space='y')))
            cr = SimpleNamespace(meeting_psql=Mock())
            self.assertIsNone(ao.owned_recording_state(cr, folder))
            cr.meeting_psql.assert_not_called()
