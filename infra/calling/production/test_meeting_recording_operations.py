import json
import sys
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
                patch('meeting_recording_acceptance.run', side_effect=RuntimeError('capture failed')), \
                patch.object(mr, 'ensure_callback') as callback, \
                patch.object(mr, 'recording_provider_overrides', return_value={mr.FLAG: 'true'}):
            mr.activate(cr, 'sha', '2', 'enable-30-days')
            callback.assert_not_called()
            self.assertEqual(apply.call_args.args[3], [('meeting-service', {mr.FLAG: 'true'})])
            with self.assertRaisesRegex(RuntimeError, 'capture failed'):
                apply.call_args.kwargs['post']()
            callback.assert_called_once_with(cr, '2')

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


class RecordingProviderIdentity(unittest.TestCase):
    def test_signer_comparison_emits_only_booleans(self):
        for secret, expected in [('provider-secret', True), ('different-provider-secret', False)]:
            cr = SimpleNamespace(env_of=Mock(return_value={'LIVEKIT_API_KEY': 'provider-id',
                                                          'LIVEKIT_API_SECRET': secret}), emit=Mock())
            with patch.object(mr.p, 'secret', side_effect=['provider-id', 'provider-secret']):
                mr.provider_identity(cr)
            kind, result = cr.emit.call_args.args
            self.assertEqual(kind, 'meetingRecordingProviderIdentity')
            self.assertEqual(result, dict(meetingSignerConfigured=True, sharesCallingSigner=expected, rtcEndpoint=None))
            self.assertNotIn('provider-', json.dumps(result))

    def test_missing_provider_identity_is_unknown_and_does_not_read_secrets(self):
        cr = SimpleNamespace(env_of=Mock(return_value={}), emit=Mock())
        with patch.object(mr.p, 'secret') as secret:
            mr.provider_identity(cr)
        secret.assert_not_called()
        self.assertIsNone(cr.emit.call_args.args[1]['sharesCallingSigner'])

    def test_endpoint_projection_excludes_credentials_path_query_and_unknown_hosts(self):
        for url, expected in (
            ('wss://user:private-token@livekit.cfmeet.com:8446/private?token=private-token',
             dict(scheme='wss', hostname='livekit.cfmeet.com', port=8446)),
            ('wss://api.cfmeet.com', dict(scheme='wss', hostname='api.cfmeet.com', port=443)),
            ('wss://private-token.invalid', None), ('wss://livekit.cfmeet.com:invalid', None)):
            cr = SimpleNamespace(env_of=lambda _: {'LIVEKIT_URL': url}, emit=Mock())
            mr.provider_identity(cr)
            value = cr.emit.call_args.args[1]
            self.assertEqual(value['rtcEndpoint'], expected)
            self.assertNotIn('private-token', json.dumps(value))


class RecordingProviderActivation(unittest.TestCase):
    def test_only_existing_same_server_keys_can_be_aligned_and_busy_media_is_preserved(self):
        for current, callback, busy, active, valid in (
            ('meeting-secret', 'callback-secret', False, False, True),
            ('wrong', 'callback-secret', False, False, False),
            ('meeting-secret', 'wrong', False, False, False),
            ('meeting-secret', 'callback-secret', True, False, False),
            ('meeting-secret', 'callback-secret', False, True, False)):
            with tempfile.TemporaryDirectory() as directory:
                config = Path(directory) / 'livekit.yaml'
                original = json.dumps(dict(keys={'meeting': 'meeting-secret', 'callback': 'callback-secret'},
                    webhook=dict(api_key='callback', urls=[mr.media_webhook.BRIDGE_URL])))
                config.write_text(original)
                cr = SimpleNamespace(env_of=lambda _: dict(LIVEKIT_API_KEY='meeting', LIVEKIT_API_SECRET=current),
                    SERVICES={'meeting-service': dict(container='meeting', port=8081)})
                yaml = SimpleNamespace(safe_load=json.loads)
                container = {'NetworkSettings': {'Networks': {mr.p.NET: {}}}}
                with patch.dict(sys.modules, yaml=yaml), patch.object(mr.media_webhook, 'CONFIG', config), \
                        patch.object(mr.media_webhook, 'calling_key', return_value=('callback', callback)), \
                        patch.object(mr.p, 'inspect', return_value=container), \
                        patch.object(mr.media_webhook, 'participants', return_value=int(busy)), \
                        patch.object(mr.p, 'lk_request', return_value={'items': [1] if active else []}):
                    if valid:
                        self.assertEqual(mr.recording_provider_overrides(cr),
                            {mr.FLAG: 'true', 'LIVEKIT_API_KEY': 'callback', 'LIVEKIT_API_SECRET': callback})
                    else:
                        with self.assertRaises(RuntimeError):
                            mr.recording_provider_overrides(cr)
                    self.assertEqual(config.read_text(), original)


class MeetingCallbacks(unittest.TestCase):
    def test_plan_adds_only_meeting_target_and_is_idempotent(self):
        before = dict(keys={'key': 'secret', 'other': 'preserved'}, rtc={'udp_port': 7882},
                      webhook=dict(api_key='key', urls=['calling']))
        after, changed = mr.callback_plan(before, 'calling', 'meeting', 'key', 'secret')
        self.assertTrue(changed)
        self.assertEqual(after['keys'], before['keys'])
        self.assertEqual(after['rtc'], before['rtc'])
        self.assertEqual(before['webhook']['urls'], ['calling'])
        self.assertEqual(after['webhook']['urls'], ['calling', 'meeting'])
        self.assertFalse(mr.callback_plan(after, 'calling', 'meeting', 'key', 'secret')[1])
        for config in (dict(keys={'key': 'different'}, webhook=before['webhook']),
                       dict(keys=before['keys'], webhook=dict(api_key='key', urls=['unknown']))):
            with self.assertRaises(RuntimeError):
                mr.callback_plan(config, 'calling', 'meeting', 'key', 'secret')

    def test_different_signers_are_refused_before_provider_access(self):
        cr = SimpleNamespace(env_of=Mock(return_value={'LIVEKIT_API_KEY': 'different'}))
        yaml = SimpleNamespace(safe_load=json.loads, safe_dump=lambda value, **_: json.dumps(value))
        with patch.dict(sys.modules, yaml=yaml), patch.object(mr.media_webhook, 'calling_key', return_value=('key', 'secret')), \
                patch.object(mr.p, 'inspect') as inspect:
            with self.assertRaisesRegex(RuntimeError, 'signers differ'):
                mr.ensure_callback(cr, '12345-1')
            inspect.assert_not_called()

    def test_callback_restart_is_idle_only_and_restores_configuration_on_failure(self):
        for busy, fails in ((True, False), (False, True), (False, False)):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                config = root / 'livekit.yaml'
                before = json.dumps(dict(keys={'key': 'secret'}, rtc={'udp_port': 7882},
                    webhook=dict(api_key='key', urls=[mr.media_webhook.BRIDGE_URL])))
                config.write_text(before)
                cr = SimpleNamespace(env_of=Mock(return_value={'LIVEKIT_API_KEY': 'key', 'LIVEKIT_API_SECRET': 'secret'}),
                    SERVICES={'meeting-service': {'container': 'cfmeet-production-backend', 'port': 8081}}, emit=Mock())
                yaml = SimpleNamespace(safe_load=json.loads, safe_dump=lambda value, **_: json.dumps(value))
                container = {'HostConfig': {'NetworkMode': 'bridge'}, 'NetworkSettings': {'Networks': {mr.p.NET: {}}}}
                with patch.dict(sys.modules, yaml=yaml), patch.object(mr.media_webhook, 'CONFIG', config), \
                        patch.object(mr.p, 'ROOT', root), patch.object(mr.p, 'inspect', return_value=container), \
                        patch.object(mr.media_webhook, 'calling_key', return_value=('key', 'secret')), \
                        patch.object(mr.media_webhook, 'route_status', return_value=401), \
                        patch.object(mr.media_webhook, 'participants', return_value=1 if busy else 0), \
                        patch.object(mr.p, 'lk_request', return_value={'items': []}), patch.object(mr.p, 'regression'), \
                        patch.object(mr.media_webhook, 'restart_and_wait', side_effect=[RuntimeError('restart'), None] if fails else None) as restart:
                    if busy or fails:
                        with self.assertRaises(RuntimeError):
                            mr.ensure_callback(cr, '12345-1')
                        self.assertEqual(config.read_text(), before)
                        self.assertEqual(restart.call_count, 2 if fails else 0)
                    else:
                        mr.ensure_callback(cr, '12345-1')
                        self.assertEqual(json.loads(config.read_text())['rtc'], {'udp_port': 7882})
                        self.assertEqual(len(json.loads(config.read_text())['webhook']['urls']), 2)
                        self.assertTrue(cr.emit.call_args.args[1]['keysPreserved'])
                        restart.assert_called_once()
