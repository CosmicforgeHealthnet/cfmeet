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
        accounts = [{'username': 'u', 'email': 'a@example.invalid', 'userId': 'x'},
                    {'email': 'b@example.invalid'}]
        value = ia.context('42', accounts, 'cfk_secret', [A, B], session=lambda a: 'rt-' + a['email'][0])
        self.assertEqual(value, {'base': 'https://api.cfmeet.com', 'messaging': 'https://api.cfmeet.com:8444',
                                 'runId': '42', 'credential': 'cfk_secret', 'principals': [A, B],
                                 'scopes': list(ia.SCOPES),
                                 'accounts': [{'email': 'a@example.invalid', 'refreshToken': 'rt-a'},
                                              {'email': 'b@example.invalid', 'refreshToken': 'rt-b'}]})

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

    def fake_platform(self, allowed, refuse=None):
        calls = []

        def admin(*args):
            calls.append(args)
            if args[:2] == refuse:
                raise ia.AdminRefused('refused')
            if args[:2] == ('credential', 'create'):
                return {'credential': {'id': 'cred-1'}, 'secret': 's'}
            return {}

        def sql(query):
            if 'platform_application_scopes' in query:
                return json.dumps(allowed)
            return json.dumps({'tenant': 't', 'application': 'a', 'account': 's'})
        fake_api = type('M', (), {'sql': staticmethod(sql)})
        fake_release = type('R', (), {'prod_scope': staticmethod(lambda: {'serviceAccountId': A})})
        return calls, admin, {'api_acceptance': fake_api, 'communications_release': fake_release}

    def test_restriction_failure_revokes_the_new_credential(self):
        calls, admin, modules = self.fake_platform(list(ia.SCOPES), refuse=('credential', 'restrict-principals'))
        with patch.dict('sys.modules', modules), patch.object(ia, 'admin', admin):
            with self.assertRaises(ia.AdminRefused):
                ia.issue_credential([A, B])
        self.assertEqual([c[:2] for c in calls], [('credential', 'create'), ('credential', 'restrict-principals'),
                                                  ('credential', 'revoke')])

    def test_only_scopes_the_application_is_allowed_are_requested_and_no_grant_is_added(self):
        calls, admin, modules = self.fake_platform(['calls.read', 'calls.write', 'messages.read', 'meetings.admin'])
        with patch.dict('sys.modules', modules), patch.object(ia, 'admin', admin):
            secret, credential_id, scopes = ia.issue_credential([A, B])
        self.assertEqual((secret, credential_id, scopes), ('s', 'cred-1', ['calls.read', 'calls.write', 'messages.read']))
        create = calls[0]
        requested = [create[i + 1] for i, v in enumerate(create) if v == '--scope']
        self.assertEqual(requested, ['calls.read', 'calls.write', 'messages.read'])
        self.assertFalse([c for c in calls if c[:2] == ('scope', 'grant')])

    def test_calling_scopes_are_required(self):
        with self.assertRaisesRegex(RuntimeError, 'calls.write'):
            ia.usable_scopes(['calls.read', 'messages.send'])

    def test_admin_refusals_are_reduced_to_a_fixed_category(self):
        self.assertEqual(ia.refusal_category('Error: scope `attachments.write` is not allowed for this application'),
                         'SCOPE_NOT_ALLOWED')
        self.assertEqual(ia.refusal_category('connection refused postgres://u:secret@db'), 'ADMIN_TOOL_FAILED')


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

    def test_screen_grant_is_read_from_the_effective_livekit_permission(self):
        class Client:
            def __init__(self, answers):
                self.answers = list(answers)

            def cmd(self, name, **_):
                assert name == 'sources'
                return {'ok': True, 'sources': self.answers.pop(0) if len(self.answers) > 1 else self.answers[0]}
        journey = client.Journey({'runId': '1'})
        self.assertTrue(client.Journey.screen_granted(Client([[1, 2, 3, 4]])))
        self.assertFalse(client.Journey.screen_granted(Client([[1, 2, 3]])))
        # The grant arrives after commit: waited for, then observed.
        with patch.object(client.time, 'sleep'):
            self.assertTrue(journey.await_screen(Client([[2], [2], [2, 3, 4]]), True))
            self.assertFalse(journey.await_screen(Client([[2, 3, 4], [2]]), False))

    def test_c7_without_a_delivered_offer_fails_and_stops(self):
        journey = client.Journey({'runId': '1'})
        view = {'version': 4, 'participants': [{'principalId': A, 'participantId': 'pa', 'endedAt': None}]}
        added = {'participants': [{'principalId': B, 'participantId': 'pc'}]}
        with patch.object(journey, 'view', return_value=view), patch.object(journey, 'scoped', return_value=added), \
                patch.object(journey, 'offer', return_value=None), patch.object(journey, 'signaling') as signaling:
            journey.c7('call', {'principalId': A}, {'principalId': B})
        self.assertEqual(journey.checks['C7 the added offer is delivered PENDING'], 'FAIL')
        signaling.assert_not_called()

    def test_checks_needing_a_scope_the_application_lacks_are_not_run(self):
        journey = client.Journey({'runId': '1', 'scopes': ['calls.read', 'calls.write', 'messages.read']})
        with patch.object(client, 'http') as http:
            journey.m_group({'principalId': A}, {'principalId': B}, None, 'contact')
        http.assert_not_called()
        self.assertEqual(set(journey.checks.values()), {'NOT_RUN'})
        self.assertIn('attachments.write', journey.details['notes']['M2 groups in common carry the group title'])

    def test_missing_managed_groups_are_reported_not_run(self):
        journey = client.Journey({'runId': '1', 'messaging': 'https://m', 'credential': 'c'})
        with patch.object(client, 'http', return_value=(422, {'error': {'code': 'MANAGED_NOT_ALLOWED'}})):
            journey.m_group({'principalId': A}, {'principalId': B}, None, 'contact')
        self.assertEqual(set(journey.checks.values()), {'NOT_RUN'})
        self.assertEqual(len(journey.checks), 3)


if __name__ == '__main__':
    unittest.main()
