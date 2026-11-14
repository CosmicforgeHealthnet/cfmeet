import base64
import json
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch

import meeting_acceptance as ma
import meeting_acceptance_client as client


def jwt(claims):
    enc = lambda v: base64.urlsafe_b64encode(json.dumps(v).encode()).decode().rstrip('=')
    return enc({'alg': 'HS256'}) + '.' + enc(claims) + '.sig'


PID = '7d6f3c0e-4a8e-4c8b-9a51-6c2f0f1d2b3a'


class Context(unittest.TestCase):
    def test_client_gets_only_base_run_and_one_account(self):
        accounts = [{'username': 'u', 'email': 'a@example.invalid', 'password': 'pw', 'userId': 'x'},
                    {'email': 'b@example.invalid', 'password': 'pw2'}]
        value = ma.context('123', accounts)
        self.assertEqual(value, {'base': 'https://api.cfmeet.com', 'runId': '123',
                                 'account': {'email': 'a@example.invalid', 'password': 'pw'}})

    def test_printed_report_never_carries_secrets(self):
        report = {'runId': '1', 'checks': {'a': 'PASS'},
                  'details': {'conferenceId': 'c', 'credential': 'eyJ', 'password': 'pw', 'token': 't'}}
        printed = json.dumps(ma.public_report(report))
        for secret in ('eyJ', '"pw"', '"t"', 'password', 'token'):
            self.assertNotIn(secret, printed)
        self.assertEqual(ma.public_report(report)['verdict'], 'PASS')

    def test_verdict_requires_every_check(self):
        self.assertEqual(ma.verdict({'checks': {}}), 'FAIL')
        self.assertEqual(ma.verdict({'checks': {'a': 'PASS', 'b': 'FAIL'}}), 'FAIL')

    def test_never_registers_accounts_and_validates_run_id(self):
        with tempfile.TemporaryDirectory() as folder, patch.object(ma.p, 'SECRETS', Path(folder)), \
                patch.object(ma.p, 'run') as run:
            with self.assertRaises(RuntimeError):
                ma.run('42')
            run.assert_not_called()
        for bad in ('../x', '1;rm', ''):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                ma.run(bad)


class ClientIdentity(unittest.TestCase):
    def claims(self, **extra):
        value = {'sub': 'ps_abc', 'name': 'Alex', 'metadata': json.dumps({'participantId': PID}),
                 'attributes': {'participantId': PID}, 'video': {'roomJoin': True}}
        value.update(extra)
        return value

    def test_reads_participant_id_from_metadata_and_attributes(self):
        self.assertEqual(client.token_participant_id(jwt(self.claims())), PID)

    def test_rejects_extra_disagreeing_or_writable_identity(self):
        for bad in (self.claims(metadata=json.dumps({'participantId': PID, 'email': 'x@y'})),
                    self.claims(attributes={'participantId': 'other'}),
                    self.claims(attributes={}),
                    self.claims(video={'canUpdateOwnMetadata': True})):
            with self.subTest(bad=bad), self.assertRaises(client.Failed):
                client.token_participant_id(jwt(bad))

    def test_remote_participant_attribute_then_metadata(self):
        self.assertEqual(client.remote_participant_id(
            types.SimpleNamespace(attributes={'participantId': PID}, metadata='')), PID)
        self.assertEqual(client.remote_participant_id(
            types.SimpleNamespace(attributes={}, metadata=json.dumps({'participantId': PID}))), PID)
        self.assertIsNone(client.remote_participant_id(types.SimpleNamespace(attributes={}, metadata='')))


if __name__ == '__main__':
    unittest.main()
