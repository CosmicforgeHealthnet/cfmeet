"""Fail-closed bounds and secrecy of the N0 controlled production auth probe."""
import unittest
import uuid
from unittest.mock import patch

import notification_auth_acceptance as a


class AuthProbeSafety(unittest.TestCase):
    def test_only_request_correlation_id_is_ignored_when_comparing_denials(self):
        first = {'success': False, 'error': {'code': 'BAD_REQUEST', 'message': 'Invalid or expired reset code', 'requestId': 'a'}}
        second = {'success': False, 'error': dict(first['error'], requestId='b')}
        self.assertEqual(a.denial_shape(first), a.denial_shape(second))
        second['error']['message'] = 'Account missing'
        self.assertNotEqual(a.denial_shape(first), a.denial_shape(second))
        with self.assertRaises(AssertionError):
            a.denial_shape({})

    def fixture(self):
        account = {'username': 'cfmeet_accept_a_1234abcd',
                   'email': 'cfmeet_accept_a_1234abcd@example.invalid',
                   'userId': str(uuid.uuid4()), 'password': 'fixture-password'}
        return account, {'user_id': account['userId']}

    def test_real_or_mismatched_accounts_are_refused_before_any_network_or_redis(self):
        for changed in ({'email': 'real@example.com'}, {'username': 'real'}, {'userId': str(uuid.uuid4())}):
            account, registration = self.fixture()
            account.update(changed)
            with patch.object(a.mail, 'public') as http, patch.object(a.mail, 'redis_command') as redis:
                with self.assertRaises(AssertionError):
                    a.run_pending(account, registration, 'redis://fixture')
                http.assert_not_called()
                redis.assert_not_called()

    def test_no_plaintext_or_missing_issued_otp_can_be_treated_as_success(self):
        for stored in ('123456', '', '00:00'):
            with patch.object(a.mail, 'redis_command', return_value=stored):
                with self.assertRaises(AssertionError):
                    a.controlled(None, {}, 'cfmeet_accept_a_1234abcd@example.invalid', 'otp:reset')

    def test_controlled_replacement_preserves_ttl_and_cannot_create_an_unissued_code(self):
        address = 'cfmeet_accept_a_1234abcd@example.invalid'
        commands = []

        def redis(cr, env, command, values):
            commands.append((command, values))
            if command.startswith('GET'):
                return a.otp.sealed('654321')
            if command.startswith('EXISTS'):
                return '0'
            return 'OK' if 'XX KEEPTTL' in command else None

        with patch.object(a.mail, 'redis_command', redis):
            code = a.controlled(None, {}, address, 'otp:reset')
        self.assertRegex(code, r'^\d{6}$')
        command, values = commands[-1]
        self.assertIn('XX KEEPTTL', command)
        self.assertEqual(values['KEY'], a.otp.hashed_key(address, 'otp:reset'))
        salt = bytes.fromhex(values['VALUE'].split(':')[0])
        self.assertEqual(values['VALUE'], a.otp.sealed(code, salt))

    def test_unexpected_http_status_is_a_failure_and_never_exposes_response_secrets(self):
        secret = 'private-token-and-address@example.invalid'
        with patch.object(a.mail, 'public', return_value=(500, {'access_token': secret})):
            with self.assertRaises(AssertionError) as error:
                a.request('POST', '/api/v1/auth/login', {})
        self.assertNotIn(secret, str(error.exception))
        self.assertIn('500', str(error.exception))


if __name__ == '__main__':
    unittest.main()
