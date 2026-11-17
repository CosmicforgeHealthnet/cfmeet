"""N0 production auth checks on a fresh, disposable acceptance fixture only (passwordless).

No provider activation or credential/configuration changes. Redis access is restricted to
the supplied fresh fixture's sign-in code key. Issued hashes are inspected in memory, never
logged; controlled inputs use SET XX and preserve the issued TTL. No user data or secrets
enter the receipt.
"""
import json
import re
import uuid

import controlled_otp as otp
import email_operations as mail


def require_fixture(account):
    assert re.fullmatch(r'cfmeet_accept_[abc]_[0-9a-f]{8}', account['username']), 'Not a fresh controlled fixture'
    assert account['email'] == account['username'] + '@example.invalid', 'Not a controlled fixture address'
    assert 'userId' not in account, 'Not a fresh fixture: the account already exists'


def request(method, path, body=None, token=None, expected=200):
    status, value = mail.public(method, path, body, token)
    assert status == expected, 'Auth acceptance: unexpected status for ' + path + ': ' + str(status)
    return value


def denial_shape(value):
    """Request IDs are deliberately unique and carry no account-existence information."""
    assert isinstance(value, dict) and isinstance(value.get('error'), dict), 'Missing error envelope'
    return dict(value, error={k: v for k, v in value['error'].items() if k != 'requestId'})


def redis(cr, env, command, values):
    out = mail.redis_command(cr, env, command, values)
    assert out is not None, 'Controlled fixture Redis operation failed'
    return out.strip().splitlines()[-1] if out.strip() else ''


def issued(cr, env, address, prefix=otp.SIGN_IN):
    key = otp.hashed_key(address, prefix)
    value = redis(cr, env, 'GET "$KEY"', {'KEY': key})
    # `<salt>:<digest>`, optionally followed by `:<link digest>` (the magic link).
    assert re.fullmatch(r'[0-9a-f]{32}:[0-9a-f]{64}(:[0-9a-f]{64})?', value), 'Issued OTP is not a salted hash'
    assert redis(cr, env, 'EXISTS "$KEY"', {'KEY': prefix + ':' + address.lower()}) == '0', \
        'An OTP key names the address'
    return value


def controlled(cr, env, address, prefix=otp.SIGN_IN):
    issued(cr, env, address, prefix)
    code = otp.new_code()
    assert redis(cr, env, 'SET "$KEY" "$VALUE" XX KEEPTTL',
                 {'KEY': otp.hashed_key(address, prefix), 'VALUE': otp.sealed(code)}) == 'OK', 'No issued OTP to replace'
    return code


def run_pending(account, redis_url):
    """Creates the fresh fixture through the checks; records its `userId`."""
    require_fixture(account)
    import coordinated_release as cr
    env = {'REDIS_URL': redis_url}
    address = account['email']
    unknown = 'cfmeet_unknown_' + uuid.uuid4().hex + '@example.invalid'
    checks = {}
    first = request('POST', '/api/v1/auth/otp/request', {'email': address})
    original = issued(cr, env, address)
    checks['new_otp_stored_as_salted_hash_without_address_key'] = 'PASS'
    assert request('POST', '/api/v1/auth/otp/request', {'email': address}) == first
    assert issued(cr, env, address) != original, 'A fresh OTP request did not replace the earlier hash'
    code = controlled(cr, env, address)
    wrong = '000000' if code != '000000' else '111111'
    denied = request('POST', '/api/v1/auth/otp/verify', {'email': address, 'code': wrong}, expected=400)
    nobody = request('POST', '/api/v1/auth/otp/verify', {'email': unknown, 'code': wrong}, expected=400)
    assert denial_shape(denied) == denial_shape(nobody), 'A wrong code distinguishes an unknown address'
    checks['fresh_otp_request_and_uniform_wrong_code_denial'] = 'PASS'
    created = request('POST', '/api/v1/auth/otp/verify', {'email': address, 'code': code})
    assert created['is_new_account'] is True, 'The fresh fixture address already had an account'
    account['userId'] = created['user']['id']
    uuid.UUID(account['userId'])
    request('POST', '/api/v1/auth/otp/verify', {'email': address, 'code': code}, expected=400)
    checks['hashed_otp_verifies_once_and_creates_the_account'] = 'PASS'
    # An existing account and an unknown address get the same answer.
    assert request('POST', '/api/v1/auth/otp/request', {'email': address}) == \
        request('POST', '/api/v1/auth/otp/request', {'email': unknown})
    checks['otp_request_answer_uniform_for_existing_and_unknown_address'] = 'PASS'
    again = request('POST', '/api/v1/auth/otp/verify', {'email': address, 'code': controlled(cr, env, address)})
    assert again['is_new_account'] is False and again['user']['id'] == account['userId'], \
        'A second sign-in did not reach the same account'
    checks['sign_in_preserves_the_original_account'] = 'PASS'
    token = again['access_token']
    request('GET', '/api/v2/me/profile', token=token)
    request('POST', '/api/v1/auth/logout', token=token)
    request('GET', '/api/v2/me/profile', token=token, expected=401)
    request('POST', '/api/v1/auth/logout', token=created['access_token'])
    checks['logout_revokes_the_session'] = 'PASS'
    # The password endpoints are gone (passwordless cutover).
    for path in ('/api/v1/auth/login', '/api/v1/auth/register', '/api/v1/auth/forgot-password'):
        status, _ = mail.public('POST', path, {'email': address})
        assert status == 404, 'A password endpoint still answers: ' + path
    checks['password_endpoints_removed'] = 'PASS'
    print(json.dumps({'notificationAuthAcceptance': checks}, sort_keys=True), flush=True)
    return checks
