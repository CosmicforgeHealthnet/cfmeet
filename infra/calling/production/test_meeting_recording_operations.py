import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

import meeting_recording_operations as mr


class RecordingStorage(unittest.TestCase):
    def test_signed_route_keeps_host_and_never_accepts_uploads_or_logs_signatures(self):
        route = mr.route_text()
        self.assertIn('/' + mr.BUCKET + '/', route)
        self.assertIn('limit_except GET HEAD { deny all; }', route)
        self.assertIn('proxy_set_header Host $http_host;', route)
        self.assertIn('access_log off;', route)
        self.assertIn(mr.relay.ADDRESS, route)
        self.assertNotIn('proxy_cache', route)

    def test_site_change_is_idempotent_and_refuses_unknown_or_duplicate_layouts(self):
        before = 'server {\n    ' + mr.ANCHOR + '\n}\n'
        after = mr.site_text(before)
        self.assertEqual(after.count(mr.INCLUDE), 1)
        self.assertEqual(mr.site_text(after), after)
        for bad in ('server {}', before + before, after + mr.INCLUDE):
            with self.assertRaises(RuntimeError):
                mr.site_text(bad)

    def test_identity_is_scoped_to_the_meeting_bucket_and_existing_disabled_keys_stay_disabled(self):
        for disabled in (False, True):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                secrets = root / 'secrets'
                def private(path, value):
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(value)
                def root_file(name):
                    path = secrets / name
                    return path, path.read_text()
                info = SimpleNamespace(returncode=0, stdout=json.dumps({'userStatus': 'disabled' if disabled else 'enabled'}), stderr='')
                with patch.object(mr.p, 'ROOT', root), patch.object(mr.p, 'SECRETS', secrets), \
                        patch.object(mr.p, 'write_private', private), patch.object(mr.providers, 'root_file', root_file), \
                        patch.object(mr.p, 'inspect', return_value={'NetworkSettings': {'Networks': {mr.p.NET: {'IPAddress': '172.18.0.2'}}}}), \
                        patch.object(mr.p, 'secret', return_value='test-only'), \
                        patch.object(mr.storage, 'mc') as mc, patch.object(mr.subprocess, 'run', return_value=info):
                    if disabled:
                        with self.assertRaisesRegex(RuntimeError, 'disabled'):
                            mr.identity()
                        self.assertFalse(any(call.args[2:4] == ('policy', 'attach') for call in mc.call_args_list))
                        continue
                    paths, _, _ = mr.identity()
                    self.assertEqual(len(paths), 2)
                    policy = json.loads((root / 'meeting-recording' / 'storage-policy.json').read_text())
                    resources = [r for statement in policy['Statement'] for r in statement['Resource']]
                    self.assertEqual(resources, ['arn:aws:s3:::' + mr.BUCKET, 'arn:aws:s3:::' + mr.BUCKET + '/artifacts/*'])
                    self.assertTrue(any(call.args[1:3] == ('encrypt', 'set') for call in mc.call_args_list))
                    self.assertTrue(any(call.args[1:3] == ('quota', 'set') for call in mc.call_args_list))

    def test_enable_requires_prepared_storage_and_runs_real_acceptance_before_success(self):
        apply = Mock()
        cr = SimpleNamespace(require_release=Mock(), env_of=lambda _: {}, apply_activation=apply, emit=Mock(), mem_available_mib=lambda:4096)
        with patch.object(mr, 'facts', return_value={'recordingStorageProtocol': 1}):
            with self.assertRaisesRegex(RuntimeError, 'Prepare'):
                mr.activate(cr, 'sha', '1', 'enable-30-days')
        apply.assert_not_called()
        cr.env_of = lambda _: {'ARTIFACT_STORAGE_BUCKET': mr.BUCKET, 'MEETING_RECORDING_STORAGE_PREPARED': 'true'}
        with patch.object(mr, 'facts', return_value={'recordingStorageProtocol': 1}), \
                patch('meeting_recording_acceptance.run', side_effect=RuntimeError('capture failed')):
            mr.activate(cr, 'sha', '2', 'enable-30-days')
            self.assertEqual(apply.call_args.args[3], [('meeting-service', {mr.FLAG: 'true'})])
            with self.assertRaisesRegex(RuntimeError, 'capture failed'):
                apply.call_args.kwargs['post']()

    def test_confirmation_and_protocol_are_checked_before_storage_mutations(self):
        cr = SimpleNamespace(require_release=Mock())
        with patch.object(mr, 'identity') as identity:
            with self.assertRaises(RuntimeError):
                mr.activate(cr, 'sha', '1', 'yes')
            cr.require_release.assert_not_called()
            with patch.object(mr, 'facts', return_value={}):
                with self.assertRaisesRegex(RuntimeError, 'Release'):
                    mr.activate(cr, 'sha', '1', 'prepare')
            identity.assert_not_called()
