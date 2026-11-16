"""Offline tests of the Notification Service activation operations (N6 grants, N14 routing)."""

import json
import re
import unittest

import coordinated_release as cr
import notification_operations as no


class Parse(unittest.TestCase):
    def test_confirm_names_one_row(self):
        self.assertEqual(no.parse_routing('contact-requests-in-app-shadow'), ('CONTACT_REQUESTS', 'IN_APP', 'SHADOW'))
        self.assertEqual(no.parse_routing('meeting-notification-email-active'), ('MEETING_NOTIFICATION', 'EMAIL', 'ACTIVE'))
        self.assertEqual(no.parse_routing('call-invitations-push-off'), ('CALL_INVITATIONS', 'PUSH', 'OFF'))
        for bad in ('', 'shadow', 'contact-requests-sms-active', 'x-email-on', 'contact_requests-in-app-shadow'):
            with self.assertRaises(RuntimeError):
                no.parse_routing(bad)

    def test_confirm_fits_the_workflow_input_rule(self):
        for category, channel in [(c, ch) for c, ch in self.seed_rows()]:
            word = {v: k for k, v in no.CHANNELS.items()}[channel]
            confirm = '%s-%s-active' % (category.lower().replace('_', '-'), word)
            self.assertRegex(confirm, r'^[a-z,-]{0,120}$')
            self.assertEqual(no.parse_routing(confirm), (category, channel, 'ACTIVE'))

    @staticmethod
    def seed_rows():
        text = (cr.Path(__file__).resolve().parents[3] / 'services/notifications/migration/src'
                / 'm20261116_000001_notifications_foundation.rs').read_text()
        return re.findall(r"\('([A-Z_]+)', '([A-Z_]+)', 'OFF', 'migration'\)", text)


class Gates(unittest.TestCase):
    facts = dict(email_provider='resend', resend_done=True, owners_done=True)

    def test_off_and_shadow_are_always_allowed(self):
        for category in no.CRITICAL | no.SOCIAL:
            self.assertIsNone(no.gate(category, 'EMAIL', 'SHADOW', {}))
            self.assertIsNone(no.gate(category, 'EMAIL', 'OFF', {}))

    def test_critical_active_is_refused_by_this_version(self):
        for category in no.CRITICAL:
            self.assertIn('CRITICAL', no.gate(category, 'EMAIL', 'ACTIVE', self.facts))

    def test_email_active_needs_a_provider_and_e1(self):
        self.assertIn('provider', no.gate('DEVELOPER_PLATFORM', 'EMAIL', 'ACTIVE', dict(self.facts, email_provider='none')))
        self.assertIn('E1', no.gate('DEVELOPER_PLATFORM', 'EMAIL', 'ACTIVE', dict(self.facts, resend_done=False)))
        self.assertIsNone(no.gate('DEVELOPER_PLATFORM', 'EMAIL', 'ACTIVE', self.facts))

    def test_social_active_needs_the_owner_apis(self):
        self.assertIn('owners', no.gate('CONTACT_REQUESTS', 'IN_APP', 'ACTIVE', dict(self.facts, owners_done=False)))
        self.assertIsNone(no.gate('CONTACT_REQUESTS', 'IN_APP', 'ACTIVE', self.facts))
        self.assertIsNone(no.gate('ORGANIZATION_UPDATES', 'IN_APP', 'ACTIVE', dict(self.facts, owners_done=False)))


class FakeRelease:
    def __init__(self, rows):
        self.rows = rows
        self.sql = []

    def db_sql(self, db, sql):
        self.sql.append((db, sql))
        if sql.startswith('SELECT coalesce(json_agg'):
            return json.dumps(self.rows)
        if sql.startswith('WITH u AS (UPDATE'):
            return '1:%d' % (self.rows[0]['version'] + 1) if self.rows else '0:0'
        raise AssertionError(sql)


class FakeCr:
    SERVICES = cr.SERVICES

    def __init__(self, rows):
        self.release = FakeRelease(rows)
        self.journal = []
        self.emitted = []

    def require_release(self, sha):
        return sha

    def env_of(self, name):
        return {'NOTIFICATIONS_EMAIL_PROVIDER': 'none'}

    def done(self, op):
        return False

    def begin(self, op, sha, run_id):
        e = dict(operation=op, sha=sha, runId=run_id, status='running', components=[])
        self.journal.append(e)
        return e

    def commit(self, entry):
        pass

    def emit(self, kind, value):
        self.emitted.append((kind, value))


class Routing(unittest.TestCase):
    def test_a_shadow_change_is_journaled_with_its_previous_mode_and_undone(self):
        fake = FakeCr([{'mode': 'OFF', 'version': 3}])
        no.activate_routing(fake, 'a' * 40, '42-1', 'contact-requests-in-app-shadow')
        entry = fake.journal[-1]
        self.assertEqual(entry['status'], 'done')
        self.assertEqual(entry['notificationRouting'], dict(category='CONTACT_REQUESTS', channel='IN_APP',
                                                            before='OFF', after='SHADOW', version=4))
        update = fake.release.sql[-1][1]
        self.assertIn("mode = 'SHADOW'", update)
        self.assertIn('AND version = 3', update)
        self.assertEqual(fake.release.sql[-1][0], 'cfmeet_notifications')
        no.undo_routing(fake, entry['notificationRouting'], '43-1')
        self.assertIn("mode = 'OFF'", fake.release.sql[-1][1])

    def test_a_refused_gate_changes_nothing(self):
        fake = FakeCr([{'mode': 'SHADOW', 'version': 3}])
        with self.assertRaises(RuntimeError):
            no.activate_routing(fake, 'a' * 40, '42-1', 'auth-verification-email-active')
        self.assertEqual(fake.release.sql, [])
        self.assertEqual(fake.journal, [])

    def test_operations_are_registered_and_grants_are_valid(self):
        self.assertIn('activate-notifications-routing', cr.OPERATIONS)
        self.assertIn('activate-notifications-owners', cr.OPERATIONS)
        granted = [(r, i, p) for op, r, i, p in cr.GRANTS if op == 'activate-notifications-owners']
        self.assertEqual({r for r, _, _ in granted}, {'meeting-service', 'messaging', 'communications', 'organizations'})
        self.assertTrue(all(i == 'notifications' for _, i, _ in granted))


if __name__ == '__main__':
    unittest.main()
