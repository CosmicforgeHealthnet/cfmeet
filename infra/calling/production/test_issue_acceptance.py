import json
import struct
import tempfile
import unittest
import zlib
from pathlib import Path
from unittest.mock import patch

import issue_acceptance as ia
import issue_acceptance_client as client

A = '7d6f3c0e-4a8e-4c8b-9a51-6c2f0f1d2b3a'
B = '0b7f2a51-9e0c-4d3c-8f6e-2b1a3c4d5e6f'


class Context(unittest.TestCase):
    def test_client_gets_urls_accounts_and_the_restricted_credential_only(self):
        accounts = [{'username': 'u', 'email': 'a@example.invalid', 'password': 'pw', 'userId': 'x'},
                    {'email': 'b@example.invalid', 'password': 'pw2'}]
        value = ia.context('42', accounts, 'cfk_secret', [A, B])
        self.assertEqual(value, {'base': 'https://api.cfmeet.com', 'messaging': 'https://api.cfmeet.com:8444',
                                 'runId': '42', 'credential': 'cfk_secret', 'principals': [A, B],
                                 'accounts': [{'email': 'a@example.invalid', 'password': 'pw'},
                                              {'email': 'b@example.invalid', 'password': 'pw2'}]})

    def test_scopes_are_personal_calling_and_messaging_only(self):
        self.assertFalse([s for s in ia.SCOPES if s.startswith(('enterprise', 'telephony', 'numbering', 'sms'))])
        self.assertNotIn('policy.write', ia.SCOPES)


class Reporting(unittest.TestCase):
    def test_verdict(self):
        self.assertEqual(ia.verdict({'checks': {}}), 'FAIL')
        self.assertEqual(ia.verdict({'checks': {'a': 'PASS'}}), 'PASS')
        self.assertEqual(ia.verdict({'checks': {'a': 'PASS', 'b': 'NOT_RUN'}}), 'PARTIAL')
        self.assertEqual(ia.verdict({'checks': {'a': 'NOT_RUN', 'b': 'FAIL'}}), 'FAIL')

    def test_printed_report_never_carries_secrets_or_unknown_details(self):
        report = {'runId': '1', 'checks': {'a': 'PASS'},
                  'details': {'c1CallId': 'c', 'credential': 'cfk_secret', 'password': 'pw', 'session': 't'}}
        printed = json.dumps(ia.public_report(report))
        for secret in ('cfk_secret', 'pw', '"t"'):
            self.assertNotIn(secret, printed)
        self.assertIn('c1CallId', printed)

    def test_only_admission_counters_are_kept_from_metrics(self):
        text = ('# HELP x\ncalling_media_admission_retries_total{reason="database"} 2\n'
                'calling_http_requests_total{route="/v1/calls"} 9\n'
                'calling_media_admission_unavailable_total{reason="communications_policy"} 0\n')
        self.assertEqual(ia.admission_metrics(text), [
            'calling_media_admission_retries_total{reason="database"} 2',
            'calling_media_admission_unavailable_total{reason="communications_policy"} 0'])


class Refusals(unittest.TestCase):
    def test_run_id_must_be_numeric(self):
        with self.assertRaises(ValueError):
            ia.run('1;rm -rf /')

    def test_never_registers_accounts(self):
        with tempfile.TemporaryDirectory() as folder, patch.object(ia.p, 'SECRETS', Path(folder)):
            with self.assertRaisesRegex(RuntimeError, 'refusing to register'):
                ia.run('7')

    def test_needs_two_accounts(self):
        with tempfile.TemporaryDirectory() as folder, patch.object(ia.p, 'SECRETS', Path(folder)):
            (Path(folder) / ia.ACCOUNTS).write_text(json.dumps([{'email': 'a', 'password': 'b'}]))
            with self.assertRaisesRegex(RuntimeError, 'two controlled'):
                ia.run('7')

    def test_restriction_failure_revokes_the_new_credential(self):
        calls = []

        def admin(*args):
            calls.append(args[:2])
            if args[:2] == ('credential', 'create'):
                return {'credential': {'id': 'cred-1'}, 'secret': 's'}
            if args[:2] == ('credential', 'restrict-principals'):
                raise RuntimeError('refused')
            return {}
        fake_api = type('M', (), {'admin': staticmethod(admin),
                                  'sql': staticmethod(lambda q: json.dumps({'tenant': 't', 'application': 'a', 'account': 's'}))})
        fake_release = type('R', (), {'prod_scope': staticmethod(lambda: {'serviceAccountId': A})})
        with patch.dict('sys.modules', {'api_acceptance': fake_api, 'communications_release': fake_release}):
            with self.assertRaises(RuntimeError):
                ia.issue_credential([A, B])
        self.assertEqual(calls, [('credential', 'create'), ('credential', 'restrict-principals'),
                                 ('credential', 'revoke')])


class ClientHelpers(unittest.TestCase):
    def test_png_is_a_valid_image(self):
        data = client.png(4, 3)
        self.assertTrue(data.startswith(b'\x89PNG\r\n\x1a\n'))
        width, height = struct.unpack('>II', data[16:24])
        self.assertEqual((width, height), (4, 3))
        idat = data.index(b'IDAT')
        size = struct.unpack('>I', data[idat - 4:idat])[0]
        self.assertEqual(len(zlib.decompress(data[idat + 4:idat + 4 + size])), 3 * (1 + 4 * 3))

    def test_outcome_reads_calling_and_hub_history(self):
        self.assertEqual(client.outcome_of({'participation': {'outcome': 'MISSED'}}), 'MISSED')
        self.assertEqual(client.outcome_of({'outcome': 'RINGING'}), 'RINGING')
        self.assertIsNone(client.outcome_of({}))

    def test_a_failed_group_is_recorded_without_hiding_the_rest(self):
        journey = client.Journey({'runId': '1'})

        def boom():
            raise client.Failed('POST /v1/calls answered 409 STALE_CALL_VERSION')
        journey.group_of('C1', boom)
        journey.group_of('C3', lambda: journey.check('C3 ok', True))
        self.assertEqual(journey.checks, {'C1': 'FAIL', 'C3 ok': 'PASS'})

    def test_missing_managed_groups_are_reported_not_run(self):
        journey = client.Journey({'runId': '1', 'messaging': 'https://m', 'credential': 'c'})
        with patch.object(client, 'http', return_value=(422, {'error': {'code': 'MANAGED_NOT_ALLOWED'}})):
            journey.m_group({'principalId': A}, {'principalId': B}, None, 'contact')
        self.assertEqual(set(journey.checks.values()), {'NOT_RUN'})
        self.assertEqual(len(journey.checks), 3)


if __name__ == '__main__':
    unittest.main()
