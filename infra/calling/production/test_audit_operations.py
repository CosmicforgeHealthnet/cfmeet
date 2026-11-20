import copy
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
