"""N0 production auth checks on newly registered, disposable acceptance fixtures only.

No provider activation or credential/configuration changes. Redis access is restricted to
the supplied fresh fixture's OTP keys. Issued hashes are inspected in memory, never logged;
controlled inputs use SET XX and preserve the issued TTL. A second fixture exercises the
previous image's legacy OTP representation. No user data or secrets enter the receipt.
"""
import json
import re
import secrets
import uuid

import controlled_otp as otp
import email_operations as mail


def require_fixture(account, registration):
    assert re.fullmatch(r'cfmeet_accept_[abc]_[0-9a-f]{8}', account['username']), 'Not a fresh controlled fixture'
    assert account['email'] == account['username'] + '@example.invalid', 'Not a controlled fixture address'
    assert registration['user_id'] == account['userId'], 'Fixture registration mismatch'
    uuid.UUID(account['userId'])


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


def issued(cr, env, address, prefix):
    key = otp.hashed_key(address, prefix)
    value = redis(cr, env, 'GET "$KEY"', {'KEY': key})
    assert re.fullmatch(r'[0-9a-f]{32}:[0-9a-f]{64}', value), 'Issued OTP is not a salted hash'
    assert redis(cr, env, 'EXISTS "$KEY"', {'KEY': otp.legacy_key(address, prefix)}) == '0', 'New OTP left a legacy key'
    return value


def controlled(cr, env, address, prefix):
    issued(cr, env, address, prefix)
    code = otp.new_code()
    assert redis(cr, env, 'SET "$KEY" "$VALUE" XX KEEPTTL',
                 {'KEY': otp.hashed_key(address, prefix), 'VALUE': otp.sealed(code)}) == 'OK', 'No issued OTP to replace'
    return code


def run_pending(account, registration, redis_url, legacy=False):
    """Called before verifying a fresh fixture; updates only its password after reset."""
    require_fixture(account, registration)
    import coordinated_release as cr
    env = {'REDIS_URL': redis_url}
    address = account['email']
    prefix = 'otp:verify'
    checks = {}
    original = issued(cr, env, address, prefix)
    checks['new_otp_stored_as_salted_hash_without_address_key'] = 'PASS'
    if legacy:
        # Representative pre-release code on this fresh fixture only, with a bounded TTL.
        code = otp.new_code()
        redis(cr, env, 'SET "$KEY" "$VALUE" EX 60', {'KEY': otp.legacy_key(address), 'VALUE': code})
        redis(cr, env, 'DEL "$KEY"', {'KEY': otp.hashed_key(address)})
    else:
        duplicate = request('POST', '/api/v1/auth/register', {
            'username': 'cfmeet_dup_' + uuid.uuid4().hex[:12], 'email': address,
            'password': account['password'], 'confirm_password': account['password']}, expected=201)
        assert set(duplicate) == set(registration), 'Registration response shape reveals an existing address'
        assert all(duplicate[k] == registration[k] for k in ('email', 'role', 'status'))
        assert duplicate['user_id'] != account['userId']
        checks['duplicate_registration_has_same_public_response'] = 'PASS'
        request('POST', '/api/v1/auth/resend-verification', {'email': address})
        assert issued(cr, env, address, prefix) != original, 'Fresh OTP did not replace the earlier hash'
        code = controlled(cr, env, address, prefix)
        wrong = '000000' if code != '000000' else '111111'
        request('POST', '/api/v1/auth/verify-email', {'email': address, 'token': wrong}, expected=400)
        checks['fresh_otp_request_and_wrong_code_denial'] = 'PASS'
    request('POST', '/api/v1/auth/verify-email', {'email': address, 'token': code})
    request('POST', '/api/v1/auth/verify-email', {'email': address, 'token': code}, expected=400)
    checks['legacy_otp_compatible_and_single_use' if legacy else 'hashed_otp_verifies_and_is_single_use'] = 'PASS'
    if not legacy:
        login = request('POST', '/api/v1/auth/login', {'email': address, 'password': account['password']})
        token = login['access_token']
        assert login['user']['id'] == account['userId'], 'Duplicate registration changed the account'
        checks['login_and_original_account_preserved'] = 'PASS'
        request('POST', '/api/v1/auth/forgot-password', {'email': address})
        code = controlled(cr, env, address, 'otp:reset')
        wrong = '000000' if code != '000000' else '111111'
        denied = request('POST', '/api/v1/auth/verify-reset-token', {'email': address, 'token': wrong}, expected=400)
        unknown = request('POST', '/api/v1/auth/verify-reset-token', {
            'email': 'cfmeet_unknown_' + uuid.uuid4().hex + '@example.invalid', 'token': wrong}, expected=400)
        assert denial_shape(denied) == denial_shape(unknown), 'Reset probe distinguishes an unknown address from an incorrect code'
        probe = request('POST', '/api/v1/auth/verify-reset-token', {'email': address, 'token': code})
        assert probe['valid'] is True
        replacement = 'Aa1!' + secrets.token_urlsafe(28)
        body = {'email': address, 'token': code, 'new_password': replacement, 'confirm_password': replacement}
        request('POST', '/api/v1/auth/reset-password', body)
        # Record the new fixture password before further assertions (never printed).
        previous = account['password']
        account['password'] = replacement
        request('POST', '/api/v1/auth/reset-password', body, expected=400)
        request('GET', '/api/v2/me/profile', token=token, expected=401)
        request('POST', '/api/v1/auth/login', {'email': address, 'password': previous}, expected=401)
        fresh = request('POST', '/api/v1/auth/login', {'email': address, 'password': replacement})
        request('POST', '/api/v1/auth/logout', token=fresh['access_token'])
        request('GET', '/api/v2/me/profile', token=fresh['access_token'], expected=401)
        checks['reset_code_validation_single_use_and_new_password_login'] = 'PASS'
        checks['reset_probe_uniform_denial_for_unknown_address_and_wrong_code'] = 'PASS'
        checks['reset_revokes_old_session_and_logout_revokes_new_session'] = 'PASS'
    print(json.dumps({'notificationAuthAcceptance': checks}, sort_keys=True), flush=True)
    return checks
