"""Offline tests of the production web journey fixtures (web_journey.py)."""
import json
import unittest
from unittest.mock import patch

import web_journey as wj


class WebJourneyFixtures(unittest.TestCase):
    def test_the_accounts_file_name_is_bound_to_a_numeric_run_id(self):
        self.assertEqual(wj.accounts_file('123').name, 'cfmeet-web-journey-123.json')
        for bad in ('', '../x', '12a', '1' * 21):
            with self.assertRaises(AssertionError):
                wj.accounts_file(bad)

    def test_prepare_connects_a_with_b_and_c_and_records_labels_without_printing_secrets(self):
        people = [dict(email='%s@example.invalid' % x, password='Aa1!secret-' + x) for x in 'abc']
        cf = {'tok-a': 'AAAA-1111', 'tok-b': 'BBBB-2222', 'tok-c': 'CCCC-3333'}
        calls, written = [], {}

        def fake_request(path, token, method='GET', body=None, key=None):
            calls.append((method, path, token))
            if path.endswith('/me'):
                return {'cfMeetId': cf[token]}
            if path.endswith('/contact-requests'):
                return {'requestId': 'r-' + body['cfMeetId'], 'version': 1}
            if path.endswith('/contacts'):
                others = [v for k, v in cf.items() if k != token]
                return {'data': [{'cfMeetId': o, 'displayName': 'Name ' + o, 'relationship': 'CONNECTED'} for o in others]}
            return {}

        import app_acceptance
        with patch.object(app_acceptance, 'accounts', return_value=people), \
                patch.object(app_acceptance, 'api', side_effect=lambda path, body: {'access_token': 'tok-' + body['email'][0]}), \
                patch.object(wj, 'request', side_effect=fake_request), \
                patch.object(wj.p, 'write_private', side_effect=lambda path, text: written.update(path=path, text=text)), \
                patch('builtins.print') as printed:
            wj.prepare('42')
        fixtures = json.loads(written['text'])
        self.assertEqual([f['cfMeetId'] for f in fixtures], ['AAAA-1111', 'BBBB-2222', 'CCCC-3333'])
        self.assertEqual(fixtures[0]['labels']['BBBB-2222'], 'Name BBBB-2222')
        self.assertEqual(set(fixtures[0]), {'email', 'password', 'cfMeetId', 'labels'}, 'no token kept')
        requests = [c for c in calls if c[1].endswith('/contact-requests')]
        self.assertEqual([c[2] for c in requests], ['tok-a', 'tok-a'], 'A asks B and C')
        accepts = [c for c in calls if c[1].endswith('/accept')]
        self.assertEqual(sorted(c[2] for c in accepts), ['tok-b', 'tok-c'])
        self.assertEqual(len([c for c in calls if c[1] == '/api/v1/auth/logout']), 3, 'setup sign-ins ended')
        out = ' '.join(str(c) for c in printed.call_args_list)
        self.assertNotIn('secret', out)
        self.assertNotIn('tok-', out)


if __name__ == '__main__':
    unittest.main()
