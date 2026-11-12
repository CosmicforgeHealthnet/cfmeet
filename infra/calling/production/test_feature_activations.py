"""Offline tests: developer console, customer webhooks, gateway families, safe defaults,
the off-host backup record (activation steps added for the all-features activation)."""
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import coordinated_release as cr
import offsite_backup as ob
from test_coordinated_release import REPO, isolated_work

MANIFEST = json.loads((REPO / 'infra/api-gateway/route-manifest.json').read_text())
EXCLUDED = ('/v1/numbers', '/v1/sms', '/v1/privacy-relay')
DIRECT = ('/v1/calls', '/v1/calling', '/v1/enterprise', '/v1/realtime')


def journal(work, *operations):
    (work / 'journal.json').write_text(json.dumps(
        [dict(id='%s:%d' % (o, i), operation=o, sha='a' * 40, status='done', components=[]) for i, o in enumerate(operations)]))


class SafeDefaults(unittest.TestCase):
    def test_active_webhook_delivery_only_after_its_activation_and_on_the_worker_too(self):
        envs = {'developer-platform': {'DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE': 'active'},
                'developer-platform-worker': {'DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE': 'active'}}
        with isolated_work() as work:
            self.assertEqual(sorted(cr.safe_default_violations(envs)),
                             ['developer-platform DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE',
                              'developer-platform-worker DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE'])
            journal(work, 'activate-developer-webhooks')
            self.assertEqual(cr.safe_default_violations(envs), [])

    def test_auto_approval_and_providers_stay_fixed_after_every_activation(self):
        with isolated_work() as work:
            journal(work, *cr.ACTIVATIONS)
            envs = {'developer-platform-worker': {'DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL': 'true'},
                    'messaging': {'MESSAGING_E2EE_DEVICES_ENABLED': 'true', 'MESSAGING_PUSH_PROVIDER': 'fcm'},
                    'sms-gateway': {'SMS_GATEWAY_PROVIDER': 'africastalking'},
                    'telephony': {'TELEPHONY_ENABLED': 'true'}, 'privacy-relay': {'PRIVACY_RELAY_ENABLED': 'true'},
                    'numbering': {'NUMBERING_PROVIDERS': 'x'}}
            self.assertEqual(len(cr.safe_default_violations(envs)), 7)


    def test_push_adapter_credentials_are_refused_until_push_is_approved(self):
        with isolated_work():
            envs = {'messaging': {'MESSAGING_PUSH_PROVIDER': 'none', 'MESSAGING_PUSH_WEB_VAPID_KEY_FILE': '/k',
                                  'MESSAGING_PUSH_FCM_SERVICE_ACCOUNT_FILE': '/f', 'MESSAGING_PUSH_APNS_TOPIC': ''}}
            self.assertEqual(cr.safe_default_violations(envs),
                             ['messaging MESSAGING_PUSH_FCM_SERVICE_ACCOUNT_FILE',
                              'messaging MESSAGING_PUSH_WEB_VAPID_KEY_FILE'])


class GatewayFamilies(unittest.TestCase):
    def test_every_family_is_a_routed_public_customer_rest_family_of_calling_messaging_or_developer_platform(self):
        upstreams = {'cfmeet.calling', 'cfmeet.messaging', 'cfmeet.developer-platform'}
        for prefix in cr.FAMILY_PREFIXES:
            routes = [r for r in MANIFEST['routes'] if r['pattern'] == prefix or r['pattern'].startswith(prefix + '/')]
            self.assertTrue(routes, prefix)
            for r in routes:
                self.assertEqual(r['protocol'], 'REST', r['id'])
                self.assertIn(r['upstream'], upstreams, r['id'])

    def test_families_never_cover_excluded_internal_or_already_direct_routes(self):
        for prefix in cr.FAMILY_PREFIXES:
            for other in EXCLUDED + DIRECT:
                self.assertFalse(prefix == other or prefix.startswith(other + '/') or other.startswith(prefix + '/'))
            for word in ('internal', 'admin', 'operator', 'callback', 'metrics'):
                self.assertNotIn(word, prefix)
        self.assertFalse(set(cr.FAMILY_PREFIXES) & set(cr.GATEWAY_PREFIXES))
        self.assertEqual(set(cr.DISABLED_DOMAINS.split(',')), {'numbering', 'sms', 'privacy-relay'})

    def test_every_family_has_a_probe_and_the_snippet_routes_both_sets(self):
        probed = {next(p for p in cr.FAMILY_PREFIXES if path == p or path.startswith(p + '/')) for _, path in cr.FAMILY_PROBES}
        self.assertEqual(probed, set(cr.FAMILY_PREFIXES))
        text = cr.gateway_snippet(cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES)
        for prefix in cr.GATEWAY_PREFIXES + cr.FAMILY_PREFIXES:
            self.assertIn('location ^~ %s/ {' % prefix, text)
        self.assertFalse([l for l in text.splitlines() if l.startswith('location') and '/internal' in l])
        self.assertEqual(cr.gateway_snippet(), cr.gateway_snippet(cr.GATEWAY_PREFIXES))

    def test_existing_routes_for_a_family_are_refused(self):
        site = 'location ^~ /v1/conversations/ { proxy_pass http://x; }\nlocation ~ ^/v1/calls(?:/|$) {}'
        self.assertEqual(cr.route_collisions([site], cr.FAMILY_PREFIXES), ['/v1/conversations'])

    def test_gateway_families_require_webhooks_and_routing(self):
        self.assertEqual(set(cr.ACTIVATIONS['activate-gateway-families'][0]),
                         {'activate-gateway-routing', 'activate-developer-webhooks'})
        self.assertEqual(cr.ACTIVATIONS['activate-developer-webhooks'][0], ('activate-developer-console',))


class DeveloperWebhooks(unittest.TestCase):
    def plan(self, calling_state='0,0', migration='0'):
        captured = {}
        sql = {'cfmeet_calling': calling_state, 'cfmeet_developer_platform': migration}
        with tempfile.TemporaryDirectory() as folder, \
                patch.object(cr.p, 'SECRETS', Path(folder)), \
                patch.object(cr, 'env_of', lambda n: {'DEVELOPER_PLATFORM_GATEWAY_API_ENABLED': 'true'}), \
                patch.object(cr, 'tls_route_present', lambda port: None), \
                patch.object(cr.release, 'db_sql', lambda db, q: sql[db]), \
                patch.object(cr, 'webhook_egress_network', lambda: cr.WEBHOOK_EGRESS_NET), \
                patch.object(cr.os, 'chown', lambda *a: None), \
                patch.object(cr, 'apply_activation', lambda op, sha, run, changes, **k: captured.update(op=op, changes=changes)):
            cr.activate_developer_webhooks('a' * 40, '1-1', '')
            key = (Path(folder) / cr.WEBHOOK_KEY)
            captured['key'] = key.read_text().strip()
            captured['mode'] = oct(key.stat().st_mode & 0o777)
        return captured

    def test_plan_signs_delivers_from_the_worker_only_and_retires_calling(self):
        out = self.plan()
        import base64
        self.assertEqual(len(base64.b64decode(out['key'])), 32)
        self.assertEqual(out['mode'], '0o400')
        changes = {c[0]: c for c in out['changes']}
        self.assertEqual(list(changes), ['developer-platform', 'developer-platform-worker', 'calling', 'messaging',
                                         'meeting-service'])
        for name in ('developer-platform', 'developer-platform-worker'):
            env = changes[name][1]
            self.assertEqual(env['DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE'], 'active')
            self.assertEqual(env['DEVELOPER_PLATFORM_WEBHOOK_SECRET_KEY_FILE'], cr.WEBHOOK_KEY_TARGET)
            self.assertNotIn('DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL', env)
            self.assertNotIn('DEVELOPER_PLATFORM_WEBHOOK_ALLOW_PRIVATE_ENDPOINTS', env)
        self.assertEqual(changes['developer-platform-worker'][2]['networks'], [cr.WEBHOOK_EGRESS_NET])
        self.assertNotIn('networks', changes['developer-platform'][2])
        self.assertEqual(changes['calling'][1]['CALLING_WEBHOOK_AUTHORITY'], 'retired')
        self.assertEqual(changes['calling'][1]['CALLING_WEBHOOK_DELIVERY_ENABLED'], 'false')
        for name in ('calling', 'messaging', 'meeting-service'):
            self.assertTrue(changes[name][1]['CF_DEVELOPER_PLATFORM_BASE_URL'].startswith('https://'))

    def test_calling_customer_state_or_a_started_migration_refuses(self):
        with self.assertRaisesRegex(RuntimeError, 'migration runbook'):
            self.plan(calling_state='2,0')
        with self.assertRaisesRegex(RuntimeError, 'migration runbook'):
            self.plan(calling_state='0,1')
        with self.assertRaisesRegex(RuntimeError, 'migration'):
            self.plan(migration='1')

    def test_recreate_adds_mounts_and_networks(self):
        c = {'Config': {'Env': ['A=1']}, 'Image': 'img', 'HostConfig': {'Mounts': []}, 'Id': 'x' * 64,
             'NetworkSettings': {'Networks': {'cfmeet-core-private': {'Aliases': ['w']}}}}
        seen = {}
        with patch.object(cr.p, 'inspect', lambda n: c), patch.object(cr.p, 'env_of', lambda c_: {'A': '1'}), \
                patch.object(cr, 'swap', lambda name, cfg, run, entry: seen.update(cfg=cfg) or 'component'), \
                patch.object(cr, 'checked', lambda *a: None):
            cr.recreate('developer-platform-worker', {'B': '2'}, None, '1-1', {},
                        {'mounts': [(Path('/k'), '/run/secrets/k')], 'networks': ['egress']})
        cfg = seen['cfg']
        self.assertIn({'Type': 'bind', 'Source': '/k', 'Target': '/run/secrets/k', 'ReadOnly': True},
                      cfg['HostConfig']['Mounts'])
        self.assertEqual(set(cfg['NetworkingConfig']['EndpointsConfig']), {'cfmeet-core-private', 'egress'})
        self.assertIn('B=2', cfg['Env'])


class OffsiteBackup(unittest.TestCase):
    RECIPIENT = 'age1' + 'q' * 58

    def test_recipients_refuse_private_keys_and_garbage(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'r.txt'
            path.write_text('# owner\n%s\n' % self.RECIPIENT)
            self.assertEqual(ob.recipients(path), [self.RECIPIENT])
            for bad in ('AGE-SECRET-KEY-1QQQ\n', '-----BEGIN PRIVATE KEY-----\n', 'ssh-ed25519 AAAA\n', ''):
                path.write_text(bad)
                with self.assertRaises(RuntimeError):
                    ob.recipients(path)

    def setup_run(self, work):
        folder = work / 'backups' / '7-1' / 'offsite'
        folder.mkdir(parents=True)
        files = []
        for label in ('meeting-service', 'organizations', 'platform-access'):
            f = folder / (label + '.dump.age')
            f.write_bytes(b'age-encryption.org/v1\n' + label.encode())
            files.append(dict(label=label, file=f.name, sha256=cr.sha256(f), histories={'h': 3}))
        (folder / 'offsite-manifest.json').write_text(json.dumps({'files': files}))
        return files

    def evidence(self, work, files, **over):
        doc = dict(runId='7-1', artifactId=99, runUrl='https://github.com/x/actions/runs/1', retentionDays=90,
                   offHostFiles=[{'file': f['file'], 'sha256': f['sha256']} for f in files],
                   drills=[{'label': f['label'], 'result': 'PASS', 'histories': {'h': 3}, 'tables': 5} for f in files])
        doc.update(over)
        path = work / 'evidence.json'
        path.write_text(json.dumps(doc))
        return path

    def test_record_requires_matching_hashes_and_passing_drills(self):
        with isolated_work() as work, patch.object(cr, 'BACKUPS', work / 'backups'):
            files = self.setup_run(work)
            self.assertIsNone(ob.verified('7-1'))
            bad_hash = [{'file': f['file'], 'sha256': '0' * 64} for f in files]
            failing = [{'label': f['label'], 'result': 'FAIL'} for f in files]
            drifted = [{'label': f['label'], 'result': 'PASS', 'histories': {'h': 2}} for f in files]
            for over in (dict(offHostFiles=bad_hash), dict(drills=failing), dict(drills=drifted),
                         dict(runId='8-1'), dict(artifactId=None)):
                with self.assertRaises(RuntimeError):
                    ob.record('7-1', self.evidence(work, files, **over))
            self.assertIsNone(ob.verified('7-1'))
            ob.record('7-1', self.evidence(work, files))
            doc = ob.verified('7-1')
            self.assertEqual(doc['status'], ob.OFFSITE_STATUS)
            self.assertEqual(doc['artifactId'], 99)

    def test_prune_touches_only_old_offsite_runs_beyond_the_newest_seven(self):
        import os
        with isolated_work() as work, patch.object(cr, 'BACKUPS', work / 'backups'):
            base = 1_000_000_000
            for i in range(10):
                d = work / 'backups' / ('%d-1' % i) / 'offsite'
                d.mkdir(parents=True)
                (d / 'offsite-manifest.json').write_text('{}')
                os.utime(d.parent, (base + i * 86400, base + i * 86400))
            release = work / 'backups' / 'release-1'
            release.mkdir()
            os.utime(release, (base, base))
            removed = ob.prune(now=base + 30 * 86400)
            self.assertEqual(sorted(removed), ['0-1', '1-1', '2-1'])
            self.assertTrue(release.exists())
            self.assertEqual(ob.prune(now=base + 30 * 86400), [])

    def test_import_gate_requires_the_offsite_record(self):
        with isolated_work() as work, patch.object(cr, 'BACKUPS', work / 'backups'):
            files = self.setup_run(work)
            with self.assertRaisesRegex(RuntimeError, 'off-host'):
                cr.require_offsite({'runId': '7-1'})
            ob.record('7-1', self.evidence(work, files))
            self.assertEqual(cr.require_offsite({'runId': '7-1'})['artifactId'], 99)


if __name__ == '__main__':
    unittest.main()
