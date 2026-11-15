"""Controlled verification codes for acceptance accounts (Notification Service N0-8).

Since N0-8 the Meeting Service stores OTP codes only as `<salt hex>:<SHA-256(salt || code) hex>`
under `<prefix>:h:<SHA-256(lower(address))>` (services/meeting-service/src/services/otp.rs), so
acceptance tooling can no longer read an issued code back. For its own controlled accounts it
instead REPLACES the issued code with a fresh one in the same format (`SET ... XX`: only when a
code was really issued). That is the same power as the former read, and nothing is ever stored
in clear. Against the previous image (plaintext code under `<prefix>:<address>`) the tooling
keeps reading the legacy key, so it works across the release in both directions.
"""
import hashlib
import os
import secrets


def address_digest(address):
    return hashlib.sha256(address.strip().lower().encode()).hexdigest()


def hashed_key(address, prefix='otp:verify'):
    return '%s:h:%s' % (prefix, address_digest(address))


def legacy_key(address, prefix='otp:verify'):
    return '%s:%s' % (prefix, address.lower())


def sealed(code, salt=None):
    salt = salt if salt is not None else os.urandom(16)
    return salt.hex() + ':' + hashlib.sha256(salt + code.encode()).hexdigest()


def new_code():
    return '%06d' % secrets.randbelow(10 ** 6)
