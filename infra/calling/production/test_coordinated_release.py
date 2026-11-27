import base64
import contextlib
import hashlib
import io
import json
import os
import re
import tempfile
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

import coordinated_release as cr
import production as p

REPO = Path(__file__).resolve().parents[3]
SHA = 'a' * 40


def permission_registry():
    """{permission: (receiver id, {grantable issuer ids})} parsed from the Rust registry."""
    ids = dict(re.findall(r'pub const ([A-Z_]+): &str = "([a-z.-]+)";',
                          (REPO / 'packages/service-auth/src/identity.rs').read_text()))
    text = (REPO / 'packages/service-auth/src/permissions.rs').read_text()
    registry = {}
    # Registry entries are written both on one line and over several lines: split the
    # REGISTRY table at every entry start and read each entry's fields.
    table = text[text.index('pub const REGISTRY: &[PermissionDefinition] = &['):]
    table = table[:table.index('\n];\n')]
    for block in table.split('PermissionDefinition {')[1:]:
        name = re.search(r'name: "([^"]+)"', block)
        receiver = re.search(r'receiver: services::([A-Z_]+)', block)
        grantable = re.search(r'grantable_to: &\[(.*?)\]', block, re.S)
        if name and receiver and grantable:
            issuers = {ids[x] for x in re.findall(r'services::([A-Z_]+)', grantable.group(1))}
            registry[name.group(1)] = (ids[receiver.group(1)], issuers)
    return registry


class FakeDocker:
    """Container names and lifecycle only; every call is recorded."""

    def __init__(self, names):
        self.names = list(names)
        self.calls = []

    def run(self, *args, input=None):
        self.calls.append(args)
        if args[:2] == ('docker', 'ps'):
            return '\n'.join(self.names)
        if args[:2] == ('docker', 'rename'):
            self.names[self.names.index(args[2])] = args[3]
        if args[:2] == ('docker', 'create'):
            self.names.append(args[args.index('--name') + 1])
        return ''

    def api(self, method, path, payload=None):
        self.calls.append(('api', method, path))
        if path.startswith('/containers/create?name='):
            self.names.append(path.split('=', 1)[1])


@contextlib.contextmanager
def isolated_work():
    with tempfile.TemporaryDirectory() as folder:
        work = Path(folder)
        with patch.object(cr, 'WORK', work), patch.object(cr, 'JOURNAL', work / 'journal.json'):
            yield work


class ResourceGate(unittest.TestCase):
    def test_mem_available_is_read_in_mib(self):
        self.assertEqual(cr.mem_available_mib('MemTotal: 8000000 kB\nMemAvailable: 6963200 kB\n'), 6800)
        with self.assertRaises(RuntimeError):
            cr.mem_available_mib('MemTotal: 1 kB\n')

    def test_required_memory_is_new_container_limits_plus_transient(self):
        new = cr.planned_new(['cfmeet-platform-access'])
        self.assertNotIn('platform-access', new)
        self.assertEqual(cr.required_mib(new), 192 * 6 + 256 + 128 + 256 * 2 + 128 + cr.TRANSIENT_MIB)
        self.assertEqual(cr.required_mib([]), cr.TRANSIENT_MIB)
        for name in cr.ORDER:
            s = cr.SERVICES[name]
            self.assertTrue(s['existing'] or 64 <= s['memory'] <= 512, name)

    def test_gate_passes_with_margin_and_reports_exact_shortfall(self):
        ok = cr.resource_gate(6800, 2368)
        self.assertEqual(ok['remainingAfterReleaseMiB'], 4432)
        self.assertEqual(ok['shortfallMiB'], 0)
        with self.assertRaisesRegex(RuntimeError, r'524 MiB short \(available 2000, required 1500, margin 1024\)'):
            cr.resource_gate(2000, 1500)
        cr.resource_gate(2524, 1500)  # exactly the margin is enough


class Grants(unittest.TestCase):
    def test_every_grant_exists_in_the_permission_registry_for_that_receiver_and_issuer(self):
        registry = permission_registry()
        text = (REPO / 'packages/service-auth/src/permissions.rs').read_text()
        all_block = text[text.index('pub const ALL'):]
        all_block = all_block[:all_block.index('];')]
        all_permissions = re.findall(r'    InternalPermission::([A-Za-z]+),\n', all_block)
        self.assertEqual(len(registry), len(all_permissions))
        self.assertGreater(len(registry), 60)
        self.assertEqual(cr.validate_grants(registry), [])

    def test_meeting_notice_preference_grant_is_in_the_hub_release_and_an_operation(self):
        # Notification Service N0-1 (S-1): without communications-preferences.read the
        # Meeting Service cannot read opt-outs and every meeting notice fails closed. The Hub
        # bundle is rebuilt from HUB_INCOMING on every release, so the grant must be there,
        # and the activation operation applies the same grant without a Hub release.
        import communications_release
        registry = permission_registry()
        self.assertIn('communications-preferences.read', communications_release.HUB_INCOMING['meeting-service'])
        self.assertIn(('activate-meeting-notice-preferences', 'communications', 'meeting-service',
                       ('communications-preferences.read',)), cr.GRANTS)
        self.assertEqual(cr.ACTIVATIONS['activate-meeting-notice-preferences'][0], ())
        # Every Hub incoming grant is received by the Hub and grantable to that issuer.
        rows = tuple(('hub-release', 'communications', issuer, tuple(perms))
                     for issuer, perms in communications_release.HUB_INCOMING.items())
        self.assertEqual(cr.validate_grants(registry, rows), [])

    def test_invalid_grants_are_reported(self):
        registry = permission_registry()
        bad = (('x', 'calling', 'meeting-service', ('organization.read',)),
               ('x', 'platform-access', 'numbering', ('platform-identity.provision-first-party',)),
               ('x', 'organizations', 'meeting-service', ('organization.everything',)))
        errors = cr.validate_grants(registry, bad)
        self.assertEqual(len(errors), 3)
        self.assertTrue(any('received by cfmeet.organizations' in e for e in errors))
        self.assertTrue(any('may not be granted to cfmeet.numbering' in e for e in errors))
        self.assertTrue(any('unknown permission' in e for e in errors))

    def test_grants_belong_to_activation_steps_never_to_release(self):
        for operation, receiver, issuer, permissions in cr.GRANTS:
            self.assertIn(operation, cr.ACTIVATIONS)
            self.assertIn(receiver, cr.SERVICES)
            self.assertIn(issuer, tuple(cr.SERVICES) + cr.OPERATOR_IDENTITIES)
            self.assertFalse(any(p_ in ('*', 'admin', 'all', 'root') for p_ in permissions))

    def test_merge_never_drops_issuers_keys_or_permissions(self):
        key_a = {'kty': 'OKP', 'crv': 'Ed25519', 'kid': 'a-1', 'x': 'AA'}
        key_b = {'kty': 'OKP', 'crv': 'Ed25519', 'kid': 'a-2', 'x': 'BB'}
        bundle = cr.empty_bundle('platform-access')
        bundle = cr.merge_grant(bundle, 'cfmeet.calling', key_a, ['platform-identity.read'])
        before = json.dumps(bundle, sort_keys=True)
        merged = cr.merge_grant(bundle, 'cfmeet.calling', key_b, ['platform-principal.resolve', 'platform-identity.read'])
        merged = cr.merge_grant(merged, 'cfmeet.organizations', key_a, ['platform-identity.provision-first-party'])
        self.assertEqual(json.dumps(bundle, sort_keys=True), before)  # input untouched
        calling = merged['trusted_issuers'][0]
        self.assertEqual(calling['permissions'], ['platform-identity.read', 'platform-principal.resolve'])
        self.assertEqual([k['kid'] for k in calling['keys']], ['a-1', 'a-2'])
        self.assertEqual([x['issuer'] for x in merged['trusted_issuers']], ['cfmeet.calling', 'cfmeet.organizations'])
        self.assertEqual(merged['trusted_issuers'][1]['audience'], 'cfmeet.platform-access')
        again = cr.merge_grant(merged, 'cfmeet.organizations', key_a, ['platform-identity.provision-first-party'])
        self.assertEqual(again, merged)
        with self.assertRaises(AssertionError):
            cr.merge_grant(dict(bundle, runtime_environment='staging'), 'cfmeet.calling', key_a, [])

    def test_trust_mount_is_repointed_in_either_docker_form(self):
        binds = {'HostConfig': {'Binds': ['/old:/run/trust.json:ro', '/k:/run/key.pem:ro']}}
        cr.set_mount(binds, '/run/trust.json', '/new.json')
        self.assertEqual(binds['HostConfig']['Binds'], ['/new.json:/run/trust.json:ro', '/k:/run/key.pem:ro'])
        mounts = {'HostConfig': {'Mounts': [{'Type': 'bind', 'Source': '/old', 'Target': '/run/trust.json', 'ReadOnly': True}]}}
        cr.set_mount(mounts, '/run/trust.json', '/new.json')
        self.assertEqual(mounts['HostConfig']['Mounts'][0]['Source'], '/new.json')
        self.assertNotIn('Binds', mounts['HostConfig'])


class TrustBundlePreparation(unittest.TestCase):
    def test_activation_bundle_is_a_new_version_that_keeps_every_existing_issuer(self):
        with isolated_work() as work:
            original = work / 'platform-access-trust.json'
            existing = cr.merge_grant(cr.empty_bundle('platform-access'), 'cfmeet.calling',
                                      {'kid': 'calling-production-1', 'x': 'AA'}, ['platform-identity.read'])
            original.write_text(json.dumps(existing))
            original.chmod(0o444)
            containers = {
                'cfmeet-platform-access': {'Name': '/cfmeet-platform-access', 'Config': {'Env': [
                    'CF_SERVICE_TRUST_BUNDLE_FILE=/run/secrets/platform-trust.json']},
                    'Mounts': [{'Source': str(original), 'Destination': '/run/secrets/platform-trust.json'}]},
                'cfmeet-organizations': {'Name': '/cfmeet-organizations', 'Config': {'Env': [
                    'CF_SERVICE_AUTH_KEY_ID=organizations-production-1',
                    'CF_SERVICE_AUTH_PRIVATE_KEY_FILE=/run/secrets/service-auth.pem']},
                    'Mounts': [{'Source': '/opt/cfmeet-core/secrets/organizations-service-auth.pem',
                                'Destination': '/run/secrets/service-auth.pem'}]}}
            key = {'kty': 'OKP', 'crv': 'Ed25519', 'kid': 'organizations-production-1', 'x': 'BB'}
            with patch.object(p, 'inspect', side_effect=lambda n: containers[n]), \
                    patch.object(cr.release, 'public_key', return_value=key) as public_key, \
                    patch.object(cr.os, 'chown'):
                bundles = cr.bundles_for('activate-provisioning', '42')
            public_key.assert_called_once_with('/opt/cfmeet-core/secrets/organizations-service-auth.pem',
                                               'organizations-production-1')
            target, path = bundles['platform-access']
            self.assertEqual(target, '/run/secrets/platform-trust.json')
            self.assertEqual(json.loads(original.read_text()), existing)  # never modified in place
            new = json.loads(path.read_text())
            self.assertEqual([x['issuer'] for x in new['trusted_issuers']], ['cfmeet.calling', 'cfmeet.organizations'])
            self.assertEqual(new['trusted_issuers'][1]['permissions'], ['platform-identity.provision-first-party'])
            self.assertEqual(path.stat().st_mode & 0o777, 0o444)
            backup = work / 'trust' / 'backup' / 'platform-access.42.before.json'
            self.assertEqual(json.loads(backup.read_text()), existing)
            self.assertEqual(cr.bundles_for('activate-org-events-unknown', '43'), {})


class DependencyOrder(unittest.TestCase):
    def test_owners_are_released_before_their_callers(self):
        self.assertEqual(sorted(cr.ORDER), sorted(cr.SERVICES))
        for name, depends in cr.DEPENDS.items():
            for dependency in depends:
                self.assertLess(cr.ORDER.index(dependency), cr.ORDER.index(name), (dependency, name))
        index = cr.ORDER.index
        middle = ('messaging', 'calling', 'communications', 'numbering', 'privacy-relay', 'sms-gateway', 'telephony')
        self.assertEqual(index('platform-access'), 0)
        self.assertEqual(index('organizations'), 1)
        self.assertTrue(all(index('organizations') < index(m) < index('meeting-service') for m in middle))
        self.assertLess(index('meeting-service'), index('developer-platform'))
        self.assertEqual(cr.ORDER[-1], 'api-gateway')

    def test_activation_requirements_follow_the_runbook_order(self):
        for operation, (requires, _) in cr.ACTIVATIONS.items():
            for required in requires:
                for alternative in (required if isinstance(required, tuple) else (required,)):
                    self.assertIn(alternative, cr.ACTIVATIONS)
        chain = ['activate-provisioning', 'activate-org-events', 'org-cutover-export', 'org-cutover-import',
                 'org-cutover-shadow', 'org-cutover-authoritative']
        for previous, step in zip(chain, chain[1:]):
            self.assertEqual(cr.ACTIVATIONS[step][0], (previous,))
        # Either cutover procedure (standard, or the empty-dataset first activation) unlocks Calling.
        self.assertEqual(cr.ACTIVATIONS['org-cutover-first-activation'][0], ('activate-org-events',))
        self.assertEqual(cr.ACTIVATIONS['activate-enterprise-calling'][0],
                         (('org-cutover-authoritative', 'org-cutover-first-activation'),
                          'activate-organization-bindings'))
        self.assertEqual(cr.ACTIVATIONS['activate-organization-bindings'][0], ('activate-provisioning',))
        self.assertEqual(cr.ACTIVATIONS['activate-gateway-routing'][0], ('activate-developer-platform',))

    def test_catalogue_matches_dockerfiles_and_backup_catalog(self):
        self.assertEqual(set(cr.IMAGES), {cr.image_of(n) for n in cr.ORDER} | {'security-scan-gateway'})
        for image in cr.IMAGES:
            self.assertTrue((REPO / 'services' / image / 'Dockerfile').is_file(), image)
        catalog = {}
        for line in (REPO / 'infra/deployment/backup/catalog.conf').read_text().splitlines():
            if line.strip() and not line.startswith('#'):
                cols = line.split()
                catalog[cols[0]] = cols[2].split(',')
        for name in cr.ORDER:
            s = cr.SERVICES[name]
            dockerfile = (REPO / 'services' / cr.image_of(name) / 'Dockerfile').read_text()
            for binary in list(s['migrations']) + [c.removeprefix('./') for c in s['command']]:
                self.assertRegex(dockerfile, r'\./%s\n' % re.escape(binary), (name, binary))
            if s['port']:
                self.assertIn('EXPOSE %d' % s['port'], dockerfile, name)
            if s['history'] and not s['identity']:
                self.assertEqual(list(s['history']), catalog[name], name)

    def test_private_tls_ports_are_unique(self):
        ports = [s['tls'] for s in cr.SERVICES.values() if s['tls']]
        self.assertEqual(len(ports), len(set(ports)))
        import scan_gateway
        self.assertNotIn(scan_gateway.TLS_PORT, ports)


class ImageVerification(unittest.TestCase):
    def image(self, revision, image_id='sha256:new'):
        return json.dumps([{'Id': image_id, 'Config': {'Labels': {'org.opencontainers.image.revision': revision}}}])

    def test_label_and_workflow_image_id_must_both_match(self):
        manifest = {'calling': {'imageId': 'sha256:new'}}
        with patch.object(p, 'run', return_value=self.image(SHA)) as run:
            self.assertEqual(cr.verify_image('calling', SHA, manifest), 'sha256:new')
            run.assert_called_with('docker', 'image', 'inspect', 'ghcr.io/cosmicforgehealthnet/cfmeet-calling:' + SHA)
        with patch.object(p, 'run', return_value=self.image('b' * 40)):
            with self.assertRaisesRegex(RuntimeError, 'revision mismatch'):
                cr.verify_image('calling', SHA, manifest)
        with patch.object(p, 'run', return_value=self.image(SHA, 'sha256:other')):
            with self.assertRaisesRegex(RuntimeError, 'workflow-verified'):
                cr.verify_image('calling', SHA, manifest)
        with self.assertRaisesRegex(RuntimeError, 'lacks'):
            cr.verify_image('numbering', SHA, manifest)

    def test_running_container_must_run_the_release_image(self):
        running = {'State': {'Running': True}, 'Image': 'sha256:new'}
        with patch.object(p, 'inspect', return_value=running), patch.object(p, 'run', return_value=self.image(SHA)):
            cr.verify_running('cfmeet-calling', 'sha256:new', SHA)
            with self.assertRaises(RuntimeError):
                cr.verify_running('cfmeet-calling', 'sha256:other', SHA)
        with patch.object(p, 'inspect', return_value=running), patch.object(p, 'run', return_value=self.image('c' * 40)):
            with self.assertRaisesRegex(RuntimeError, 'revision'):
                cr.verify_running('cfmeet-calling', 'sha256:new', SHA)


class ProductionBrowserOrigins(unittest.TestCase):
    def test_meeting_service_release_pins_the_production_web_origins(self):
        self.assertEqual(cr.RELEASE_PINNED_ENV['meeting-service']['FRONTEND_URLS'],
                         'https://cfmeet.com,https://www.cfmeet.com')
        self.assertNotIn('localhost', cr.RELEASE_PINNED_ENV['meeting-service']['FRONTEND_URLS'])

    def test_verify_fails_while_the_audited_list_is_running(self):
        audited = 'http://localhost:3000,http://localhost:3001,https://17730sdl-3000.uks1.devtunnels.ms'
        self.assertEqual(cr.pinned_env_checks({'meeting-service': {'FRONTEND_URLS': audited}}),
                         {'pinned meeting-service FRONTEND_URLS': 'FAIL'})
        pinned = {'meeting-service': {'FRONTEND_URLS': 'https://cfmeet.com,https://www.cfmeet.com'}}
        self.assertEqual(cr.pinned_env_checks(pinned), {'pinned meeting-service FRONTEND_URLS': 'PASS'})


class RollbackOnFailedHealth(unittest.TestCase):
    def test_existing_service_is_restored_exactly_when_the_new_image_is_unhealthy(self):
        docker = FakeDocker(['cfmeet-numbering', 'cosmicforge-db'])
        original = {'State': {'Running': True}, 'Image': 'sha256:old'}
        with isolated_work(), patch.object(p, 'run', side_effect=docker.run), \
                patch.object(p, 'docker_api', side_effect=docker.api), patch.object(p, 'inspect', return_value=original), \
                patch.object(cr.release, 'clone', return_value={'Labels': {}}), \
                patch.object(cr, 'health', side_effect=RuntimeError('Health check failed: numbering')), \
                patch.object(cr.time, 'time_ns', return_value=7):
            entry = cr.begin('release', SHA, '99')
            with self.assertRaisesRegex(RuntimeError, 'Health check failed'):
                cr.deploy('numbering', 'sha256:new', SHA, '99', entry)
            journal = cr.load_journal()
        self.assertIn('cfmeet-numbering', docker.names)
        self.assertIn('cfmeet-numbering-failed-99-7', docker.names)
        self.assertNotIn('cfmeet-numbering-rollback-99', docker.names)
        renames = [c[2:] for c in docker.calls if c[:2] == ('docker', 'rename')]
        self.assertEqual(renames, [('cfmeet-numbering', 'cfmeet-numbering-rollback-99'),
                                   ('cfmeet-numbering', 'cfmeet-numbering-failed-99-7'),
                                   ('cfmeet-numbering-rollback-99', 'cfmeet-numbering')])
        self.assertIn(('docker', 'start', 'cfmeet-numbering'), docker.calls)
        self.assertEqual(journal[-1]['components'][0]['status'], 'rolled-back')
        self.assertFalse(any('cosmicforge-db' in c for c in docker.calls if c[:2] != ('docker', 'ps')))

    def test_new_service_is_retired_when_unhealthy(self):
        docker = FakeDocker(['cfmeet-platform-access'])
        with isolated_work(), patch.object(p, 'run', side_effect=docker.run), \
                patch.object(cr, 'template_env', return_value={'DATABASE_URL': 'postgres://o:secret@db/x'}), \
                patch.object(cr, 'health', side_effect=RuntimeError('Health check failed: organizations')), \
                patch.object(cr.time, 'time_ns', return_value=8):
            entry = cr.begin('release', SHA, '100')
            with self.assertRaises(RuntimeError):
                cr.deploy('organizations', 'sha256:new', SHA, '100', entry)
            env_file = cr.WORK / 'env' / 'cfmeet-organizations.env'
            self.assertEqual(env_file.stat().st_mode & 0o777, 0o600)
        self.assertNotIn('cfmeet-organizations', docker.names)
        self.assertIn('cfmeet-organizations-rolled-back-100-8', docker.names)
        create = next(c for c in docker.calls if c[:2] == ('docker', 'create'))
        self.assertNotIn('--publish', create)
        self.assertNotIn('-p', create)
        self.assertEqual(entry['components'][0]['status'], 'rolled-back')

    def test_failed_swap_before_health_is_also_restored(self):
        docker = FakeDocker(['cfmeet-calling'])

        def api(method, path, payload=None):
            raise AssertionError('Docker transaction rejected')

        with isolated_work(), patch.object(p, 'run', side_effect=docker.run), patch.object(p, 'docker_api', side_effect=api), \
                patch.object(cr, 'no_active_calls'), patch.object(cr.time, 'time_ns', return_value=9):
            entry = cr.begin('activate-x', SHA, '101')
            with self.assertRaises(AssertionError):
                cr.swap('calling', {}, '101', entry)
        self.assertEqual(docker.names, ['cfmeet-calling'])

    def test_irreversible_cutover_is_never_rolled_back(self):
        with isolated_work():
            entry = cr.begin('org-cutover-authoritative', SHA, '5', irreversible=True)
            entry['components'].append(dict(service='meeting-service', name=cr.MEETING, previous='x', status='released'))
            entry['status'] = 'done'
            cr.commit(entry)
            with patch.object(cr, 'restore') as restore:
                with self.assertRaisesRegex(RuntimeError, 'cannot be rolled back'):
                    cr.rollback(SHA, '6')
                restore.assert_not_called()


class Ownership(unittest.TestCase):
    def test_only_cf_meet_service_containers_are_owned(self):
        for name in ('cfmeet-platform-access', 'cfmeet-production-backend', 'cfmeet-organizations',
                     'cfmeet-developer-platform-worker', 'cfmeet-calling-rollback-123', 'cfmeet-api-gateway-failed-1-2',
                     'cfmeet-gateway-ingress', 'cfmeet-restore-test-123-meeting-service', 'cfmeet-restore-test-12-3-organizations'):
            self.assertTrue(cr.owned(name), name)
        for name in ('cosmicforge-backend', 'cosmicforge-db', 'cosmicforge-redis', 'cosmicforge-scan-gateway',
                     'cfmeet-staging-backend', 'cfmeet-staging-platform-access', 'lk-server', 'cfmeet-core-db',
                     'cfmeet-core-tls', 'cfmeet-messaging-ingress', 'cfmeet-storage-ingress', 'cfmeet-calling-redis',
                     'cfmeet-calling-egress', 'cfmeet-calling-loopback', 'cfmeet-calling-artifacts', 'cfmeet-calling-x'):
            self.assertFalse(cr.owned(name), name)
        self.assertTrue(cr.owned('cfmeet-core-db', exec_only=True))
        self.assertFalse(cr.owned('cosmicforge-db', exec_only=True))

    def test_lifecycle_helpers_refuse_foreign_containers_without_calling_docker(self):
        with patch.object(p, 'run') as run:
            for call in (lambda: cr.stop('cosmicforge-db'), lambda: cr.start('cfmeet-staging-backend'),
                         lambda: cr.remove('cosmicforge-redis'), lambda: cr.rename('cfmeet-calling', 'cosmicforge-backend'),
                         lambda: cr.rename('lk-server', 'cfmeet-calling'), lambda: cr.stop('cfmeet-core-db')):
                with self.assertRaisesRegex(RuntimeError, 'does not own'):
                    call()
            run.assert_not_called()

    def test_inspection_lists_cf_meet_production_containers_only(self):
        names = ['cfmeet-calling', 'cosmicforge-backend', 'cfmeet-staging-backend', 'lk-server', 'cosmicforge-db']
        self.assertEqual(cr.cfmeet_containers(names), ['cfmeet-calling', 'lk-server'])


class SecretsAreNeverPrinted(unittest.TestCase):
    ENV = ['DATABASE_URL=postgres://meeting:hunter2-db@cosmicforge-db:5432/meeting', 'JWT_SECRET=hunter2-jwt',
           'SMTP_HOST=smtp.example', 'SMTP_USERNAME=mailer', 'SMTP_PASSWORD=hunter2-mail',
           'ORGANIZATIONS_AUTHORITY_MODE=legacy', 'CALLING_PUBLIC_OUTBOUND_ENABLED=false',
           'MESSAGING_PUSH_PROVIDER=https://user:hunter2-push@push.example', 'API_GATEWAY_REDIS_URL=rediss://:hunter2-redis@r']

    def container(self):
        return {'Name': '/cfmeet-production-backend', 'State': {'Status': 'running'}, 'Image': 'sha256:x',
                'Config': {'Image': 'img', 'Env': list(self.ENV), 'Labels': {'org.opencontainers.image.revision': SHA}},
                'HostConfig': {'Memory': 0, 'RestartPolicy': {'Name': 'unless-stopped'}, 'PortBindings': {}},
                'NetworkSettings': {'Networks': {'cfmeet-core-private': {}}}}

    def test_flags_and_container_summaries_carry_no_secret_values(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            env = p.env_of(self.container())
            cr.emit('container', cr.summarize(self.container()))
            cr.emit('mail', cr.mail_presence(env))
            cr.emit('activated', {'flags': cr.safe_flags(env)})
        printed = out.getvalue()
        self.assertNotIn('hunter2', printed)
        self.assertIn('"ORGANIZATIONS_AUTHORITY_MODE": "legacy"', printed)
        self.assertIn('"MESSAGING_PUSH_PROVIDER": "present"', printed)
        self.assertIn('DATABASE_URL', printed)  # names only
        self.assertIn('"smtpCredentialsConfigured": true', printed)

    def test_failure_messages_are_scrubbed(self):
        self.assertNotIn('hunter2', cr.scrub('connection to postgres://u:hunter2@db:5432/x failed'))
        out = io.StringIO()
        with isolated_work(), contextlib.redirect_stdout(out), \
                patch.object(cr, 'inspect', side_effect=RuntimeError('x postgres://a:hunter2@h/d')):
            self.assertEqual(cr.main(['inspect', SHA, '1']), 1)
            self.assertEqual(cr.main(['no-such-operation', SHA, '1']), 1)
            self.assertEqual(cr.main(['inspect', 'not-a-sha', '1']), 1)
        self.assertIn('COORDINATED_RELEASE_FAILED: RuntimeError', out.getvalue())
        self.assertNotIn('hunter2', out.getvalue())

    def test_migration_and_dump_urls_reach_containers_only_through_private_env_files(self):
        calls = []
        with isolated_work(), patch.object(cr, 'migrator_target', return_value=('postgres://m:hunter2@db/x', 'net')), \
                patch.object(cr, 'run_binary', side_effect=lambda args, **k: calls.append(args) or b''):
            cr.migrate('organizations', 'sha256:new')
            self.assertFalse((cr.WORK / 'run' / 'organizations.migration.env').exists())
        self.assertTrue(calls)
        self.assertFalse(any('hunter2' in arg for call in calls for arg in call))
        self.assertTrue(all('--env-file' in call for call in calls))

    def test_a_failed_migration_reports_its_scrubbed_reason(self):
        """Production 2026-11-18: a Communications migration failed with only "subprocess failed:
        docker". The reason now travels, credentials and literal values masked."""
        failed = subprocess.CompletedProcess([], 1, b'', b"Error: Execution Error: error returned from database: "
                                             b"column \"x\" contains null values; url postgres://m:hunter2@db/x")
        with isolated_work(), patch.object(cr, 'migrator_target', return_value=('postgres://m:hunter2@db/x', 'net')), \
                patch.object(cr.subprocess, 'run', return_value=failed):
            with self.assertRaises(RuntimeError) as raised:
                cr.migrate('organizations', 'sha256:new')
        message = str(raised.exception)
        self.assertIn('[organizations-migration]', message)
        self.assertIn('contains null values', message)
        self.assertNotIn('hunter2', message)


class SafeDefaultsAndRouting(unittest.TestCase):
    def test_a_release_gives_calling_its_private_metrics_listener_once(self):
        # C4: call counters for cfmeet-prometheus; an operator's own value is kept.
        self.assertEqual(cr.release_env_defaults('calling', {}), {'CALLING_METRICS_ADDRESS': '0.0.0.0:9464'})
        self.assertEqual(cr.release_env_defaults('calling', {'CALLING_METRICS_ADDRESS': ' '}),
                         {'CALLING_METRICS_ADDRESS': '0.0.0.0:9464'})
        self.assertEqual(cr.release_env_defaults('calling', {'CALLING_METRICS_ADDRESS': '0.0.0.0:9500'}), {})
        self.assertEqual(cr.release_env_defaults('messaging', {}), {})
        import monitoring_stack
        target = monitoring_stack.listener_target(cr.SERVICES['calling']['container'],
                                                  cr.RELEASE_ENV_DEFAULTS['calling']['CALLING_METRICS_ADDRESS'])
        self.assertEqual(target, 'cfmeet-calling:9464')
        self.assertNotEqual(9464, cr.SERVICES['calling']['port'])

    def test_provider_dependent_features_must_stay_off(self):
        envs = {name: dict(cr.SERVICES[name]['env']) for name in cr.ORDER if not cr.SERVICES[name]['existing']}
        envs['messaging'] = {'MESSAGING_E2EE_DEVICES_ENABLED': 'false'}
        envs['meeting-service'] = {}
        self.assertEqual(cr.safe_default_violations(envs), [])
        envs['sms-gateway']['SMS_GATEWAY_PROVIDER'] = 'africastalking'
        envs['messaging']['MESSAGING_E2EE_DEVICES_ENABLED'] = 'true'
        envs['telephony']['TELEPHONY_ENABLED'] = 'true'
        envs['meeting-service']['NOTIFICATIONS_EMAIL_ENABLED'] = 'true'
        self.assertEqual(sorted(cr.safe_default_violations(envs)), [
            'meeting-service NOTIFICATIONS_EMAIL_ENABLED without mail credentials',
            'messaging MESSAGING_E2EE_DEVICES_ENABLED', 'messaging MESSAGING_E2EE_PRODUCTION_RELEASE_FILE',
            'sms-gateway SMS_GATEWAY_PROVIDER', 'telephony TELEPHONY_ENABLED'])
        envs['meeting-service'].update(SMTP_HOST='h', SMTP_USERNAME='u', SMTP_PASSWORD='p')
        self.assertNotIn('meeting-service NOTIFICATIONS_EMAIL_ENABLED without mail credentials',
                         cr.safe_default_violations(envs))

    def test_personal_e2ee_is_allowed_only_after_the_owner_activation_with_its_record(self):
        envs = {'messaging': {'MESSAGING_E2EE_DEVICES_ENABLED': 'true'}}
        with patch.object(cr, 'done', lambda op: False):
            self.assertIn('messaging MESSAGING_E2EE_DEVICES_ENABLED', cr.safe_default_violations(envs))
        with patch.object(cr, 'done', lambda op: op == 'activate-personal-encryption'):
            self.assertEqual(cr.safe_default_violations(envs), ['messaging MESSAGING_E2EE_PRODUCTION_RELEASE_FILE'])
            envs['messaging']['MESSAGING_E2EE_PRODUCTION_RELEASE_FILE'] = cr.E2EE_RELEASE_TARGET
            self.assertEqual(cr.safe_default_violations(envs), [])
            envs['messaging']['MESSAGING_E2EE_DEVICES_ENABLED'] = 'yes'
            self.assertIn('messaging MESSAGING_E2EE_DEVICES_ENABLED', cr.safe_default_violations(envs))

    def test_web_push_settings_are_allowed_only_after_activate_web_push_with_exact_values(self):
        envs = {'messaging': {'MESSAGING_PUSH_WEB_VAPID_KEY_FILE': cr.WEB_PUSH_KEY_TARGET,
                              'MESSAGING_PUSH_WEB_VAPID_SUBJECT': cr.WEB_PUSH_SUBJECT}}
        with patch.object(cr, 'done', lambda op: False):
            self.assertEqual(cr.safe_default_violations(envs), [
                'messaging MESSAGING_PUSH_WEB_VAPID_KEY_FILE', 'messaging MESSAGING_PUSH_WEB_VAPID_SUBJECT'])
        with patch.object(cr, 'done', lambda op: op == 'activate-web-push'):
            self.assertEqual(cr.safe_default_violations(envs), [])
            # Another key path, a widened host list or any APNs / FCM credential stays refused.
            other = {'messaging': dict(envs['messaging'], MESSAGING_PUSH_WEB_VAPID_KEY_FILE='/tmp/k',
                                       MESSAGING_PUSH_WEB_ALLOWED_HOSTS='evil.example',
                                       MESSAGING_PUSH_APNS_KEY_FILE='/a', MESSAGING_PUSH_FCM_SERVICE_ACCOUNT_FILE='/f')}
            self.assertEqual(cr.safe_default_violations(other), [
                'messaging MESSAGING_PUSH_APNS_KEY_FILE', 'messaging MESSAGING_PUSH_FCM_SERVICE_ACCOUNT_FILE',
                'messaging MESSAGING_PUSH_WEB_ALLOWED_HOSTS', 'messaging MESSAGING_PUSH_WEB_VAPID_KEY_FILE'])
        self.assertEqual(cr.ACTIVATIONS['activate-web-push'][0], ('activate-personal-encryption',))
        self.assertTrue(cr.WEB_PUSH_SUBJECT.startswith('https://'), 'VAPID contact is the public site, no address')

    def test_activate_web_push_refuses_without_e2ee_devices_or_with_provider_credentials(self):
        base = {'CF_RUNTIME_ENVIRONMENT': 'production', 'MESSAGING_E2EE_DEVICES_ENABLED': 'true'}
        for env, text in ((dict(base, MESSAGING_E2EE_DEVICES_ENABLED='false'), 'personal encryption'),
                          (dict(base, MESSAGING_PUSH_FCM_SERVICE_ACCOUNT_FILE='/f'), 'APNs/FCM'),
                          (dict(base, CF_RUNTIME_ENVIRONMENT='staging'), 'production runtime')):
            with patch.object(cr, 'env_of', lambda name, e=env: e), \
                    patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply')):
                with self.assertRaisesRegex(RuntimeError, text):
                    cr.activate_web_push('a' * 40, '1', '')

    def test_activate_web_push_mounts_the_generated_key_and_checks_the_adapter_started(self):
        import messaging_runtime
        calls = {}
        env = {'CF_RUNTIME_ENVIRONMENT': 'production', 'MESSAGING_E2EE_DEVICES_ENABLED': 'true'}
        key = Path('/opt/cfmeet-core/messaging/web-push-vapid.pem')

        def apply(op, sha, run_id, changes, post=None):
            calls.update(op=op, changes=changes, post=post)

        with patch.object(cr, 'env_of', lambda name: env), \
                patch.object(messaging_runtime, 'protected_file', return_value=key) as protected, \
                patch.object(cr.p, 'run', return_value='-----BEGIN PUBLIC KEY-----\nX\n-----END PUBLIC KEY-----'), \
                patch.object(cr, 'apply_activation', apply):
            cr.activate_web_push('a' * 40, '1', '')
        self.assertEqual(protected.call_args[0][0], 'web-push-vapid.pem')
        self.assertIs(protected.call_args[0][1], cr.vapid_private_key_pem)
        self.assertEqual(calls['op'], 'activate-web-push')
        service, overrides, extra = calls['changes'][0]
        self.assertEqual(service, 'messaging')
        self.assertEqual(overrides, {'MESSAGING_PUSH_WEB_VAPID_KEY_FILE': cr.WEB_PUSH_KEY_TARGET,
                                     'MESSAGING_PUSH_WEB_VAPID_SUBJECT': cr.WEB_PUSH_SUBJECT})
        self.assertNotIn('MESSAGING_PUSH_PROVIDER', overrides)
        self.assertEqual(extra['mounts'], [(str(key), cr.WEB_PUSH_KEY_TARGET)])
        log = '{"message":"push adapters","platforms":"[\\"WEB_PUSH\\"]"}'
        with patch.object(cr.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, log, '')):
            calls['post']()
        with patch.object(cr.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, '{"message":"push adapters","platforms":"[]"}', '')):
            with self.assertRaisesRegex(RuntimeError, 'Web Push adapter'):
                calls['post']()

    def test_the_public_key_check_runs_real_openssl_with_text_output(self):
        # Regression (activate-web-push run 43702218850): DER output is binary and p.run
        # decodes text, which raised UnicodeDecodeError on the VPS.
        with tempfile.TemporaryDirectory() as d:
            key = Path(d) / 'vapid.pem'
            key.write_text(cr.vapid_private_key_pem())
            self.assertTrue(cr.vapid_public_key_present(key))

    def test_the_vapid_key_is_a_pkcs8_p256_pem(self):
        pem = cr.vapid_private_key_pem()
        self.assertTrue(pem.startswith('-----BEGIN PRIVATE KEY-----'))
        out = subprocess.run(['openssl', 'pkey', '-noout', '-text'], input=pem, capture_output=True, text=True)
        self.assertEqual(out.returncode, 0)
        self.assertIn('P-256', out.stdout + out.stderr)

    def test_the_e2ee_release_record_states_the_actual_assessment_status(self):
        record = cr.e2ee_release_record('123', witnessed=False, now='2026-11-15T00:00:00Z')
        self.assertEqual(record['independentAssessment'], 'NOT_PERFORMED')
        self.assertEqual(record['scope'], 'PERSONAL_PRIVATE_E2EE')
        self.assertEqual(record['acceptedResidualRisks'], [
            'NO_INDEPENDENT_CRYPTOGRAPHIC_ASSESSMENT', 'KEY_TRANSPARENCY_UNWITNESSED', 'WEB_CODE_DELIVERY_TRUST'])
        self.assertNotIn('KEY_TRANSPARENCY_UNWITNESSED',
                         cr.e2ee_release_record('1', witnessed=True)['acceptedResidualRisks'])
        self.assertNotIn('assessmentReference', record)
        with self.assertRaisesRegex(RuntimeError, 'owner decision'):
            cr.activate_personal_e2ee('a' * 40, '1', '')
        self.assertEqual(cr.ACTIVATIONS['activate-personal-encryption'][0], ('activate-first-party-features',))

    def test_every_operation_and_confirmation_passes_the_workflow_input_patterns(self):
        # The workflows refuse names outside these patterns before reaching the VPS.
        import re
        for op in cr.OPERATIONS:
            self.assertRegex(op, r'^[a-z][a-z-]{1,40}$', op)
        self.assertRegex(cr.E2EE_CONFIRM, r'^[a-z,-]{0,120}$')
        workflow = (Path(__file__).resolve().parents[3] / '.github/workflows/cfmeet-calling-production.yml').read_text()
        options = re.search(r'options: \[([^\]]+)\]', workflow).group(1)
        for op in (o.strip() for o in options.split(',')):
            self.assertRegex(op, r'^[a-z][a-z-]{1,79}$', op)

    def test_new_services_are_private_bounded_and_dark(self):
        with tempfile.TemporaryDirectory() as folder, patch.object(p, 'SECRETS', Path(folder)):
            for name in cr.ORDER:
                s = cr.SERVICES[name]
                if s['existing']:
                    continue
                args = cr.create_args(name, 'sha256:new', '/x.env', SHA)
                self.assertNotIn('--publish', args)
                self.assertIn('--memory', args)
                self.assertIn('--read-only', args)
                self.assertEqual(args[args.index('--network') + 1], p.NET)
                self.assertIn('--env-file', args)
                for flag, value in s['env'].items():
                    if flag.endswith('_ENABLED'):
                        self.assertEqual(value, 'false', flag)
                if s['tls']:
                    self.assertGreaterEqual(s['tls'], 8450)

    def test_gateway_routes_only_developer_platform_public_families(self):
        manifest = json.loads((REPO / 'infra/api-gateway/route-manifest.json').read_text())
        families = {'/'.join(r['pattern'].split('/')[:3]) for r in manifest['routes']
                    if r['upstream'] == 'cfmeet.developer-platform'}
        # The Developer Platform families the CF Meet Platform activations add to the edge
        # (/v1/users; /v1/oauth and /v1/app-directory) are routed only by those operations.
        later = set(cr.platform_ops.USERS_EDGE) | set(cr.integrate_ops.OAUTH_EDGE + cr.integrate_ops.DIRECTORY_EDGE)
        self.assertEqual(set(cr.GATEWAY_PREFIXES), families - later)
        self.assertFalse(set(cr.GATEWAY_PREFIXES) & later)
        others = [r for r in manifest['routes'] if r['upstream'] != 'cfmeet.developer-platform'
                  and any(r['pattern'] == x or r['pattern'].startswith(x + '/') for x in cr.GATEWAY_PREFIXES)]
        self.assertEqual(others, [])
        snippet = cr.gateway_snippet()
        directives = '\n'.join(x for x in snippet.splitlines() if not x.startswith('#'))
        self.assertNotIn('/internal', directives)
        self.assertNotIn('/admin', directives)
        locations = re.findall(r'location (?:=|\^~) (\S+) \{', snippet)
        self.assertEqual(len(locations), 2 * len(cr.GATEWAY_PREFIXES))
        self.assertTrue(all(x.rstrip('/') in cr.GATEWAY_PREFIXES for x in locations))
        self.assertIn('proxy_pass http://127.0.0.1:%d;' % cr.GATEWAY_RELAY_PORT, snippet)

    def test_gateway_routing_refuses_to_shadow_existing_routes(self):
        self.assertEqual(cr.route_collisions(['location /v1/calls { }', 'location = /v1/projectsx { }']), [])
        self.assertEqual(cr.route_collisions(['location ^~ /v1/organizations/ {', 'location = /v1/scopes{']),
                         ['/v1/organizations', '/v1/scopes'])

    def test_private_tls_routes_are_idempotent_and_never_reuse_a_port(self):
        conf = 'http {\n  server { listen 8443 ssl; server_name api.cfmeet.com; }\n}\n'
        once = cr.tls_routes(conf)
        self.assertEqual(cr.tls_routes(once), once)
        self.assertIn('set $peer cfmeet-organizations:8101;', once)
        self.assertNotIn('cfmeet-api-gateway', once)
        self.assertTrue(once.endswith('}\n'))
        with self.assertRaisesRegex(RuntimeError, '8450'):
            cr.tls_routes(conf.replace('8443', '8450'))

    def test_first_party_features_never_touch_providers_e2ee_or_healthcare(self):
        changes = dict(cr.first_party_feature_changes({}))
        self.assertEqual(changes['messaging'], {'MESSAGING_SESSION_DEVICES_ENABLED': 'true'})
        self.assertNotIn('NOTIFICATIONS_EMAIL_ENABLED', changes['meeting-service'])
        self.assertNotIn('FEATURE_HEALTHCARE_PROFILE_ENABLED', changes['meeting-service'])
        with_mail = dict(cr.first_party_feature_changes({'SMTP_HOST': 'h', 'SMTP_USERNAME': 'u', 'SMTP_PASSWORD': 'p'}))
        self.assertEqual(with_mail['meeting-service']['NOTIFICATIONS_EMAIL_ENABLED'], 'true')
        envs = {'messaging': {'MESSAGING_E2EE_DEVICES_ENABLED': 'false', **changes['messaging']},
                'meeting-service': dict(changes['meeting-service'])}
        self.assertEqual(cr.safe_default_violations(envs), [])
        self.assertEqual(cr.ACTIVATIONS['activate-first-party-features'][0], ())

    def test_workflow_offers_exactly_these_operations(self):
        try:
            import yaml
        except ImportError:
            self.skipTest('PyYAML unavailable')
        workflow = yaml.safe_load((REPO / '.github/workflows/cfmeet-coordinated-release.yml').read_text())
        options = tuple((workflow.get('on') or workflow[True])['workflow_dispatch']['inputs']['operation']['options'])
        # The workflow is integrator-owned (docs/platform/build-internal-contracts.md §10): the CF Meet
        # Platform "Build" operations are either all offered, in this order, or (until the
        # integrator adds them) all absent. Every other operation must match exactly.
        platform = cr.platform_ops.OPERATIONS + (cr.platform_ops.ACCEPTANCE,)
        pending = tuple(op for op in platform if op not in options)
        self.assertIn(pending, ((), platform))
        # The CF Meet Apps operations (integrate_operations.py) likewise: all offered or all absent.
        integrate = cr.integrate_ops.OPERATIONS
        later = tuple(op for op in integrate if op not in options)
        self.assertIn(later, ((), integrate))
        self.assertEqual(options, tuple(op for op in cr.OPERATIONS if op not in pending + later))


class AdminBridge(unittest.TestCase):
    """HQ admin bridge (ADR 0068): dark deployment, least privilege, grants, HQ port."""

    def test_it_is_released_dark_with_no_owner_url(self):
        with patch.object(cr, 'runtime_url', lambda name: 'postgres://r@db/' + name):
            env = cr.template_env('admin-bridge')
        self.assertEqual(env['ADMIN_BRIDGE_ENABLED'], 'false')
        self.assertEqual(env['ADMIN_BRIDGE_WRITES_ENABLED'], 'false')
        modules = [k for k in env if k.startswith('ADMIN_BRIDGE_MODULE_')]
        self.assertEqual(len(modules), 17)
        self.assertTrue(all(env[k] == 'false' for k in modules))
        self.assertEqual([k for k in env if k.startswith('CF_') and k.endswith('_BASE_URL')], [])
        self.assertEqual(env['ADMIN_BRIDGE_OPS_BIND_ADDRESS'], '127.0.0.1:8190')
        s = cr.SERVICES['admin-bridge']
        self.assertEqual((s['port'], s['health'], s['tls'], s['db'], s['role']),
                         (8190, '/ready', cr.ADMIN_BRIDGE_EVENTS_TLS, 'cfmeet_admin_bridge', 'admin_bridge'))
        # Dark: no owner events listener (its private route answers 502 until activated).
        self.assertNotIn('ADMIN_BRIDGE_EVENTS_BIND_ADDRESS', env)
        self.assertEqual(cr.safe_default_violations({'admin-bridge': env}), [])

    def test_safe_defaults_keep_it_off_and_widen_only_the_activated_module(self):
        on = dict(cr.SERVICES['admin-bridge']['env'], ADMIN_BRIDGE_ENABLED='true')
        writes = dict(cr.SERVICES['admin-bridge']['env'], ADMIN_BRIDGE_WRITES_ENABLED='true')
        orgs = dict(cr.SERVICES['admin-bridge']['env'], ADMIN_BRIDGE_MODULE_ORGANIZATIONS='true')
        accounts = dict(cr.SERVICES['admin-bridge']['env'], ADMIN_BRIDGE_MODULE_ACCOUNTS='true')
        with patch.object(cr, 'done', lambda op: False):
            self.assertEqual(cr.safe_default_violations({'admin-bridge': on}), ['admin-bridge ADMIN_BRIDGE_ENABLED'])
            self.assertEqual(cr.safe_default_violations({'admin-bridge': writes}), ['admin-bridge ADMIN_BRIDGE_WRITES_ENABLED'])
            self.assertEqual(cr.safe_default_violations({'admin-bridge': orgs}),
                             ['admin-bridge ADMIN_BRIDGE_MODULE_ORGANIZATIONS'])
        with patch.object(cr, 'done', lambda op: op == 'activate-admin-bridge'):
            self.assertEqual(cr.safe_default_violations({'admin-bridge': orgs}), [])
            self.assertEqual(cr.safe_default_violations({'admin-bridge': accounts}),
                             ['admin-bridge ADMIN_BRIDGE_MODULE_ACCOUNTS'])
            # The bridge itself and commands stay off until their own later operation.
            self.assertEqual(cr.safe_default_violations({'admin-bridge': on}), ['admin-bridge ADMIN_BRIDGE_ENABLED'])
            self.assertEqual(cr.safe_default_violations({'admin-bridge': writes}), ['admin-bridge ADMIN_BRIDGE_WRITES_ENABLED'])

    def test_activation_grants_exactly_the_organizations_operator_reads(self):
        rows = [g for g in cr.GRANTS if g[0] == 'activate-admin-bridge']
        self.assertEqual(rows, [('activate-admin-bridge', 'organizations', 'admin-bridge',
                                 ('organization.administer', 'organization.operator-read'))])
        self.assertEqual(cr.validate_grants(permission_registry(), tuple(rows)), [])
        self.assertEqual(cr.service_id('admin-bridge'), 'cfmeet.admin-bridge')
        self.assertEqual(cr.ACTIVATIONS['activate-admin-bridge'][0], ())
        workflow = (REPO / '.github/workflows/cfmeet-coordinated-release.yml').read_text()
        self.assertIn('activate-admin-bridge,', workflow)
        self.assertIn("'admin-bridge.yml'", workflow)

    def test_activation_points_the_bridge_at_organizations_and_switches_only_that_module(self):
        applied = []
        env = {'CF_RUNTIME_ENVIRONMENT': 'production', 'CF_SERVICE_AUTH_KEY_ID': 'k', 'CF_SERVICE_AUTH_PRIVATE_KEY_FILE': '/f'}
        with patch.object(cr, 'env_of', lambda name: env), \
                patch.object(cr, 'tls_route_present', lambda port: None), \
                patch.object(cr, 'apply_activation', lambda op, sha, run, changes, **kw: applied.append((op, changes))):
            cr.activate_admin_bridge('a' * 40, '1', '')
        self.assertEqual(applied, [('activate-admin-bridge', [('admin-bridge', {
            'CF_ORGANIZATIONS_BASE_URL': cr.private_url(cr.SERVICES['organizations']['tls']),
            'ADMIN_BRIDGE_MODULE_ORGANIZATIONS': 'true'})])])
        with patch.object(cr, 'env_of', lambda name: dict(env, CF_RUNTIME_ENVIRONMENT='staging')), \
                patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply')):
            with self.assertRaisesRegex(RuntimeError, 'signing identity'):
                cr.activate_admin_bridge('a' * 40, '1', '')

    def test_runtime_role_is_narrowed_to_db_roles_sql_after_every_migration(self):
        sql = cr.admin_bridge_grants_sql()
        self.assertIn('REVOKE ALL ON ALL TABLES IN SCHEMA public FROM admin_bridge;', sql)
        self.assertIn('FOR ROLE admin_bridge_migrator IN SCHEMA public REVOKE ALL ON TABLES FROM admin_bridge;', sql)
        self.assertNotIn('{', sql.split('DO $$')[0])
        grants = [l for l in sql.splitlines() if l.startswith('GRANT ')]
        for line in grants:
            for table in ('admin_audit_entries', 'admin_revocations', 'admin_policy_revisions'):
                if table in line:
                    self.assertTrue(line.startswith('GRANT SELECT, INSERT ON'), line)
        self.assertFalse(any('TRUNCATE' in l for l in grants))
        # The A3.0 ledger, granted only once its migration exists, and never deletable.
        self.assertIn("IF to_regclass('public.admin_operations') IS NOT NULL THEN", sql)
        self.assertIn("EXECUTE 'GRANT SELECT, INSERT, UPDATE ON admin_operations TO admin_bridge'", sql)
        self.assertIn("OR (privilege_type = 'DELETE' AND table_name = 'admin_operations')", sql)
        self.assertTrue(sql.strip().startswith('BEGIN;') and sql.strip().endswith('COMMIT;'))
        calls = []
        with patch.object(cr, 'migrator_target', lambda name: ('postgres://m@db/x', 'net')), \
                patch.object(cr.release, 'env_file', lambda path, values: Path(tempfile.mkdtemp()) / 'e'), \
                patch.object(cr, 'run_binary', lambda args, step=None: calls.append(('migrate', step))), \
                patch.object(cr.release, 'db_sql', lambda db, text: calls.append(('sql', db, text))), \
                patch.object(cr, 'emit', lambda *a: None):
            cr.migrate('admin-bridge', 'sha256:x')
        self.assertEqual(calls[0], ('migrate', 'admin-bridge-migration'))
        self.assertEqual(calls[1], ('sql', 'cfmeet_admin_bridge', sql))

    def test_same_host_relay_joins_hq_network_at_the_agreed_address_and_publishes_nothing(self):
        # Agreed with HQ 2026-11-26: one VPS, no tunnel, no host port.
        config = cr.admin_bridge_hq_relay_config(cr.ADMIN_BRIDGE_HQ_SAME_HOST_SOURCES)
        self.assertIn('allow 10.231.14.3;\n        allow 10.231.14.4;\n        deny all;', config)
        self.assertNotIn('10.231.14.1', config, 'the Docker gateway is never admitted')
        self.assertNotIn('ssl', config, 'TLS passes straight through')
        args = cr.admin_bridge_hq_relay_args('img', '/w/relay.conf', None)
        self.assertNotIn('--publish', args)
        self.assertEqual(args[args.index('--network') + 1], 'cf-staging-cfmeet-bridge')
        self.assertEqual(args[args.index('--ip') + 1], '10.231.14.2')
        self.assertEqual(args[args.index('--cap-drop') + 1], 'ALL')
        with self.assertRaises(RuntimeError):
            cr.admin_bridge_hq_relay_config(())
        # HQ owns the network: it must exist, be internal and have exactly the agreed subnet.
        good = {'Internal': True, 'IPAM': {'Config': [{'Subnet': '10.231.14.0/28'}]}}
        cr.same_host_network_problems(['cf-staging-cfmeet-bridge'], lambda n: good)
        for bad_net in ({'Internal': False, 'IPAM': good['IPAM']},
                        {'Internal': True, 'IPAM': {'Config': [{'Subnet': '10.231.0.0/16'}]}},
                        {'Internal': True, 'IPAM': {'Config': []}}):
            with self.assertRaises(RuntimeError):
                cr.same_host_network_problems(['cf-staging-cfmeet-bridge'], lambda n, b=bad_net: b)
        with self.assertRaisesRegex(RuntimeError, 'HQ creates it first'):
            cr.same_host_network_problems(['cfmeet-private'], lambda n: good)

    def test_enable_record_selects_the_transport(self):
        base = {'policy_revision': 1, 'policy_key_id': 'cfmeet-policy-1', 'policy_public_key_hex': 'ab' * 32,
                'hq_workload_uri': 'cf://staging/product-admin/cfmeet', 'enrolled_markets': ['GLOBAL'],
                'enrolled_tenants': ['0b7e7f0e-2b9c-4c8e-9d7a-1f2e3d4c5b6a']}
        _, listen, source = cr.admin_bridge_enable_settings(dict(base, transport='same-host'))
        self.assertEqual((listen, source), (None, ('10.231.14.3', '10.231.14.4')))
        tunnel = dict(base, listen_address='10.250.0.1', allowed_source='10.250.0.2')
        self.assertEqual(cr.admin_bridge_enable_settings(tunnel)[1:], ('10.250.0.1', '10.250.0.2'))
        self.assertEqual(cr.admin_bridge_enable_settings(dict(tunnel, transport='tunnel'))[1:],
                         ('10.250.0.1', '10.250.0.2'))
        for bad in (dict(base), dict(base, transport='same-host', listen_address='10.250.0.1'),
                    dict(base, transport='public'), dict(base, transport='same-host', allowed_source='10.0.0.9')):
            with self.assertRaises(RuntimeError, msg=bad):
                cr.admin_bridge_enable_settings(bad)

    def test_hq_port_is_a_passthrough_relay_admitting_only_hq(self):
        config = cr.admin_bridge_hq_relay_config('10.77.0.2')
        self.assertIn('listen %d;' % cr.ADMIN_BRIDGE_HQ_PORT, config)
        self.assertIn('allow 10.77.0.2;\n        deny all;', config)
        self.assertIn('resolver 127.0.0.11', config, 'Docker DNS: the relay sits on the private network')
        self.assertIn('set $peer cfmeet-admin-bridge:8443;', config)
        self.assertNotIn('ssl', config, 'TLS passes straight through: the bridge terminates mTLS')
        args = cr.admin_bridge_hq_relay_args('img', '/w/relay.conf', '10.77.0.1')
        self.assertEqual(args[args.index('--publish') + 1], '10.77.0.1:%d:%d/tcp' % ((cr.ADMIN_BRIDGE_HQ_PORT,) * 2),
                         'published on the tunnel address only')
        self.assertEqual(args[args.index('--network') + 1], cr.ADMIN_BRIDGE_HQ_RELAY_NET)
        self.assertNotEqual(cr.ADMIN_BRIDGE_HQ_RELAY_NET, cr.p.NET, 'the private network cannot publish')
        for flag in ('--read-only', 'no-new-privileges'):
            self.assertIn(flag, args)
        self.assertEqual(args[args.index('--cap-drop') + 1], 'ALL')
        self.assertTrue(cr.owned(cr.ADMIN_BRIDGE_HQ_RELAY))
        # Owner decision 2026-11-20: private tunnel only; HQ never uses a public address.
        for bad_listen in ('', '0.0.0.0', '203.0.113.5', '10.77.0.0/24', '999.1.1.1', '100.64.0.1'):
            with self.assertRaises(RuntimeError, msg=bad_listen):
                cr.admin_bridge_hq_relay_args('img', '/w/relay.conf', bad_listen)
        for bad in ('', '0.0.0.0/0', '10.0.0.0/8', '203.0.113.7', '8.8.8.8/32', '999.1.1.1', 'any',
                    '10.77.0.2; allow all', '172.32.0.1', '192.169.1.1', '100.64.0.9'):
            with self.assertRaises(RuntimeError, msg=bad):
                cr.admin_bridge_hq_relay_config(bad)
        for ok in ('172.16.5.4', '192.168.1.0/24', '10.250.0.2'):
            cr.admin_bridge_hq_relay_config(ok)
        self.assertNotIn(cr.ADMIN_BRIDGE_HQ_PORT, [s['tls'] for s in cr.SERVICES.values() if s['tls']])
        routes = cr.tls_routes('http {\n  server { listen 9999 ssl; }\n}')
        self.assertNotIn(str(cr.ADMIN_BRIDGE_HQ_PORT), routes)
        # The bridge's only private route is its owner events listener, never HQ or ops.
        bridge = [line for line in routes.splitlines() if 'admin-bridge' in line]
        self.assertEqual(len(bridge), 1)
        self.assertIn('listen %d ssl;' % cr.ADMIN_BRIDGE_EVENTS_TLS, bridge[0])
        self.assertIn('set $peer cfmeet-admin-bridge:%d;' % cr.ADMIN_BRIDGE_EVENTS_PORT, bridge[0])
        self.assertNotIn(':8443;', bridge[0])
        self.assertNotIn(':8190;', bridge[0])


TENANT = '0f1e2d3c-4b5a-4968-8776-a5b4c3d2e1f0'
POLICY_KEY = 'ab' * 32
HMAC_TEXT = base64.urlsafe_b64encode(bytes(range(32))).decode().rstrip('=')


def enable_record(**changes):
    record = {'policy_revision': 1, 'policy_key_id': 'cfmeet-policy-1', 'policy_public_key_hex': POLICY_KEY,
              'hq_workload_uri': 'cf://staging/product-admin/cfmeet', 'enrolled_markets': ['GLOBAL'],
              'enrolled_tenants': [TENANT], 'listen_address': '10.250.0.1', 'allowed_source': '10.250.0.2'}
    record.update(changes)
    return record


class AdminBridgeOwners(unittest.TestCase):
    """ADR 0068: one activation per owner family the bridge reads and commands."""

    def test_each_owner_family_grants_only_the_permissions_its_bridge_routes_check(self):
        expected = {
            'activate-admin-bridge-notifications': {
                ('notifications', ('notifications.administer', 'notifications-observability.read')),
                ('notifications-2', ('notifications.administer', 'notifications-observability.read'))},
            'activate-admin-bridge-partners': {
                ('developer-platform', ('developer-platform.administer',)),
                ('platform-access', ('platform-identity.administer',))},
            'activate-admin-bridge-communications': {('communications', ('communications.administer',))},
            'activate-admin-bridge-meetings': {('meeting-service', ('account.administer', 'meeting.administer'))},
            'activate-admin-bridge-messaging': {('messaging', ('messaging-firstparty.administer',))},
            'activate-admin-bridge-calling': {('calling', ('calling.administer',))},
        }
        for op, rows in expected.items():
            got = {(g[1], g[3]) for g in cr.GRANTS if g[0] == op}
            self.assertEqual(got, rows, op)
            self.assertTrue(all(g[2] == 'admin-bridge' for g in cr.GRANTS if g[0] == op))
            self.assertEqual(cr.ACTIVATIONS[op][0], ('activate-admin-bridge',))
        bridge_rows = tuple(g for g in cr.GRANTS if g[2] == 'admin-bridge')
        self.assertEqual(cr.validate_grants(permission_registry(), bridge_rows), [])
        modules = [m for spec in cr.ADMIN_BRIDGE_OWNERS.values() for m in spec['modules']]
        self.assertEqual(len(modules), len(set(modules)), 'each module has one owner activation')
        self.assertTrue(set(modules) <= set(cr.ADMIN_BRIDGE_MODULES))
        workflow = (REPO / '.github/workflows/cfmeet-coordinated-release.yml').read_text()
        for op in expected:
            self.assertIn(op + ',', workflow)

    def test_an_owner_activation_points_the_bridge_at_the_owner_and_switches_its_modules(self):
        applied = []
        env = {'CF_RUNTIME_ENVIRONMENT': 'production', 'CF_SERVICE_AUTH_KEY_ID': 'k', 'CF_SERVICE_AUTH_PRIVATE_KEY_FILE': '/f'}
        routes = []
        with patch.object(cr, 'env_of', lambda name: env),                 patch.object(cr, 'tls_route_present', lambda port: routes.append(port)),                 patch.object(cr, 'apply_activation', lambda op, sha, run, changes, **kw: applied.append((op, changes))):
            cr.ACTIVATIONS['activate-admin-bridge-meetings'][1]('a' * 40, '1', '')
            cr.ACTIVATIONS['activate-admin-bridge-partners'][1]('a' * 40, '1', '')
        (op, changes), (op2, changes2) = applied
        self.assertEqual(op, 'activate-admin-bridge-meetings')
        self.assertEqual(changes, [
            ('meeting-service', {'ADMIN_OPERATOR_API_ENABLED': 'true'}),
            ('admin-bridge', {'CF_MEETING_SERVICE_BASE_URL': cr.private_url(cr.SERVICES['meeting-service']['tls']),
                              'ADMIN_BRIDGE_MODULE_ACCOUNTS': 'true', 'ADMIN_BRIDGE_MODULE_MEETINGS': 'true'})])
        self.assertEqual(changes2, [('admin-bridge', {
            'CF_DEVELOPER_PLATFORM_BASE_URL': cr.private_url(cr.SERVICES['developer-platform']['tls']),
            'CF_PLATFORM_ACCESS_BASE_URL': cr.private_url(cr.SERVICES['platform-access']['tls']),
            'ADMIN_BRIDGE_MODULE_PARTNERS': 'true', 'ADMIN_BRIDGE_MODULE_IDENTITY': 'true',
            'ADMIN_BRIDGE_MODULE_APPS': 'true', 'ADMIN_BRIDGE_MODULE_VERIFICATION': 'true'})])
        self.assertEqual(set(routes), {cr.SERVICES[o]['tls'] for o in ('meeting-service', 'developer-platform', 'platform-access')})
        # Never the master switch or writes.
        for _, ch in applied:
            for _, overrides in ch:
                self.assertNotIn('ADMIN_BRIDGE_ENABLED', overrides)
                self.assertNotIn('ADMIN_BRIDGE_WRITES_ENABLED', overrides)
        with patch.object(cr, 'env_of', lambda name: dict(env, CF_RUNTIME_ENVIRONMENT='staging')),                 patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply')):
            with self.assertRaisesRegex(RuntimeError, 'signing identity'):
                cr.ACTIVATIONS['activate-admin-bridge-calling'][1]('a' * 40, '1', '')

    def test_a_module_may_be_on_only_after_its_owner_activation(self):
        calling = dict(cr.SERVICES['admin-bridge']['env'], ADMIN_BRIDGE_MODULE_CALLING='true')
        with patch.object(cr, 'done', lambda op: op == 'activate-admin-bridge'):
            self.assertEqual(cr.safe_default_violations({'admin-bridge': calling}), ['admin-bridge ADMIN_BRIDGE_MODULE_CALLING'])
        with patch.object(cr, 'done', lambda op: op in ('activate-admin-bridge', 'activate-admin-bridge-calling')):
            self.assertEqual(cr.safe_default_violations({'admin-bridge': calling}), [])
            messaging = dict(calling, ADMIN_BRIDGE_MODULE_MESSAGING='true')
            self.assertEqual(cr.safe_default_violations({'admin-bridge': messaging}), ['admin-bridge ADMIN_BRIDGE_MODULE_MESSAGING'])


class AdminBridgeHq(unittest.TestCase):
    """Runbook §3: activate-admin-bridge-hq switches the bridge on for HQ, read-only."""

    def test_settings_are_exactly_what_the_bridge_and_hq_accept(self):
        overrides, listen, source = cr.admin_bridge_enable_settings(enable_record())
        self.assertEqual((listen, source), ('10.250.0.1', '10.250.0.2'))
        self.assertEqual(overrides['ADMIN_BRIDGE_ENABLED'], 'true')
        self.assertEqual(overrides['ADMIN_BRIDGE_WRITES_ENABLED'], 'false', 'reads only')
        self.assertEqual(overrides['ADMIN_BRIDGE_ENROLLED_TENANTS'], TENANT)
        self.assertEqual(overrides['ADMIN_BRIDGE_HMAC_KEY_DIR'], cr.ADMIN_BRIDGE_HMAC_TARGET)
        self.assertTrue(overrides['ADMIN_BRIDGE_TLS_KEY_FILE'].startswith(cr.ADMIN_BRIDGE_TLS_TARGET + '/'))
        self.assertFalse([k for k in overrides if k.startswith('ADMIN_BRIDGE_MODULE_')], 'modules belong to their own activations')
        bad = [dict(policy_revision=0), dict(policy_revision='1'), dict(policy_revision=True),
               dict(policy_public_key_hex='AB' * 32), dict(policy_public_key_hex='ab' * 31),
               dict(policy_key_id='bad id'), dict(hq_workload_uri='cf://staging/product-admin/other'),
               dict(hq_workload_uri='https://hq'), dict(enrolled_markets=[]), dict(enrolled_markets=['global']),
               dict(enrolled_tenants=['cfmeet-first-party']), dict(enrolled_tenants=[TENANT.upper()]),
               dict(enrolled_tenants=[TENANT, TENANT]), dict(enrolled_tenants=TENANT),
               dict(listen_address='0.0.0.0'), dict(listen_address='100.64.0.1'),
               dict(allowed_source='0.0.0.0/0'), dict(allowed_source='203.0.113.9')]
        for change in bad:
            with self.assertRaises(RuntimeError, msg=change):
                cr.admin_bridge_enable_settings(enable_record(**change))
        with self.assertRaisesRegex(RuntimeError, 'exactly'):
            cr.admin_bridge_enable_settings(dict(enable_record(), writes_enabled=True))
        with self.assertRaisesRegex(RuntimeError, 'exactly'):
            record = enable_record()
            del record['allowed_source']
            cr.admin_bridge_enable_settings(record)

    def test_hmac_key_file_decodes_like_the_bridge(self):
        self.assertEqual(cr.hmac_key_bytes(HMAC_TEXT + '\n'), bytes(range(32)))
        self.assertEqual(cr.hmac_key_bytes(base64.urlsafe_b64encode(bytes(range(40))).decode()), bytes(range(40)))
        for bad in ('', 'c2hvcnQ', HMAC_TEXT + '+', '\x00' * 40, HMAC_TEXT[:40]):
            self.assertIsNone(cr.hmac_key_bytes(bad), bad)

    def test_material_must_be_sealed_and_owned_by_the_bridge(self):
        if not hasattr(os, 'getuid'):
            self.skipTest('POSIX ownership')
        root = Path(tempfile.mkdtemp())
        for d in ('tls', 'hmac'):
            (root / d).mkdir(mode=0o750)
        for name in cr.ADMIN_BRIDGE_TLS_FILES:
            (root / 'tls' / name).write_text('x')
            (root / 'tls' / name).chmod(0o400)
        (root / 'hmac' / 'hq-hmac-1.key').write_text(HMAC_TEXT + '\n')
        (root / 'hmac' / 'hq-hmac-1.key').chmod(0o400)
        uid = os.getuid()  # the test owns the files; production requires the bridge's uid 10001
        with patch.object(cr, '_sealed', lambda path, mask: ORIGINAL_SEALED(path, mask, uid)):
            digests = cr.admin_bridge_material(root)
            self.assertEqual(digests, {'hq-hmac-1': hashlib.sha256(bytes(range(32))).hexdigest()})
            (root / 'hmac' / 'hq-hmac-1.key').chmod(0o440)
            with self.assertRaisesRegex(RuntimeError, 'too permissive'):
                cr.admin_bridge_material(root)
            (root / 'hmac' / 'hq-hmac-1.key').chmod(0o400)
            (root / 'hmac' / 'notes.txt').write_text('x')
            with self.assertRaisesRegex(RuntimeError, 'only <X-HQ-Key-ID>.key'):
                cr.admin_bridge_material(root)
            (root / 'hmac' / 'notes.txt').unlink()
            (root / 'hmac' / 'raw.key').write_bytes(os.urandom(32))
            (root / 'hmac' / 'raw.key').chmod(0o400)
            with self.assertRaisesRegex(RuntimeError, 'base64url'):
                cr.admin_bridge_material(root)
            (root / 'hmac' / 'raw.key').unlink()
            (root / 'tls' / 'server.key').unlink()
            (root / 'tls' / 'server.key').symlink_to(root / 'tls' / 'server.crt')
            with self.assertRaisesRegex(RuntimeError, 'symbolic link'):
                cr.admin_bridge_material(root)
        with self.assertRaisesRegex(RuntimeError, 'owned by uid'):
            ORIGINAL_SEALED(root / 'tls' / 'server.crt', 0o022, uid + 1)

    def test_hq_is_admitted_only_when_fully_enrolled(self):
        digest = hashlib.sha256(bytes(range(32))).hexdigest()
        good = {'certs': 1, 'staffKeys': 1, 'hmac': {'hq-hmac-1': digest}, 'policies': {'1': 'cfmeet-policy-1'}}
        check = lambda state: cr.admin_bridge_enrolment_problems(state, {'hq-hmac-1': digest}, 1, 'cfmeet-policy-1')
        self.assertEqual(check(good), [])
        self.assertIn('enroll-cert', check(dict(good, certs=0))[0])
        self.assertIn('enroll-staff-key', check(dict(good, staffKeys=0))[0])
        self.assertIn('enroll-hmac', check(dict(good, hmac={'hq-hmac-1': '0' * 64}))[0])
        self.assertIn('enroll-hmac', check(dict(good, hmac={}))[0])
        self.assertIn('policy-put', check(dict(good, policies={}))[0])
        self.assertIn('another key id', check(dict(good, policies={'1': 'other'}))[0])
        self.assertIn('newer', check(dict(good, policies={'1': 'cfmeet-policy-1', '2': 'cfmeet-policy-1'}))[0])
        sql = cr.ADMIN_BRIDGE_ENROLMENT_SQL
        self.assertEqual(sql.count('revoked_at IS NULL'), 3)
        self.assertNotIn('%(', sql)

    def test_it_needs_the_owner_confirmation_and_refuses_before_changing_anything(self):
        with patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply')):
            with self.assertRaisesRegex(RuntimeError, 'confirm=hq-enrolled-read-only'):
                cr.activate_admin_bridge_hq('a' * 40, '1', '')
        env = {'CF_RUNTIME_ENVIRONMENT': 'production', 'CF_SERVICE_AUTH_KEY_ID': 'k', 'CF_SERVICE_AUTH_PRIVATE_KEY_FILE': '/f'}
        root = Path(tempfile.mkdtemp())
        (root / 'enable.json').write_text(json.dumps(enable_record()))
        digest = hashlib.sha256(bytes(range(32))).hexdigest()
        state = {'certs': 1, 'staffKeys': 0, 'hmac': {'hq-hmac-1': digest}, 'policies': {'1': 'cfmeet-policy-1'}}
        with patch.object(cr, 'env_of', lambda name: env), patch.object(cr, 'ADMIN_BRIDGE_ROOT', root), \
                patch.object(cr, 'admin_bridge_material', lambda: {'hq-hmac-1': digest}), \
                patch.object(cr.release, 'db_sql', lambda db, sql: json.dumps(state)), \
                patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply')):
            with self.assertRaisesRegex(RuntimeError, 'not ready to be admitted.*enroll-staff-key'):
                cr.activate_admin_bridge_hq('a' * 40, '1', cr.ADMIN_BRIDGE_HQ_CONFIRM)

    def test_it_recreates_the_bridge_with_mounts_then_installs_the_relay(self):
        env = {'CF_RUNTIME_ENVIRONMENT': 'production', 'CF_SERVICE_AUTH_KEY_ID': 'k', 'CF_SERVICE_AUTH_PRIVATE_KEY_FILE': '/f'}
        root = Path(tempfile.mkdtemp())
        (root / 'enable.json').write_text(json.dumps(enable_record()))
        digest = hashlib.sha256(bytes(range(32))).hexdigest()
        state = {'certs': 1, 'staffKeys': 1, 'hmac': {'hq-hmac-1': digest}, 'policies': {'1': 'cfmeet-policy-1'}}
        applied, events = [], []

        def apply(op, sha, run, changes, post=None, **kw):
            applied.append((op, changes))
            post()

        with patch.object(cr, 'env_of', lambda name: env), patch.object(cr, 'ADMIN_BRIDGE_ROOT', root), \
                patch.object(cr, 'admin_bridge_material', lambda: {'hq-hmac-1': digest}), \
                patch.object(cr.release, 'db_sql', lambda db, sql: json.dumps(state)), \
                patch.object(cr, 'apply_activation', apply), patch.object(cr, 'emit', lambda *a: None), \
                patch.object(cr, 'admin_bridge_ready_document', lambda: {'enabled': True, 'policy_revision': 1}), \
                patch.object(cr, 'install_admin_bridge_hq_relay', lambda l, s: events.append(('relay', l, s))), \
                patch.object(cr, 'remove_admin_bridge_hq_relay', lambda: events.append('removed')):
            cr.activate_admin_bridge_hq('a' * 40, '1', cr.ADMIN_BRIDGE_HQ_CONFIRM)
            (op, [(service, overrides, extra)]), = applied
            self.assertEqual((op, service), ('activate-admin-bridge-hq', 'admin-bridge'))
            self.assertEqual(overrides['ADMIN_BRIDGE_ENABLED'], 'true')
            self.assertEqual(extra['mounts'], [(str(root / 'tls'), cr.ADMIN_BRIDGE_TLS_TARGET),
                                               (str(root / 'hmac'), cr.ADMIN_BRIDGE_HMAC_TARGET)])
            self.assertEqual(events, [('relay', '10.250.0.1', '10.250.0.2')])
            # Not enabled on the pinned revision: the relay is never left behind.
            events.clear()
            with patch.object(cr, 'admin_bridge_ready_document', lambda: {'enabled': True, 'policy_revision': None}):
                with self.assertRaisesRegex(RuntimeError, 'policy revision 1'):
                    cr.activate_admin_bridge_hq('a' * 40, '1', cr.ADMIN_BRIDGE_HQ_CONFIRM)
            self.assertEqual(events, ['removed'])

    def test_registry_safe_defaults_rollback_and_workflow(self):
        self.assertEqual(cr.ACTIVATIONS['activate-admin-bridge-hq'][0], ('activate-admin-bridge',))
        on = dict(cr.SERVICES['admin-bridge']['env'], ADMIN_BRIDGE_ENABLED='true')
        writes = dict(on, ADMIN_BRIDGE_WRITES_ENABLED='true')
        with patch.object(cr, 'done', lambda op: op in ('activate-admin-bridge', 'activate-admin-bridge-hq')):
            self.assertEqual(cr.safe_default_violations({'admin-bridge': on}), [])
            self.assertEqual(cr.safe_default_violations({'admin-bridge': writes}), ['admin-bridge ADMIN_BRIDGE_WRITES_ENABLED'])
        source = (REPO / 'infra/calling/production/coordinated_release.py').read_text(encoding='utf-8')
        rollback = source[source.index('def rollback(sha, run_id):'):source.index('# --- activation steps')]
        self.assertLess(rollback.index('remove_admin_bridge_hq_relay()'), rollback.index('restore(component, run_id)'),
                        'HQ loses its way in before the bridge goes dark')
        workflow = (REPO / '.github/workflows/cfmeet-coordinated-release.yml').read_text()
        self.assertIn('activate-admin-bridge-hq,', workflow)
        self.assertIn('activate-admin-bridge-hq: hq-enrolled-read-only', workflow)


class AdminBridgeHqCommands(unittest.TestCase):
    """HQ stage S6: commands on at a newer signed policy revision."""

    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        (self.root / 'enable.json').write_text(json.dumps(enable_record(policy_revision=2)))
        self.env = {'CF_RUNTIME_ENVIRONMENT': 'production', 'CF_SERVICE_AUTH_KEY_ID': 'k',
                    'CF_SERVICE_AUTH_PRIVATE_KEY_FILE': '/f', 'ADMIN_BRIDGE_ENABLED': 'true',
                    'ADMIN_BRIDGE_POLICY_REVISION': '1', 'ADMIN_BRIDGE_POLICY_KEY_ID': 'cfmeet-policy-1',
                    'ADMIN_BRIDGE_POLICY_PUBLIC_KEY_HEX': POLICY_KEY}
        digest = hashlib.sha256(bytes(range(32))).hexdigest()
        self.digest = digest
        self.state = {'certs': 1, 'staffKeys': 1, 'hmac': {'hq-hmac-1': digest},
                      'policies': {'1': 'cfmeet-policy-1', '2': 'cfmeet-policy-1'}}
        self.ledger = 't'

    def run_it(self, confirm=None, ready=None):
        applied = []

        def apply(op, sha, run, changes, post=None, **kw):
            applied.append((op, changes))
            post()

        def sql(db, text):
            return self.ledger if 'to_regclass' in text else json.dumps(self.state)

        with patch.object(cr, 'env_of', lambda name: self.env), patch.object(cr, 'ADMIN_BRIDGE_ROOT', self.root), \
                patch.object(cr, 'admin_bridge_material', lambda: {'hq-hmac-1': self.digest}), \
                patch.object(cr.release, 'db_sql', sql), patch.object(cr, 'emit', lambda *a: None), \
                patch.object(cr, 'apply_activation', apply), \
                patch.object(cr, 'admin_bridge_ready_document',
                             lambda: ready or {'enabled': True, 'policy_revision': 2}):
            cr.activate_admin_bridge_hq_commands('a' * 40, '1', confirm or cr.ADMIN_BRIDGE_COMMANDS_CONFIRM)
        return applied

    def test_it_turns_writes_on_at_the_newer_revision_only(self):
        applied = self.run_it()
        self.assertEqual(applied, [('activate-admin-bridge-hq-commands', [(
            'admin-bridge', {'ADMIN_BRIDGE_WRITES_ENABLED': 'true', 'ADMIN_BRIDGE_POLICY_REVISION': '2'})])])
        self.assertEqual(cr.ACTIVATIONS['activate-admin-bridge-hq-commands'][0], ('activate-admin-bridge-hq',))
        on = dict(cr.SERVICES['admin-bridge']['env'], ADMIN_BRIDGE_ENABLED='true', ADMIN_BRIDGE_WRITES_ENABLED='true')
        with patch.object(cr, 'done', lambda op: op in ('activate-admin-bridge-hq', 'activate-admin-bridge-hq-commands')):
            self.assertEqual(cr.safe_default_violations({'admin-bridge': on}), [])
        workflow = (REPO / '.github/workflows/cfmeet-coordinated-release.yml').read_text()
        self.assertIn('activate-admin-bridge-hq-commands: hq-commands-two-person', workflow)

    def test_it_refuses_before_changing_anything(self):
        with self.assertRaisesRegex(RuntimeError, 'confirm=hq-commands-two-person'):
            self.run_it(confirm='yes')
        self.env['ADMIN_BRIDGE_POLICY_REVISION'] = '2'
        with self.assertRaisesRegex(RuntimeError, 'not newer'):
            self.run_it()
        self.env['ADMIN_BRIDGE_POLICY_REVISION'] = '1'
        self.ledger = 'f'
        with self.assertRaisesRegex(RuntimeError, 'operations ledger'):
            self.run_it()
        self.ledger = 't'
        self.state['policies'] = {'1': 'cfmeet-policy-1'}
        with self.assertRaisesRegex(RuntimeError, 'policy-put'):
            self.run_it()
        self.state['policies'] = {'1': 'cfmeet-policy-1', '2': 'cfmeet-policy-1'}
        self.env['ADMIN_BRIDGE_ENABLED'] = 'false'
        with self.assertRaisesRegex(RuntimeError, 'activate-admin-bridge-hq first'):
            self.run_it()
        self.env['ADMIN_BRIDGE_ENABLED'] = 'true'
        self.env['ADMIN_BRIDGE_POLICY_KEY_ID'] = 'other'
        with self.assertRaisesRegex(RuntimeError, 'signing key differs'):
            self.run_it()
        self.env['ADMIN_BRIDGE_POLICY_KEY_ID'] = 'cfmeet-policy-1'
        with self.assertRaisesRegex(RuntimeError, 'policy revision 2'):
            self.run_it(ready={'enabled': True, 'policy_revision': 1})


SIGNED = {'CF_RUNTIME_ENVIRONMENT': 'production', 'CF_SERVICE_AUTH_KEY_ID': 'k', 'CF_SERVICE_AUTH_PRIVATE_KEY_FILE': '/f'}


def capture_apply(applied, run_post=True):
    def apply(op, sha, run, changes, post=None, **kw):
        applied.append((op, changes))
        if post and run_post:
            post()
    return apply


class AdminBridgeEvents(unittest.TestCase):
    """ADR 0068 A7: owner events to the bridge's private events listener."""

    def setUp(self):
        self.envs = {name: dict(SIGNED) for name in cr.SERVICES}
        self.envs['admin-bridge'].update(ADMIN_BRIDGE_ENABLED='true', CF_SERVICE_TRUST_BUNDLE_FILE='/run/secrets/trust.json')
        self.tables = {'devplat_bridge_events', 'messaging_admin_bridge_events', 'calling_admin_bridge_events',
                       'communication_admin_bridge_events'}
        self.status = '401'

    def run_it(self):
        applied, routes = [], []
        with patch.object(cr, 'env_of', lambda name: self.envs[name]), \
                patch.object(cr, 'tls_route_present', lambda port: routes.append(port)), \
                patch.object(cr, 'table_present', lambda service, table: table in self.tables), \
                patch.object(cr.notif_ops, 'private_status', lambda cr_, url: self.status), \
                patch.object(cr, 'emit', lambda *a: None), patch.object(cr, 'apply_activation', capture_apply(applied)):
            cr.activate_admin_bridge_events(SHA, '1', '')
        return applied, routes

    def test_the_bridge_receives_hq_bridge_events_publish_from_each_publisher(self):
        rows = [g for g in cr.GRANTS if g[0] == 'activate-admin-bridge-events']
        self.assertEqual({(g[1], g[2], g[3]) for g in rows},
                         {('admin-bridge', issuer, ('hq-bridge-events.publish',))
                          for issuer in ('developer-platform', 'messaging', 'calling', 'communications')})
        self.assertEqual(cr.validate_grants(permission_registry(), tuple(rows)), [])
        self.assertEqual(cr.ACTIVATIONS['activate-admin-bridge-events'][0], ('activate-admin-bridge-hq',))
        workflow = (REPO / '.github/workflows/cfmeet-coordinated-release.yml').read_text()
        self.assertIn('activate-admin-bridge-events,', workflow)

    def test_it_starts_the_listener_and_points_every_publisher_at_the_private_route(self):
        applied, routes = self.run_it()
        url = cr.private_url(cr.ADMIN_BRIDGE_EVENTS_TLS)
        self.assertEqual(url, 'https://api.cfmeet.com:8457')
        self.assertEqual(routes, [cr.ADMIN_BRIDGE_EVENTS_TLS])
        (op, changes), = applied
        self.assertEqual(op, 'activate-admin-bridge-events')
        self.assertEqual(changes, [
            ('admin-bridge', {'ADMIN_BRIDGE_EVENTS_BIND_ADDRESS': '0.0.0.0:8191'}),
            ('developer-platform', {'CF_ADMIN_BRIDGE_BASE_URL': url}),
            ('developer-platform-worker', {'CF_ADMIN_BRIDGE_BASE_URL': url}),
            ('messaging', {'CF_ADMIN_BRIDGE_BASE_URL': url}),
            ('calling', {'CF_ADMIN_BRIDGE_BASE_URL': url}),
            ('communications', {'CF_ADMIN_BRIDGE_BASE_URL': url})])
        for _, overrides in changes:
            self.assertFalse({'ADMIN_BRIDGE_ENABLED', 'ADMIN_BRIDGE_WRITES_ENABLED'} & set(overrides))
        # The listener must answer an unsigned post with 401 through the route, or it is undone.
        self.status = '502'
        with self.assertRaisesRegex(RuntimeError, 'answered 502'):
            self.run_it()

    def test_it_refuses_before_changing_anything(self):
        self.envs['admin-bridge']['ADMIN_BRIDGE_ENABLED'] = 'false'
        with patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply')):
            with self.assertRaisesRegex(RuntimeError, 'activate-admin-bridge-hq first'):
                self.run_it()
        self.envs['admin-bridge']['ADMIN_BRIDGE_ENABLED'] = 'true'
        self.tables.discard('calling_admin_bridge_events')
        with self.assertRaisesRegex(RuntimeError, 'calling has no calling_admin_bridge_events'):
            self.run_it()
        self.tables.add('calling_admin_bridge_events')
        self.envs['communications']['CF_RUNTIME_ENVIRONMENT'] = 'staging'
        with self.assertRaisesRegex(RuntimeError, 'communications lacks its production signing identity'):
            self.run_it()

    def test_safe_defaults_refuse_the_listener_and_the_urls_until_recorded(self):
        url = cr.private_url(cr.ADMIN_BRIDGE_EVENTS_TLS)
        bridge = dict(cr.SERVICES['admin-bridge']['env'], ADMIN_BRIDGE_EVENTS_BIND_ADDRESS='0.0.0.0:8191')
        envs = {'admin-bridge': bridge, 'developer-platform-worker': {'CF_ADMIN_BRIDGE_BASE_URL': url},
                'communications': {'CF_ADMIN_BRIDGE_BASE_URL': url}}
        with patch.object(cr, 'done', lambda op: op in ('activate-admin-bridge', 'activate-admin-bridge-hq')):
            self.assertEqual(sorted(cr.safe_default_violations(envs)),
                             ['admin-bridge ADMIN_BRIDGE_EVENTS_BIND_ADDRESS',
                              'communications CF_ADMIN_BRIDGE_BASE_URL',
                              'developer-platform-worker CF_ADMIN_BRIDGE_BASE_URL'])
        with patch.object(cr, 'done', lambda op: op == 'activate-admin-bridge-events'):
            self.assertEqual(cr.safe_default_violations(envs), [])
            # Exactly the private route: never another address.
            self.assertEqual(cr.safe_default_violations({'calling': {'CF_ADMIN_BRIDGE_BASE_URL': 'http://x:8191'}}),
                             ['calling CF_ADMIN_BRIDGE_BASE_URL'])

    def test_certification_probes_the_events_route_not_the_ops_readiness(self):
        self.assertEqual(cr.SERVICES['admin-bridge']['probe'], ('/internal/v1/hq-bridge/events', 405))
        self.assertEqual(cr.SERVICES['organizations']['probe'], ('/ready', 200))
        self.assertEqual(cr.SERVICES['organizations']['upstream'], 8101)


class PlanEnforcement(unittest.TestCase):
    """ADR 0068 A6: Meeting and Messaging enforce CF Meet plan limits after the activation."""

    def setUp(self):
        self.envs = {name: dict(SIGNED) for name in cr.SERVICES}
        self.dp = cr.private_url(cr.SERVICES['developer-platform']['tls'])
        self.envs['messaging']['CF_DEVELOPER_PLATFORM_BASE_URL'] = self.dp

    def run_it(self, confirm=cr.PLAN_ENFORCEMENT_CONFIRM):
        applied = []
        with patch.object(cr, 'env_of', lambda name: self.envs[name]), \
                patch.object(cr, 'tls_route_present', lambda port: None), \
                patch.object(cr, 'apply_activation', capture_apply(applied)):
            cr.activate_plan_enforcement(SHA, '1', confirm)
        return applied

    def test_the_developer_platform_receives_developer_entitlement_read_from_both_enforcers(self):
        rows = [g for g in cr.GRANTS if g[0] == 'activate-plan-enforcement']
        self.assertEqual({(g[1], g[2], g[3]) for g in rows},
                         {('developer-platform', 'meeting-service', ('developer-entitlement.read',)),
                          ('developer-platform', 'messaging', ('developer-entitlement.read',))})
        registry = permission_registry()
        # Received by the Developer Platform. Grantability to the two enforcers comes with the
        # Developer Platform's A6 registry change (checked for every grant in the Grants test).
        self.assertEqual(registry['developer-entitlement.read'][0], 'cfmeet.developer-platform')
        errors = cr.validate_grants(registry, tuple(rows))
        self.assertTrue(all('may not be granted to' in e for e in errors), errors)
        self.assertEqual(cr.ACTIVATIONS['activate-plan-enforcement'][0], ('activate-developer-platform',))
        workflow = (REPO / '.github/workflows/cfmeet-coordinated-release.yml').read_text()
        self.assertIn('activate-plan-enforcement,', workflow)
        self.assertIn('activate-plan-enforcement: plan-limits-reviewed', workflow)

    def test_it_switches_enforcement_on_and_sets_the_url_only_where_missing(self):
        (op, changes), = self.run_it()
        self.assertEqual(op, 'activate-plan-enforcement')
        self.assertEqual(changes, [
            ('meeting-service', {'MEETING_PLAN_ENFORCEMENT': 'on', 'MEETING_PLAN_CACHE_SECONDS': '60',
                                 'CF_DEVELOPER_PLATFORM_BASE_URL': self.dp}),
            ('messaging', {'MESSAGING_PLAN_ENFORCEMENT': 'on', 'MESSAGING_PLAN_CACHE_SECONDS': '60'})])

    def test_it_refuses_before_changing_anything(self):
        with patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply')):
            with self.assertRaisesRegex(RuntimeError, 'confirm=plan-limits-reviewed'):
                cr.activate_plan_enforcement(SHA, '1', '')
        self.envs['messaging']['CF_DEVELOPER_PLATFORM_BASE_URL'] = 'https://elsewhere:1'
        with self.assertRaisesRegex(RuntimeError, 'messaging points CF_DEVELOPER_PLATFORM_BASE_URL somewhere else'):
            self.run_it()
        self.envs['messaging']['CF_DEVELOPER_PLATFORM_BASE_URL'] = self.dp
        self.envs['meeting-service']['CF_RUNTIME_ENVIRONMENT'] = 'staging'
        with self.assertRaisesRegex(RuntimeError, 'meeting-service lacks its production signing identity'):
            self.run_it()

    def test_switches_are_refused_as_on_until_the_activation_is_recorded(self):
        meeting = {'MEETING_PLAN_ENFORCEMENT': 'on'}
        legacy = {'MEETING_PLAN_ENFORCEMENT_ENABLED': 'true'}
        messaging = {'MESSAGING_PLAN_ENFORCEMENT': 'on'}
        with patch.object(cr, 'done', lambda op: False):
            self.assertEqual(cr.safe_default_violations({'meeting-service': meeting}),
                             ['meeting-service MEETING_PLAN_ENFORCEMENT'])
            self.assertEqual(cr.safe_default_violations({'meeting-service': legacy}),
                             ['meeting-service MEETING_PLAN_ENFORCEMENT_ENABLED'])
            self.assertEqual(cr.safe_default_violations({'messaging': messaging}),
                             ['messaging MESSAGING_PLAN_ENFORCEMENT'])
            self.assertEqual(cr.safe_default_violations({'messaging': {'MESSAGING_PLAN_ENFORCEMENT': 'off'}}), [])
        with patch.object(cr, 'done', lambda op: op == 'activate-plan-enforcement'):
            self.assertEqual(cr.safe_default_violations({'meeting-service': meeting, 'messaging': messaging}), [])
            self.assertEqual(cr.safe_default_violations({'messaging': {'MESSAGING_PLAN_ENFORCEMENT': 'yes'}}),
                             ['messaging MESSAGING_PLAN_ENFORCEMENT'])


class OwnerDeliveredLinks(unittest.TestCase):
    """ADR 0068 A4: links sealed to the Notification Service, off until their activation."""

    KEY = ('notif-hpke-2026-11-1', Path('/secrets/notifications-hpke.key'), 'notif-hpke-2026-11-1:PUBLIC')

    def setUp(self):
        notif = cr.private_url(cr.SERVICES['notifications']['tls'])
        self.envs = {name: dict(SIGNED) for name in cr.SERVICES}
        for name in ('developer-platform', 'developer-platform-worker', 'organizations'):
            self.envs[name]['CF_NOTIFICATIONS_BASE_URL'] = notif
        self.envs['notifications']['NOTIFICATIONS_HPKE_PRIVATE_KEY_FILE'] = cr.notif_ops.HPKE_TARGET
        self.tables = {'devplat_owner_deliveries', 'organization_exports'}

    def run_it(self, step):
        applied = []
        with patch.object(cr, 'env_of', lambda name: self.envs[name]), \
                patch.object(cr, 'tls_route_present', lambda port: None), \
                patch.object(cr, 'table_present', lambda service, table: table in self.tables), \
                patch.object(cr.notif_ops, 'sealing_key', lambda cr_: self.KEY), \
                patch.object(cr, 'organizations_export_keys', lambda: Path('/secrets/organizations-export.keys')), \
                patch.object(cr, 'emit', lambda *a: None), patch.object(cr, 'apply_activation', capture_apply(applied)):
            step(SHA, '1', '')
        return applied

    def test_the_developer_platform_gets_the_public_sealing_key_and_the_replicas_the_private_one(self):
        (op, changes), = self.run_it(cr.activate_developer_owner_invitations)
        self.assertEqual(op, 'activate-developer-owner-invitations')
        # notifications already holds the private key; only notifications-2 gets the mount.
        self.assertEqual(changes, [
            ('notifications-2', {'NOTIFICATIONS_HPKE_PRIVATE_KEY_FILE': cr.notif_ops.HPKE_TARGET,
                                 'NOTIFICATIONS_HPKE_KEY_ID': self.KEY[0]},
             {'mounts': [(self.KEY[1], cr.notif_ops.HPKE_TARGET)]}),
            ('developer-platform', {'CF_NOTIFICATIONS_SEALING_KEY': self.KEY[2]})])
        self.assertFalse([g for g in cr.GRANTS if g[0] == 'activate-developer-owner-invitations'],
                         'I1 is granted by activate-developer-publications')
        self.assertEqual(cr.ACTIVATIONS['activate-developer-owner-invitations'][0], ('activate-developer-publications',))

    def test_organizations_export_delivery_mounts_the_generated_keys(self):
        (op, changes), = self.run_it(cr.activate_organization_export_delivery)
        self.assertEqual(op, 'activate-organization-export-delivery')
        self.assertEqual(changes[-1], ('organizations', {
            'ORGANIZATIONS_EXPORT_DELIVERY_ENABLED': 'true', 'CF_NOTIFICATIONS_SEALING_KEY': self.KEY[2],
            'ORGANIZATIONS_EXPORT_KEYS_FILE': '/run/secrets/organizations-export.keys'},
            {'mounts': [(Path('/secrets/organizations-export.keys'), '/run/secrets/organizations-export.keys')]}))
        self.assertEqual(cr.ACTIVATIONS['activate-organization-export-delivery'][0],
                         ('activate-organizations-publications',))
        workflow = (REPO / '.github/workflows/cfmeet-coordinated-release.yml').read_text()
        for op in ('activate-developer-owner-invitations,', 'activate-organization-export-delivery,'):
            self.assertIn(op, workflow)

    def test_each_refuses_before_changing_anything(self):
        with patch.object(cr, 'apply_activation', side_effect=AssertionError('must not apply')):
            del self.envs['developer-platform-worker']['CF_NOTIFICATIONS_BASE_URL']
            with self.assertRaisesRegex(RuntimeError, 'developer-platform-worker .*activate-developer-publications first'):
                self.run_it(cr.activate_developer_owner_invitations)
            del self.envs['organizations']['CF_NOTIFICATIONS_BASE_URL']
            with self.assertRaisesRegex(RuntimeError, 'activate-organizations-publications first'):
                self.run_it(cr.activate_organization_export_delivery)
        self.setUp()
        self.tables.clear()
        with self.assertRaisesRegex(RuntimeError, 'devplat_owner_deliveries'):
            self.run_it(cr.activate_developer_owner_invitations)
        with self.assertRaisesRegex(RuntimeError, 'organization_exports'):
            self.run_it(cr.activate_organization_export_delivery)
        self.setUp()
        self.envs['developer-platform']['CF_NOTIFICATIONS_SEALING_KEY'] = 'notif-hpke-2020-01-1:OTHER'
        with self.assertRaisesRegex(RuntimeError, 'another sealing key'):
            self.run_it(cr.activate_developer_owner_invitations)

    def test_the_export_key_file_is_generated_once_in_the_vault_format(self):
        with tempfile.TemporaryDirectory() as folder, patch.object(p, 'SECRETS', Path(folder)), \
                patch.object(cr.os, 'chown', lambda *a, **k: None, create=True), \
                patch.object(cr.os, 'chmod', lambda *a, **k: None):
            import datetime
            path = cr.organizations_export_keys(datetime.date(2026, 11, 21))
            first = path.read_text()
            self.assertRegex(first, r'^exp-2026-11:[A-Za-z0-9_-]{43}\n$')
            self.assertEqual(len(base64.urlsafe_b64decode(first.split(':')[1].strip() + '=')), 32)
            cr.organizations_export_keys(datetime.date(2027, 1, 1))
            self.assertEqual(path.read_text(), first, 'never regenerated')
            path.write_text('exp-1:short\n')
            with self.assertRaisesRegex(RuntimeError, 'malformed'):
                cr.organizations_export_keys()

    def test_safe_defaults_refuse_delivery_until_recorded(self):
        orgs = {'ORGANIZATIONS_EXPORT_DELIVERY_ENABLED': 'true'}
        dp = {'CF_NOTIFICATIONS_SEALING_KEY': self.KEY[2]}
        with patch.object(cr, 'done', lambda op: False):
            self.assertEqual(cr.safe_default_violations({'organizations': orgs, 'developer-platform': dp}),
                             ['organizations ORGANIZATIONS_EXPORT_DELIVERY_ENABLED',
                              'developer-platform CF_NOTIFICATIONS_SEALING_KEY'])
        with patch.object(cr, 'done', lambda op: op in ('activate-organization-export-delivery',
                                                        'activate-developer-owner-invitations')):
            self.assertEqual(cr.safe_default_violations({'organizations': orgs, 'developer-platform': dp}), [])
        # Partner push gives the Developer Platform the same key (it seals push secrets to it).
        with patch.object(cr, 'done', lambda op: op == cr.platform_ops.PUSH):
            self.assertEqual(cr.safe_default_violations({'developer-platform': dp}), [])


ORIGINAL_SEALED = cr._sealed


class PublicInternalDenial(unittest.TestCase):
    def test_only_a_marked_nginx_404_proves_the_denial(self):
        # Notification Service N0-6: a Meeting Service 404 (route token unset) is not proof.
        self.assertTrue(cr.public_denial_confirmed('HTTP/2 404\r\nserver: nginx\r\nx-cfmeet-ingress: denied\r\n\r\n'))
        self.assertTrue(cr.public_denial_confirmed('HTTP/1.1 404 Not Found\nX-CFMeet-Ingress: Denied\n'))
        for bad in ('HTTP/2 404\r\nserver: nginx\r\n', 'HTTP/2 401\r\nx-cfmeet-ingress: denied\r\n',
                    'HTTP/2 200\r\n', '', 'HTTP/2 404\r\nx-cfmeet-ingress: allowed\r\n'):
            self.assertFalse(cr.public_denial_confirmed(bad), bad)

    def test_the_operation_is_offered_without_prerequisites(self):
        self.assertEqual(cr.ACTIVATIONS['activate-public-internal-denial'][0], ())


class RestoreTest(unittest.TestCase):
    def test_readiness_and_every_step_use_tcp_never_the_init_socket(self):
        # Releases 43589841572 / 43609024085: socket readiness can report the entrypoint's
        # temporary init server, so createdb raced its shutdown. Only the real server listens on TCP.
        calls = []

        class Done:
            returncode = 0
            stdout = ''

        def fake_run(args, **kwargs):
            calls.append(list(args))
            return Done()

        def fake_step(args, step, input=None):
            calls.append(list(args))
            if 'psql' in args:
                return '0' if 'information_schema' in (input or '') else ''
            return ''

        with patch.object(cr, '_own', lambda *a, **k: None), patch.object(cr.subprocess, 'run', fake_run), \
                patch.object(cr, 'docker_step', fake_step), \
                patch.object(cr, 'run_binary', lambda args, **k: calls.append(list(args))), \
                patch.object(cr, 'history_state', lambda sql: {}), patch.object(cr, 'remove', lambda name: None):
            result = cr.restore_test('/dev/null', 16, {}, '1-1', 'organizations')
        self.assertEqual(result['status'], 'PASS')
        execs = [c for c in calls if c[:2] == ['docker', 'exec']]
        self.assertTrue(execs)
        for c in execs:
            self.assertIn('127.0.0.1', c, c)
        self.assertIn('pg_isready', execs[0])


class PreReleaseRestoreTests(unittest.TestCase):
    def test_every_release_restore_tests_the_developer_platform_database(self):
        import inspect
        default = inspect.signature(cr.backup).parameters['restore_labels'].default
        self.assertEqual(default, cr.RESTORE_TEST_LABELS)
        for label in ('meeting-service', 'organizations', 'platform-access', 'developer-platform'):
            self.assertIn(label, default)
        # The label is derived from the database name exactly as backup() does.
        db = cr.SERVICES['developer-platform']['db']
        self.assertEqual(db.removeprefix('cfmeet_').replace('_', '-'), 'developer-platform')
        self.assertTrue(cr.RESTORE_RE.match('cfmeet-restore-test-12-3-developer-platform'))


class SafeStderr(unittest.TestCase):
    def test_failures_name_the_step_and_never_print_credentials_or_row_values(self):
        raw = (b'pg_restore: error: could not execute query: ERROR:  duplicate key value violates unique constraint\n'
               b'DETAIL:  Key (slug)=(acme-health) already exists.\n'
               b'connection to postgresql://svc:s3cret@db:5432/x failed for user "alice@example.com"\n')
        text = cr.safe_stderr(raw)
        self.assertIn('Key (slug)=(...)', text)
        for secret in ('acme-health', 's3cret', 'alice@example.com', 'postgresql://'):
            self.assertNotIn(secret, text)
        with self.assertRaisesRegex(RuntimeError, r'sh -c \[restore test organizations\] \(exit 1\): .*duplicate key'):
            cr.run_binary(['sh', '-c', 'printf "%s" "$0" >&2; exit 1',
                           'ERROR:  duplicate key value violates unique constraint'], step='restore test organizations')

    def test_long_output_is_bounded(self):
        self.assertLessEqual(len(cr.safe_stderr(b'x' * 10000 + b'\n' + b'y' * 10000)), 400)


if __name__ == '__main__':
    unittest.main()
