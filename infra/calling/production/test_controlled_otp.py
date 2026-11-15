"""N0-8: acceptance tooling writes controlled codes in exactly the Meeting Service format and
still reads plaintext codes from the previous image."""
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
        self.assertEqual(c.hashed_key('Ada@Example.com'), 'otp:verify:h:' + VECTOR_DIGEST)
        self.assertEqual(c.legacy_key('Ada@Example.com'), 'otp:verify:ada@example.com')
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
            code = eo.otp_from_redis(None, {}, address)
        self.assertRegex(code, r'^\d{6}$')
        stored = redis.data[c.hashed_key(address)]
        salt, digest = stored.split(':')
        self.assertEqual(c.sealed(code, bytes.fromhex(salt)), stored, 'a valid salted hash of the code')
        self.assertNotIn(code, stored)
        self.assertFalse(any(cmd.startswith('GET') for cmd, _ in redis.commands), 'nothing read back')

    def test_the_previous_image_plaintext_code_is_still_read(self):
        address = 'cfmeetotp@example.invalid'
        redis = FakeRedis({c.legacy_key(address): '482913'})
        with patch.object(eo, 'redis_command', redis):
            self.assertEqual(eo.otp_from_redis(None, {}, address), '482913')
        self.assertNotIn(c.hashed_key(address), redis.data, 'never writes the new key for the old image')

    def test_no_issued_code_is_reported_as_missing(self):
        with patch.object(eo, 'redis_command', FakeRedis({})):
            self.assertIsNone(eo.otp_from_redis(None, {}, 'nobody@example.invalid'))


class AppAcceptance(unittest.TestCase):
    def test_reader_input_carries_both_keys_and_a_sealed_controlled_code(self):
        x = app_acceptance.controlled_input('redis://r:6379/0', 'cfmeet_accept_a_1@example.invalid')
        self.assertEqual(x['hashed'], c.hashed_key('cfmeet_accept_a_1@example.invalid'))
        self.assertEqual(x['legacy'], c.legacy_key('cfmeet_accept_a_1@example.invalid'))
        salt = bytes.fromhex(x['sealed'].split(':')[0])
        self.assertEqual(c.sealed(x['code'], salt), x['sealed'])


if __name__ == '__main__':
    unittest.main()
