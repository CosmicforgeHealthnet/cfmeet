"""P16: service-auth key rotation (publish, switch, retire) in the release tooling. Nothing here
touches a real key, container or production file: the VPS operations are patched."""

import json
import unittest
from datetime import datetime, timedelta, timezone
from unittest.mock import patch

import coordinated_release as cr
import key_rotation as kr
from test_coordinated_release import REPO, isolated_work

KEY_OLD = {'kty': 'OKP', 'crv': 'Ed25519', 'kid': 'calling-production-1', 'x': 'A' * 43}
KEY_NEW = {'kty': 'OKP', 'crv': 'Ed25519', 'kid': 'calling-production-2', 'x': 'B' * 43}


def bundle(receiver, *issuers):
    return {'bundle_version': 1, 'receiver': receiver, 'runtime_environment': 'production',
            'trusted_issuers': [{'issuer': i, 'audience': receiver, 'runtime_environment': 'production',
                                 'keys': [dict(KEY_OLD)], 'permissions': ['x.read']} for i in issuers]}


class Helpers(unittest.TestCase):
    def test_the_next_kid_is_derived_and_odd_ids_are_refused(self):
        self.assertEqual(kr.next_kid('calling-production-1'), 'calling-production-2')
        self.assertEqual(kr.next_kid('meeting-service-production-9'), 'meeting-service-production-10')
        for bad in ('', 'calling-1', 'calling-staging-1', 'Calling-production-1'):
            with self.assertRaises(RuntimeError):
                kr.next_kid(bad)

    def test_the_service_comes_from_the_confirmation_only(self):
        self.assertEqual(kr.service_from(cr, 'service=calling'), 'calling')
        self.assertEqual(kr.service_from(cr, 'x,service=meeting-service'), 'meeting-service')
        for bad in ('', 'calling', 'service=unknown', 'service=calling,service=messaging'):
            with self.assertRaises(RuntimeError):
                kr.service_from(cr, bad)

    def test_publish_adds_and_retire_removes_only_that_issuers_key(self):
        b = bundle('cfmeet.privacy-relay', 'cfmeet.calling', 'cfmeet.telephony')
        added = kr.add_key(b, 'cfmeet.calling', KEY_NEW)
        calling = [e for e in added['trusted_issuers'] if e['issuer'] == 'cfmeet.calling'][0]
        telephony = [e for e in added['trusted_issuers'] if e['issuer'] == 'cfmeet.telephony'][0]
        self.assertEqual([k['kid'] for k in calling['keys']], ['calling-production-1', 'calling-production-2'])
        self.assertEqual(calling['permissions'], ['x.read'])
        self.assertEqual([k['kid'] for k in telephony['keys']], ['calling-production-1'])
        self.assertEqual(kr.add_key(added, 'cfmeet.calling', KEY_NEW), added, 'idempotent')
        self.assertIsNone(kr.add_key(b, 'cfmeet.messaging', KEY_NEW), 'not a receiver of that issuer')
        self.assertEqual(len(b['trusted_issuers'][0]['keys']), 1, 'the input is never modified')
        retired = kr.remove_key(added, 'cfmeet.calling', 'calling-production-1')
        calling = [e for e in retired['trusted_issuers'] if e['issuer'] == 'cfmeet.calling'][0]
        self.assertEqual([k['kid'] for k in calling['keys']], ['calling-production-2'])
        with self.assertRaisesRegex(RuntimeError, 'last trusted key'):
            kr.remove_key(retired, 'cfmeet.calling', 'calling-production-2')

    def test_operations_are_wired_and_listed_in_the_workflow(self):
        for op in kr.OPERATIONS:
            self.assertIn(op, cr.ACTIVATIONS)
            self.assertIn(op, cr.OPERATIONS)
        workflow = (REPO / '.github/workflows/cfmeet-coordinated-release.yml').read_text()
        for op in kr.OPERATIONS:
            self.assertIn(op, workflow)
        self.assertIn('^[a-z0-9,=-]{0,120}$', workflow)
        self.assertRegex('service=notifications-2', r'^[a-z0-9,=-]{0,120}$')


class Steps(unittest.TestCase):
    """The three steps against a patched VPS: receivers, containers and journal."""

    def run_steps(self, work, retire_at=None, fail_receiver=None):
        bundles = {'privacy-relay': bundle('cfmeet.privacy-relay', 'cfmeet.calling'),
                   'messaging': bundle('cfmeet.messaging', 'cfmeet.api-gateway')}
        recreated, env = [], {'CF_SERVICE_AUTH_KEY_ID': 'calling-production-1',
                              'CF_SERVICE_AUTH_PRIVATE_KEY_FILE': kr.KEY_TARGET}
        written = {}

        def recreate(name, overrides, bundle_, run_id, entry):
            if name == fail_receiver:
                entry['components'].append({'service': name})
                raise RuntimeError('health check failed')
            recreated.append((name, overrides, bundle_))
            entry['components'].append({'service': name})
            if name == 'calling':
                env.update(overrides)
            else:
                bundles[name] = json.loads(written[bundle_[1]])

        def write_bundle(name, new, run_id, like):
            path = '%s.%s.json' % (name, run_id)
            written[path] = json.dumps(new)
            return path

        restored = []
        secrets = work / 'secrets'
        secrets.mkdir()
        with patch.object(kr, 'receivers', lambda cr_: {n: ('/run/secrets/trust.json', '/src/' + n, b)
                                                         for n, b in bundles.items()}), \
                patch.object(cr, 'write_bundle', write_bundle), patch.object(cr, 'recreate', recreate), \
                patch.object(cr, 'restore', lambda c, r: restored.append(c['service'])), \
                patch.object(cr, 'require_release', lambda sha: sha), \
                patch.object(kr, 'stat_of', lambda cr_, path: None), patch.object(cr.os, 'chown', lambda *a: None), \
                patch.object(kr, 'current_identity', lambda cr_, n: env['CF_SERVICE_AUTH_KEY_ID']), \
                patch.object(kr.p, 'SECRETS', secrets), patch.object(kr.p, 'run', lambda *a: (secrets / 'calling-service-auth.calling-production-2.pem').write_text('k')), \
                patch.object(kr.p, 'regression', lambda: None), \
                patch('communications_release.public_key', lambda path, kid: dict(KEY_NEW, kid=kid)):
            errors = []
            for step, call in (('publish', lambda: kr.publish(cr, 'a' * 40, '1-1', 'service=calling')),
                               ('switch', lambda: kr.switch(cr, 'a' * 40, '2-1', 'service=calling')),
                               ('retire', lambda: kr.retire(cr, 'a' * 40, '3-1', 'service=calling',
                                                            now=retire_at))):
                try:
                    call()
                except RuntimeError as e:
                    errors.append((step, str(e)))
                    break
        journal = json.loads((work / 'journal.json').read_text())
        return errors, bundles, env, recreated, restored, journal

    def test_publish_switch_retire_overlap_the_keys_and_journal_each_step(self):
        later = datetime.now(timezone.utc) + timedelta(hours=1)
        with isolated_work() as work:
            errors, bundles, env, recreated, restored, journal = self.run_steps(work, retire_at=later)
        self.assertEqual(errors, [])
        # Only the receiver that trusts Calling changed; Calling switched to the new kid.
        self.assertEqual([n for n, _, _ in recreated], ['privacy-relay', 'calling', 'privacy-relay'])
        self.assertEqual(recreated[1][1], {'CF_SERVICE_AUTH_KEY_ID': 'calling-production-2'})
        self.assertEqual(recreated[1][2][0], kr.KEY_TARGET)
        self.assertTrue(recreated[1][2][1].endswith('calling-service-auth.calling-production-2.pem'))
        keys = bundles['privacy-relay']['trusted_issuers'][0]['keys']
        self.assertEqual([k['kid'] for k in keys], ['calling-production-2'])
        self.assertEqual([k['kid'] for k in bundles['messaging']['trusted_issuers'][0]['keys']],
                         ['calling-production-1'], 'other issuers untouched')
        self.assertEqual(env['CF_SERVICE_AUTH_KEY_ID'], 'calling-production-2')
        self.assertEqual([(e['operation'], e['status']) for e in journal],
                         [(op, 'done') for op in kr.OPERATIONS])
        self.assertEqual(journal[0]['keyRotation']['newKid'], 'calling-production-2')
        # Never key material in the journal (no public x, no private key).
        self.assertNotIn('B' * 43, json.dumps(journal))
        self.assertNotIn('PRIVATE KEY', json.dumps(journal))
        self.assertEqual(restored, [])

    def test_retire_waits_until_old_tokens_have_expired(self):
        with isolated_work() as work:
            errors, bundles, env, recreated, _, journal = self.run_steps(work, retire_at=datetime.now(timezone.utc))
        self.assertEqual(errors[0][0], 'retire')
        self.assertRegex(errors[0][1], 'have expired')
        keys = bundles['privacy-relay']['trusted_issuers'][0]['keys']
        self.assertEqual([k['kid'] for k in keys], ['calling-production-1', 'calling-production-2'])

    def test_a_failed_receiver_restores_the_step_and_switch_requires_publish(self):
        with isolated_work() as work:
            errors, bundles, env, recreated, restored, journal = self.run_steps(work, fail_receiver='privacy-relay')
        self.assertEqual(errors[0], ('publish', 'health check failed'))
        self.assertEqual(restored, ['privacy-relay'])
        self.assertEqual(journal[-1]['status'], 'failed')
        self.assertEqual(env['CF_SERVICE_AUTH_KEY_ID'], 'calling-production-1')
        with isolated_work() as work, patch.object(cr, 'require_release', lambda sha: sha):
            with self.assertRaisesRegex(RuntimeError, 'publish for calling first'):
                kr.switch(cr, 'a' * 40, '2-1', 'service=calling')
            with self.assertRaisesRegex(RuntimeError, 'switch for calling first'):
                kr.retire(cr, 'a' * 40, '3-1', 'service=calling')

    def test_rollback_restores_the_containers_of_the_last_rotation_step(self):
        src = (REPO / 'infra/calling/production/coordinated_release.py').read_text()
        # Each step records its recreated containers as components, which `rollback` restores.
        self.assertIn("e['components'] or", src)
        self.assertIn('key_rotation.publish', src)


if __name__ == '__main__':
    unittest.main()
