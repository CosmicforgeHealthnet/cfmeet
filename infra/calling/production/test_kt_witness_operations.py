import base64
import json
import re
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

import kt_witness_operations as w


class ManagedWitness(unittest.TestCase):
    def test_only_explicit_managed_confirmation_can_mutate(self):
        self.assertRegex(w.WITNESS, re.compile(r'^[a-z0-9][a-z0-9.:-]{2,63}$'))
        cr = SimpleNamespace(require_release=Mock())
        with patch.object(w, 'log_identity') as identity:
            with self.assertRaisesRegex(RuntimeError, 'operator-managed'):
                w.activate(cr, 'sha', 'run', 'independent')
            cr.require_release.assert_not_called()
            identity.assert_not_called()

    def test_route_is_post_only_and_layout_is_checked_without_replacing_unknown_content(self):
        before = 'server {\n' + w.layout.ANCHOR + '\n}\n'
        after = w.site_text(before)
        self.assertEqual(w.site_text(after), after)
        for bad in ('server {}', before + before, after + w.INCLUDE):
            with self.assertRaises(RuntimeError):
                w.site_text(bad)
        self.assertIn('limit_except POST', w.route_text())
        self.assertIn('127.0.0.1:8109', w.route_text())
        self.assertIn('client_max_body_size 64k', w.route_text())

    def test_public_key_requires_an_ed25519_der_encoding(self):
        prefix = bytes.fromhex('302a300506032b6570032100')
        for data, ok in [(prefix + b'x' * 32, True), (b'x' * 44, False), (prefix + b'x' * 31, False)]:
            with patch.object(w.subprocess, 'run', return_value=SimpleNamespace(returncode=0, stdout=data)):
                if ok:
                    self.assertEqual(w.public_key('private-file'), base64.urlsafe_b64encode(b'x' * 32).decode().rstrip('='))
                else:
                    with self.assertRaises(RuntimeError):
                        w.public_key('private-file')

    def test_missing_log_mount_never_generates_a_new_log_identity(self):
        cr = SimpleNamespace(SERVICES={'messaging': {'container': 'messaging'}})
        with patch.object(w.p, 'inspect', return_value={'Image': 'image', 'Mounts': []}), \
                patch.object(w.p, 'env_of', return_value={}), patch.object(w, 'public_key') as public:
            with self.assertRaises(RuntimeError):
                w.log_identity(cr)
            public.assert_not_called()

    def test_directory_mounted_log_key_is_resolved_without_replacing_it(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            c = {'Image': 'image', 'Mounts': [{'Type': 'bind', 'Destination': '/keys', 'Source': str(root)}]}
            env = {'MESSAGING_KT_SIGNING_KEY_FILE': '/keys/log.pem', 'MESSAGING_KT_LOG_ID': 'pinned-log'}
            cr = SimpleNamespace(SERVICES={'messaging': {'container': 'messaging'}})
            with patch.object(w.p, 'inspect', return_value=c), patch.object(w.p, 'env_of', return_value=env), \
                    patch.object(w, 'public_key', return_value='public') as public:
                self.assertEqual(w.log_identity(cr), ('image', 'pinned-log', 'public'))
                public.assert_called_once_with((root / 'log.pem').resolve())

    def test_activation_is_scoped_and_requires_a_verified_current_head_with_rollback(self):
        for fail in (False, True):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                site = root / 'site'
                before = 'server {\n' + w.layout.ANCHOR + '\n}\n'
                site.write_text(before)
                snippet = root / 'snippet'
                emit = Mock()
                def apply(*args, **kwargs):
                    if fail:
                        raise RuntimeError('activation failed')
                    kwargs['post']()
                cr = SimpleNamespace(require_release=Mock(), emit=emit, apply_activation=Mock(side_effect=apply),
                                     release=SimpleNamespace(db_sql=Mock(side_effect=['0', '1'])))
                def command(*args):
                    if args[:2] == ('openssl', 'genpkey'):
                        return 'private-test-key'
                    return ''
                with patch.object(w.p, 'ROOT', root), patch.object(w.p, 'SECRETS', root / 'secrets'), \
                        patch.object(w.layout, 'SITE', site), patch.object(w, 'SNIPPET', snippet), \
                        patch.object(w, 'log_identity', return_value=('image', 'pinned-log', 'public-log-key')), \
                        patch.object(w, 'public_key', return_value='public-witness-key'), \
                        patch.object(w.providers, 'root_file'), patch.object(w.p, 'reload_host_nginx'), \
                        patch.object(w.p, 'run', side_effect=command) as run:
                    if fail:
                        with self.assertRaisesRegex(RuntimeError, 'activation failed'):
                            w.activate(cr, 'sha', 'run', 'operator-managed')
                        self.assertEqual(site.read_text(), before)
                        self.assertFalse(snippet.exists())
                        self.assertTrue(any(c.args == ('docker', 'rm', '-f', w.NAME) for c in run.call_args_list))
                    else:
                        w.activate(cr, 'sha', 'run', 'operator-managed')
                        report = emit.call_args.args[1]
                        self.assertFalse(report['independentOperator'])
                        self.assertTrue(report['cosignaturesVerified'])
                        config = json.loads((root / 'managed-kt-witness' / 'witnesses.json').read_text())
                        self.assertEqual(config[0]['url'], 'https://api.cfmeet.com' + w.PATH)
                        self.assertNotIn('private-test-key', json.dumps(report))
                        docker = next(c.args for c in run.call_args_list if c.args[:3] == ('docker', 'run', '-d'))
                        self.assertIn('--read-only', docker)
                        self.assertIn('10001:10001', docker)
                        self.assertNotIn('DATABASE_URL', ' '.join(docker))
