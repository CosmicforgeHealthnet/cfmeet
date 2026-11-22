import datetime as dt
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

import audit_operations as operations
import service_log_collector as collector


class ServiceLogs(unittest.TestCase):
    def test_catalogue_and_owned_dependencies_are_collected_without_foreign_or_staging_containers(self):
        names = operations.log_containers({'calling': {'container': 'cfmeet-calling'}}, {
            'cfmeet-calling', 'cfmeet-calling-artifacts', 'cfmeet-calling-redis',
            'cfmeet-messaging-kt-witness', 'cfmeet-staging-calling', 'cosmicforge-other', 'lk-server',
        })
        self.assertEqual(names, ['cfmeet-calling', 'cfmeet-calling-artifacts',
                                 'cfmeet-calling-redis', 'cfmeet-messaging-kt-witness'])

    def test_archives_exact_bytes_and_restores_the_last_durable_cursor(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            line = b'2026-11-21T01:02:03.123456789Z payload\xff\n'
            archive = collector.Archive(root, 'cfmeet-calling')
            archive.append(line)
            archive.close()
            self.assertEqual((root / 'cfmeet-calling' / '2026-11-21.log').read_bytes(), line)
            restored = collector.Archive(root, 'cfmeet-calling')
            self.assertEqual(restored.cursor, '2026-11-21T01:02:03.123456789Z')
            restored.close()

    def test_retains_fourteen_complete_days_and_excludes_unrelated_files(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            for name in ('2026-11-06.log', '2026-11-07.log', '2026-11-21.log', 'cursor.json', 'foreign.log'):
                (root / name).write_text('data')
            expired = list(collector.expired_archives(root, dt.date(2026, 11, 21)))
            self.assertEqual([p.name for p in expired], ['2026-11-06.log'])

    def test_container_names_cannot_escape_the_owned_archive(self):
        with tempfile.TemporaryDirectory() as folder:
            for name in ('../other', 'cosmicforge-other', 'cfmeet-../../etc'):
                with self.assertRaises(ValueError):
                    collector.Archive(Path(folder), name)

    def test_unit_cannot_send_logs_to_the_network_and_keeps_archives_private(self):
        unit = operations.log_unit(Path('/opt/cfmeet-core'))
        self.assertIn('RestrictAddressFamilies=AF_UNIX', unit)
        self.assertIn('UMask=0077', unit)
        self.assertIn('ProtectSystem=strict', unit)
        self.assertIn('MemoryMax=512M', unit)

    def test_activation_and_rollback_refuse_unreviewed_inputs(self):
        fake = Mock()
        with self.assertRaisesRegex(RuntimeError, 'confirm'):
            operations.activate_service_logs(fake, 'a' * 40, '1-1', '')
        fake.require_release.assert_not_called()
        with patch.object(operations.p, 'run') as run:
            with self.assertRaisesRegex(RuntimeError, 'rollback path'):
                operations.restore_service_logs({'files': {'/etc/passwd': None}, 'wasActive': False})
            run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
