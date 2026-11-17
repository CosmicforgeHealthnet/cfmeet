"""N0-8: acceptance tooling writes controlled codes in exactly the Meeting Service format, under
the passwordless sign-in and step-up keys, and only replaces a code that was really issued."""
import unittest
from unittest.mock import patch

import app_acceptance
import controlled_otp as c
import email_operations as eo

# Shared vector with services/meeting-service/src/services/otp.rs (tests module).
VECTOR_SEALED = '000102030405060708090a0b0c0d0e0f:99c90773ea9ce66a355bd6a2a6e55122c3caf5a164785c90b1d58c2d63bb8680'
VECTOR_DIGEST = 'b5fc85e55755f9e0d030a10ab4429b6b2944855f9a0d60077fe832becbc41d72'


class Format(unittest.TestCase):
    def test_same_format_and_keys_as_the_meeting_service(self):
        self.assertEqual(c.sealed('123456', bytes(range(16))), VECTOR_SEALED)
        self.assertEqual(c.address_digest(' Ada@Example.com '), VECTOR_DIGEST)
        self.assertEqual(c.hashed_key('Ada@Example.com'), 'otp:signin:h:' + VECTOR_DIGEST)
        user = '0b6f4f4e-8f2a-4c1e-9d0b-3e2f1a0c9b7d'
        self.assertEqual(c.step_up_key(user, 'DEACTIVATE_ACCOUNT'),
                         'otp:stepup:h:' + c.address_digest('user:%s:deactivate_account' % user))
        code = c.new_code()
        self.assertRegex(code, r'^\d{6}$')
        self.assertNotIn(code, c.sealed(code))
        self.assertNotEqual(c.sealed(code), c.sealed(code))


class FakeRedis:
    def __init__(self, data):
        self.data = dict(data)
        self.commands = []

    def __call__(self, cr, env, command, extra=None):
        extra = extra or {}
        self.commands.append((command, dict(extra)))
        key = extra.get('KEY')
        if command.startswith('EXISTS'):
            return '1\n' if key in self.data else '0\n'
        if command.startswith('SET'):
            if key not in self.data:  # XX
                return '\n'
            self.data[key] = extra['VALUE']
            return 'OK\n'
        if command.startswith('GET'):
            return (self.data.get(key) or '') + '\n'
        raise AssertionError(command)


class EmailAcceptance(unittest.TestCase):
    def test_a_hashed_issued_code_is_replaced_by_a_controlled_one(self):
        address = 'cfmeetotp@example.invalid'
        redis = FakeRedis({c.hashed_key(address): 'issued:hash'})
        with patch.object(eo, 'redis_command', redis):
            code = eo.otp_from_redis(None, {}, c.hashed_key(address))
        self.assertRegex(code, r'^\d{6}$')
        stored = redis.data[c.hashed_key(address)]
        salt, digest = stored.split(':')
        self.assertEqual(c.sealed(code, bytes.fromhex(salt)), stored, 'a valid salted hash of the code')
        self.assertNotIn(code, stored)
        self.assertFalse(any(cmd.startswith('GET') for cmd, _ in redis.commands), 'nothing read back')
        self.assertTrue(any('KEEPTTL' in cmd for cmd, _ in redis.commands), 'the issued TTL is kept')

    def test_no_issued_code_is_reported_as_missing_and_nothing_is_created(self):
        redis = FakeRedis({})
        with patch.object(eo, 'redis_command', redis):
            self.assertIsNone(eo.otp_from_redis(None, {}, c.hashed_key('nobody@example.invalid')))
        self.assertEqual(redis.data, {})


class AppAcceptance(unittest.TestCase):
    def test_helper_input_carries_the_key_and_a_sealed_controlled_code(self):
        key = c.hashed_key('cfmeet_accept_a_1@example.invalid')
        x = app_acceptance.controlled_input('redis://r:6379/0', key)
        self.assertEqual(x['hashed'], key)
        self.assertNotIn('legacy', x)
        salt = bytes.fromhex(x['sealed'].split(':')[0])
        self.assertEqual(c.sealed(x['code'], salt), x['sealed'])
        self.assertIn("'XX','KEEPTTL'", app_acceptance.REDIS_HELPER, 'only an issued code is replaced')

    def test_sign_in_requests_a_code_replaces_it_and_verifies(self):
        calls = []

        def api(path, body):
            calls.append((path, body))
            return {'access_token': 'a', 'refresh_token': 'r', 'is_new_account': True, 'user': {'id': 'u'}}

        account = {'email': 'cfmeet_accept_a_1@example.invalid'}
        with patch.object(app_acceptance, 'api', api), \
                patch.object(app_acceptance, 'controlled_code', return_value='012345') as controlled:
            self.assertEqual(app_acceptance.refresh_token_for(account, 'redis://r'), 'r')
        controlled.assert_called_once_with(c.hashed_key(account['email']), 'redis://r')
        self.assertEqual(calls, [('otp/request', {'email': account['email']}),
                                 ('otp/verify', {'email': account['email'], 'code': '012345'})])


if __name__ == '__main__':
    unittest.main()
