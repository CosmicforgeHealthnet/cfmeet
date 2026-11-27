"""Offline tests: the OTP acceptance follows the sign-in code to wherever the routing fence sent it."""
import unittest
import uuid
from unittest import mock

import email_operations as eo


class FakeCr:
    SERVICES = {'notifications': {'db': 'cfmeet_notifications'}}
    MEETING = 'meeting-service'

    def __init__(self, decisions, service_rows='[]', ledger=''):
        self.decisions = list(decisions)  # successive answers of the decision query
        self.service_rows = service_rows
        self.ledger = ledger
        self.release = mock.Mock()
        self.release.db_sql.side_effect = lambda db, sql: self.service_rows

    def meeting_psql(self, sql):
        if 'notification_route_decisions' in sql:
            return self.decisions.pop(0) if len(self.decisions) > 1 else self.decisions[0]
        assert 'email_deliveries' in sql
        return self.ledger


def journey(cr, accepted=None):
    responses = {
        '/api/v1/auth/otp/request': (200, {}),
        '/api/v1/auth/login': (404, {}),
        '/api/v2/me/profile': (200, {}),
    }
    verifies = iter([(400, {}), (200, {'access_token': 't', 'is_new_account': True, 'user': {'id': 'u'}}), (400, {})])

    def public(method, path, body=None, token=None):
        return next(verifies) if path == '/api/v1/auth/otp/verify' else responses[path]
    with mock.patch.object(eo, 'public', public), \
            mock.patch.object(eo.p, 'env_of', lambda _: {}), mock.patch.object(eo.p, 'inspect', lambda _: {}), \
            mock.patch.object(eo, 'acceptance_recipient', lambda _: 'delivered@resend.dev'), \
            mock.patch.object(eo, 'otp_from_redis', lambda *a: '123456'), \
            mock.patch.object(eo, 'deactivate', lambda *a: 'PASS'), \
            mock.patch.object(eo, 'wait_for', lambda *a: accepted), \
            mock.patch.object(eo.time, 'sleep', lambda s: None):
        return eo.otp_journey(cr, 'run-1')


class OtpRoute(unittest.TestCase):
    def test_legacy_only_uses_the_meeting_ledger(self):
        checks = journey(FakeCr([''], ledger='d1|QUEUED'), accepted={'status': 'ACCEPTED', 'provider': 'resend'})
        self.assertEqual(checks['sign_in_route'], 'OLD')
        self.assertEqual(checks['sign_in_email_recorded'], 'PASS')
        self.assertEqual(checks['sign_in_email_accepted_by_resend'], 'PASS')

    def test_new_uses_the_notification_service_message_of_the_decision(self):
        decision = str(uuid.uuid4())
        cr = FakeCr(['NEW|%s|ROUTED' % decision], service_rows='[{"status": "ACCEPTED", "provider": "resend"}]')
        checks = journey(cr)
        self.assertEqual(checks['sign_in_route'], 'NEW')
        self.assertEqual(checks['sign_in_email_recorded'], 'PASS')
        self.assertEqual(checks['sign_in_email_accepted_by_resend'], 'PASS')
        self.assertIn(decision, cr.release.db_sql.call_args[0][1])
        self.assertEqual(checks['valid_code_signs_in_and_creates_the_account'], 'PASS')

    def test_a_failover_is_followed_to_the_ledger(self):
        decision = str(uuid.uuid4())
        cr = FakeCr(['NEW|%s|ROUTED' % decision, 'OLD|%s|AUTO_FAILOVER' % decision], ledger='d1|QUEUED')
        with mock.patch.object(eo.time, 'monotonic', side_effect=[0, 0, 1000]):
            checks = journey(cr, accepted={'status': 'DELIVERED', 'provider': 'resend'})
        self.assertEqual(checks['sign_in_route'], 'OLD after AUTO_FAILOVER')
        self.assertEqual(checks['sign_in_email_accepted_by_resend'], 'PASS')

    def test_new_without_a_service_email_fails(self):
        cr = FakeCr(['NEW|%s|ROUTED' % uuid.uuid4()], service_rows='[{"status": "QUEUED", "provider": null}]')
        with mock.patch.object(eo.time, 'monotonic', side_effect=[0, 0, 1000]):
            checks = journey(cr)
        self.assertEqual(checks['sign_in_email_recorded'], 'FAIL')

    def test_the_route_is_recorded_not_judged(self):
        """Production run 44218158768: every check PASS with sign_in_route NEW, yet failed."""
        checks = journey(FakeCr(['NEW|%s|ROUTED' % uuid.uuid4()],
                                service_rows='[{"status": "ACCEPTED", "provider": "resend"}]'))
        self.assertEqual(checks['sign_in_route'], 'NEW')
        self.assertTrue(eo.journey_passed(checks))
        self.assertFalse(eo.journey_passed(dict(checks, sign_in_email_recorded='FAIL')))

    def test_the_address_never_reaches_sql_unescaped(self):
        self.assertNotIn("x'y", eo.address_hash_sql("x'y@example.com"))


if __name__ == '__main__':
    unittest.main()
