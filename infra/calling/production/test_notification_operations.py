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



class FakeProducerCr(FakeCr):
    """FakeCr whose Messaging database answers the fence row / outbox queries and whose
    readiness and probes are scripted."""

    def __init__(self, rows, protocol=1, done=(), statuses=(), outbox=True):
        super().__init__(rows)
        self.done_ops, self.statuses = set(done), list(statuses)
        self.activations, self.checked, self.probed = [], [], []
        outer = self
        ready = json.dumps({'status': 'ok', 'routingProtocol': protocol} if protocol else {'status': 'ok'})

        class Release(FakeRelease):
            def db_sql(self, db, sql):
                if 'to_regclass' in sql:
                    self.sql.append((db, sql))
                    return 't' if outbox else 'f'
                return FakeRelease.db_sql(self, db, sql)
        self.release = Release(rows)

        class P:
            NET = 'net'

            @staticmethod
            def run(*args):
                if args[-1].startswith('http://127.0.0.1'):
                    return ready
                outer.probed.append(args[-1])
                return outer.statuses.pop(0)
        self.p = P()
        self.PROBE_IMAGE = 'curl'

    def done(self, op):
        return op in self.done_ops

    def tls_route_present(self, port):
        self.checked.append(('tls', port))

    def private_url(self, port):
        return 'https://api.cfmeet.com:%d' % port

    def has_signing_identity(self, name):
        self.checked.append(('signing', name))

    def apply_activation(self, operation, sha, run_id, changes, post=None):
        self.activations.append((operation, changes))
        post()


class MessagingPublications(unittest.TestCase):
    def test_publish_is_granted_to_messaging_on_both_replicas_only(self):
        granted = [(r, i, p) for op, r, i, p in cr.GRANTS if op == 'activate-messaging-publications']
        self.assertEqual(sorted(granted), [('notifications', 'messaging', ('notifications-event.publish',)),
                                           ('notifications-2', 'messaging', ('notifications-event.publish',))])
        workflow = (cr.Path(__file__).resolve().parents[3] / '.github/workflows/cfmeet-coordinated-release.yml').read_text()
        for op in ('activate-messaging-publications', 'activate-notification-fence'):
            self.assertIn(op, cr.OPERATIONS)
            self.assertIn(op, workflow)

    def test_messaging_gets_the_private_route_after_i1_and_its_outbox_are_proven(self):
        fake = FakeProducerCr([], statuses=['401', '401'])
        no.activate_messaging_publications(fake, 'a' * 40, '42-1', '')
        [(operation, changes)] = fake.activations
        self.assertEqual(operation, 'activate-messaging-publications')
        self.assertEqual(changes, [('messaging', {'CF_NOTIFICATIONS_BASE_URL': 'https://api.cfmeet.com:8456'})])
        self.assertEqual(fake.probed, ['https://api.cfmeet.com:8456/internal/v1/notification-events'] * 2)
        self.assertIn(('signing', 'messaging'), fake.checked)
        self.assertEqual(fake.release.sql[-1][0], 'cfmeet_messaging')

    def test_publications_are_refused_without_i1_the_fence_or_the_outbox(self):
        for kwargs in (dict(statuses=['404']), dict(statuses=['401'], protocol=None),
                       dict(statuses=['401'], outbox=False)):
            fake = FakeProducerCr([], **kwargs)
            with self.assertRaises(RuntimeError):
                no.activate_messaging_publications(fake, 'a' * 40, '42-1', '')
            self.assertEqual(fake.activations, [])


class ProducerFence(unittest.TestCase):
    DONE = ('activate-messaging-publications',)

    def test_confirm_names_one_producer_row(self):
        self.assertEqual(no.parse_fence('messaging-message-wakeups-shadow'), ('messaging', 'MESSAGE_WAKEUPS', 'SHADOW'))
        self.assertEqual(no.parse_fence('meeting-service-auth-verification-legacy-only'),
                         ('meeting-service', 'AUTH_VERIFICATION', 'LEGACY_ONLY'))
        for bad in ('', 'messaging-message-wakeups-fenced-new', 'messaging-message-wakeups-fenced-old',
                    'calling-x-shadow', 'messaging-shadow', 'messaging_message-wakeups-shadow'):
            with self.assertRaises(RuntimeError):
                no.parse_fence(bad)
        self.assertRegex('messaging-message-wakeups-legacy-only', r'^[a-z,-]{0,120}$')

    def test_shadow_is_journaled_on_the_messaging_database_and_undone(self):
        fake = FakeProducerCr([{'mode': 'LEGACY_ONLY', 'version': 1}], done=self.DONE)
        no.activate_fence(fake, 'a' * 40, '42-1', 'messaging-message-wakeups-shadow')
        entry = fake.journal[-1]
        self.assertEqual(entry['status'], 'done')
        self.assertEqual(entry['notificationFence'], dict(service='messaging', category='MESSAGE_WAKEUPS',
                                                          before='LEGACY_ONLY', after='SHADOW', version=2))
        db, update = fake.release.sql[-1]
        self.assertEqual(db, 'cfmeet_messaging')
        self.assertIn("UPDATE messaging_notification_routing_state SET mode = 'SHADOW', canary_percent = 0", update)
        self.assertIn('AND routing_version = 1', update)
        no.undo_fence(fake, entry['notificationFence'], '43-1')
        self.assertIn("mode = 'LEGACY_ONLY'", fake.release.sql[-1][1])
        self.assertEqual(fake.emitted[-1][0], 'notificationFenceRestored')

    def test_gates_refuse_and_change_nothing(self):
        cases = [
            # Meeting has no producer code recording decisions: SHADOW would block its legacy emails.
            (dict(rows=[{'mode': 'LEGACY_ONLY', 'version': 1}], done=self.DONE), 'meeting-service-auth-verification-shadow'),
            # The publication activation must come first.
            (dict(rows=[{'mode': 'LEGACY_ONLY', 'version': 1}]), 'messaging-message-wakeups-shadow'),
            # A pre-fence image.
            (dict(rows=[{'mode': 'LEGACY_ONLY', 'version': 1}], done=self.DONE, protocol=None),
             'messaging-message-wakeups-shadow'),
            # This version never leaves a FENCED_* mode.
            (dict(rows=[{'mode': 'FENCED_NEW', 'version': 4}], done=self.DONE), 'messaging-message-wakeups-legacy-only'),
        ]
        for kwargs, confirm in cases:
            rows = kwargs.pop('rows')
            fake = FakeProducerCr(rows, **kwargs)
            with self.assertRaisesRegex(RuntimeError, 'Routing fence gate'):
                no.activate_fence(fake, 'a' * 40, '42-1', confirm)
            self.assertFalse([q for _, q in fake.release.sql if q.startswith('WITH u AS (UPDATE')], confirm)
            self.assertEqual(fake.journal, [])

    def test_back_to_legacy_only_needs_no_prerequisite(self):
        fake = FakeProducerCr([{'mode': 'SHADOW', 'version': 2}], protocol=None)
        no.activate_fence(fake, 'a' * 40, '42-1', 'messaging-message-wakeups-legacy-only')
        self.assertEqual(fake.journal[-1]['notificationFence']['after'], 'LEGACY_ONLY')

    def test_rollback_restores_a_fence_entry(self):
        src = (cr.Path(__file__).resolve().parent / 'coordinated_release.py').read_text()
        self.assertIn("e.get('notificationFence')", src)
        self.assertIn("notif_ops.undo_fence(", src)



class Diagnostics(unittest.TestCase):
    def test_counts_only_from_messaging_and_the_service(self):
        seen = []

        class Release:
            @staticmethod
            def db_sql(db, sql):
                seen.append((db, sql))
                return '{"ok": 1}'

        class Fake:
            SERVICES = cr.SERVICES
            release = Release
            emitted = []

            def emit(self, kind, value):
                self.emitted.append((kind, value))
        fake = Fake()
        out = no.notification_diagnostics(fake)
        self.assertEqual(out, {'messaging': {'ok': 1}, 'notifications': {'ok': 1}})
        self.assertEqual([db for db, _ in seen], ['cfmeet_messaging', 'cfmeet_notifications'])
        for _, sql in seen:
            self.assertTrue(sql.startswith('SELECT json_build_object('))
            for forbidden in ('envelope', 'event_id', 'recipient', 'INSERT', 'UPDATE', 'DELETE'):
                self.assertNotIn(forbidden, sql)
        self.assertEqual(fake.emitted[-1][0], 'notificationDiagnostics')
        src = (cr.Path(__file__).resolve().parent / 'coordinated_release.py').read_text()
        self.assertIn('notif_ops.notification_diagnostics(sys.modules[__name__])', src)


if __name__ == '__main__':
    unittest.main()
