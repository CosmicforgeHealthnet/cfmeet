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


class FakeRelayCr(FakeCr):
    """Records probes, signing checks and the activation request of activate_relay."""

    def __init__(self, statuses):
        super().__init__([])
        self.statuses = list(statuses)
        self.probed, self.activations, self.checked = [], [], []
        outer = self

        class P:
            NET = 'net'

            @staticmethod
            def run(*args):
                outer.probed.append(args[-1])
                return outer.statuses.pop(0)
        self.p = P()
        self.PROBE_IMAGE = 'curl'

    def tls_route_present(self, port):
        self.checked.append(('tls', port))

    def private_url(self, port):
        return 'https://api.cfmeet.com:%d' % port

    def has_signing_identity(self, name):
        self.checked.append(('signing', name))

    def apply_activation(self, operation, sha, run_id, changes, post=None):
        self.activations.append((operation, changes))
        post()


class Relay(unittest.TestCase):
    def test_relay_grants_i2_to_meeting_on_both_replicas_only(self):
        granted = [(r, i, p) for op, r, i, p in cr.GRANTS if op == 'activate-notifications-relay']
        self.assertEqual(sorted(granted), [('notifications', 'meeting-service', ('notifications-user.forward',)),
                                           ('notifications-2', 'meeting-service', ('notifications-user.forward',))])
        self.assertIn('activate-notifications-relay', cr.OPERATIONS)
        workflow = (cr.Path(__file__).resolve().parents[3] / '.github/workflows/cfmeet-coordinated-release.yml').read_text()
        self.assertIn('activate-notifications-relay', workflow)

    def test_relay_points_meeting_at_the_private_route_and_probes_before_and_after(self):
        fake = FakeRelayCr(['401', '401', '401', '401'])
        no.activate_relay(fake, 'a' * 40, '42-1', '')
        [(operation, changes)] = fake.activations
        self.assertEqual(operation, 'activate-notifications-relay')
        self.assertEqual(changes, [('meeting-service', {'CF_NOTIFICATIONS_BASE_URL': 'https://api.cfmeet.com:8456'})])
        self.assertEqual(fake.probed, ['https://api.cfmeet.com:8456/internal/v1/notifications/dispatch',
                                       'https://api.cfmeet.com:8448/api/v2/notifications/read',
                                       'https://api.cfmeet.com:8448/api/v2/notifications/read',
                                       'https://api.cfmeet.com:8456/internal/v1/notifications/dispatch'])
        self.assertIn(('signing', 'meeting-service'), fake.checked)

    def test_relay_refuses_when_an_image_does_not_serve_its_side(self):
        for statuses in (['404'], ['401', '404'], ['401', '200']):
            fake = FakeRelayCr(statuses)
            with self.assertRaises(RuntimeError):
                no.activate_relay(fake, 'a' * 40, '42-1', '')
            self.assertEqual(fake.activations, [])

    def test_a_relay_answering_after_activation_fails_the_step(self):
        fake = FakeRelayCr(['401', '401', '503', '401'])
        with self.assertRaises(RuntimeError):
            no.activate_relay(fake, 'a' * 40, '42-1', '')


class OwnerDenial(unittest.TestCase):
    def fake(self, answers):
        calls = []

        class Release:
            NOTIFICATION_OWNER_DENIAL_PROBES = cr.release.NOTIFICATION_OWNER_DENIAL_PROBES

            @staticmethod
            def secure_public_ingress():
                calls.append('secure')

        class P:
            @staticmethod
            def reload_host_nginx():
                calls.append('reload')

        class Fake:
            release, p = Release, P
            emitted = []

            @staticmethod
            def public_denial_probe(path):
                calls.append(path)
                return answers.pop(0)

            def emit(self, kind, value):
                self.emitted.append((kind, value))
        return Fake(), calls

    def test_owner_routes_are_denied_and_proven_before_activation(self):
        fake, calls = self.fake([True, True])
        no.deny_owner_routes_publicly(fake)
        self.assertEqual(calls[:2], ['secure', 'reload'])
        self.assertEqual(calls[2:], list(cr.release.NOTIFICATION_OWNER_DENIAL_PROBES))
        self.assertEqual(fake.emitted[-1][0], 'publicOwnerDenial')

    def test_a_reachable_owner_route_stops_the_activation(self):
        fake, _ = self.fake([True, False])
        with self.assertRaises(RuntimeError):
            no.deny_owner_routes_publicly(fake)


class FenceCr:
    """Fake coordinated_release for the routing-fence helpers."""
    SERVICES = cr.SERVICES
    PROBE_IMAGE = 'curl'

    def __init__(self, table_exists=True, fenced='', image_has_fence=False, ready='{}'):
        self.sql_seen, self.docker = [], []
        outer = self

        def sql(q):
            outer.sql_seen.append(q)
            if 'to_regclass' in q:
                return 't' if table_exists else 'f'
            return fenced
        self.meeting_psql = sql

        class Release:
            @staticmethod
            def db_sql(db, q):
                outer.sql_seen.append((db, q))
                return sql(q)
        self.release = Release

        class P:
            @staticmethod
            def run(*args):
                outer.docker.append(args)
                if '/bin/sh' in args:
                    return 'fence\n' if image_has_fence else 'none\n'
                return ready
        self.p = P


class Fence(unittest.TestCase):
    def test_floor_is_inert_while_every_category_is_legacy_only_or_the_table_is_missing(self):
        for fake in (FenceCr(fenced=''), FenceCr(table_exists=False)):
            no.rollback_floor(fake, 'meeting-service', 'sha256:old')
            self.assertEqual(fake.docker, [])
        no.rollback_floor(FenceCr(fenced='X:FENCED_NEW'), 'calling', 'sha256:old')  # not a fence producer

    def test_floor_refuses_a_pre_fence_image_once_a_category_left_legacy_only(self):
        fake = FenceCr(fenced='AUTH_VERIFICATION:SHADOW')
        with self.assertRaisesRegex(RuntimeError, 'Rollback floor'):
            no.rollback_floor(fake, 'meeting-service', 'sha256:old')
        self.assertIn('test -x /app/notification-fence-migration', fake.docker[-1][-1])
        no.rollback_floor(FenceCr(fenced='AUTH_VERIFICATION:SHADOW', image_has_fence=True), 'meeting-service', 'sha256:new')

    def test_messaging_uses_its_prefixed_table_and_database(self):
        fake = FenceCr(fenced='MESSAGE_WAKEUPS:FENCED_OLD')
        self.assertEqual(no.fenced_categories(fake, 'messaging'), ['MESSAGE_WAKEUPS:FENCED_OLD'])
        routed = [q for q in fake.sql_seen if isinstance(q, tuple)]
        self.assertTrue(routed and all(db == 'cfmeet_messaging' for db, _ in routed))
        self.assertIn('messaging_notification_routing_state', routed[-1][1])

    def test_protocol_precondition_reads_readiness(self):
        self.assertEqual(no.require_fence_protocol(FenceCr(ready='{"status":"ok","routingProtocol":1}'), 'messaging'), 1)
        for ready in ('{"status":"ok"}', 'not json', '{"routingProtocol":"1"}'):
            with self.assertRaises(RuntimeError):
                no.require_fence_protocol(FenceCr(ready=ready), 'meeting-service')

    def test_release_tooling_runs_and_verifies_both_fence_histories(self):
        self.assertIn('notification-fence-migration', cr.SERVICES['meeting-service']['migrations'])
        self.assertEqual(cr.SERVICES['messaging']['migrations'][-1], 'messaging-notification-fence-migration')
        catalog = (cr.Path(__file__).resolve().parents[3] / 'infra/deployment/backup/catalog.conf').read_text()
        self.assertIn('notification_fence_migrations', catalog)
        self.assertIn('messaging_notification_fence_seaql_migrations', catalog)


if __name__ == '__main__':
    unittest.main()
