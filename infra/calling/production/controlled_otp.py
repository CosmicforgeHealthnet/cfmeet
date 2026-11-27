"""Controlled one-time codes for acceptance accounts (passwordless sign-in).

The Meeting Service stores every one-time code only as `<salt hex>:<SHA-256(salt || code) hex>`
under `<prefix>:h:<SHA-256(lower(subject))>` (services/meeting-service/src/services/otp.rs):
`otp:signin` for sign-in codes (subject: the address) and `otp:stepup` for step-up codes
(subject: `user:<id>:<purpose>`). Acceptance tooling cannot read an issued code back; for its
own controlled accounts it REPLACES the issued code with a fresh one in the same format
(`SET ... XX`: only when a code was really issued). Nothing is ever stored in clear.
"""
import hashlib
import os
import secrets


def address_digest(address):
    return hashlib.sha256(address.strip().lower().encode()).hexdigest()


SIGN_IN = 'otp:signin'
STEP_UP = 'otp:stepup'


def hashed_key(address, prefix=SIGN_IN):
    return '%s:h:%s' % (prefix, address_digest(address))


def step_up_key(user_id, purpose):
    """The step-up code key of one account and purpose (e.g. DEACTIVATE_ACCOUNT)."""
    return hashed_key('user:%s:%s' % (user_id, purpose), STEP_UP)


def sealed(code, salt=None):
    salt = salt if salt is not None else os.urandom(16)
    return salt.hex() + ':' + hashlib.sha256(salt + code.encode()).hexdigest()


def new_code():
    return '%06d' % secrets.randbelow(10 ** 6)
