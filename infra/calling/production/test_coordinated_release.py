import contextlib
import io
import json
import re
import tempfile
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
    for block in re.findall(r'\n    PermissionDefinition \{\n(.*?)\n    \},', text, re.S):
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
        self.assertEqual(cr.required_mib(new), 192 * 6 + 256 + 128 + cr.TRANSIENT_MIB)
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
        self.assertEqual(len(registry), (REPO / 'packages/service-auth/src/permissions.rs').read_text()
                         .count('\n    PermissionDefinition {\n'))
        self.assertGreater(len(registry), 60)
        self.assertEqual(cr.validate_grants(registry), [])

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
                self.assertIn(required, cr.ACTIVATIONS)
        chain = ['activate-provisioning', 'activate-org-events', 'org-cutover-export', 'org-cutover-import',
                 'org-cutover-shadow', 'org-cutover-authoritative', 'activate-enterprise-calling']
        for previous, step in zip(chain, chain[1:]):
            self.assertEqual(cr.ACTIVATIONS[step][0], (previous,))
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
                patch.object(p, 'run', side_effect=lambda *a, **k: calls.append(a) or ''):
            cr.migrate('organizations', 'sha256:new')
            self.assertFalse((cr.WORK / 'run' / 'organizations.migration.env').exists())
        self.assertTrue(calls)
        self.assertFalse(any('hunter2' in arg for call in calls for arg in call))
        self.assertTrue(all('--env-file' in call for call in calls))


class SafeDefaultsAndRouting(unittest.TestCase):
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
            'messaging MESSAGING_E2EE_DEVICES_ENABLED', 'sms-gateway SMS_GATEWAY_PROVIDER', 'telephony TELEPHONY_ENABLED'])
        envs['meeting-service'].update(SMTP_HOST='h', SMTP_USERNAME='u', SMTP_PASSWORD='p')
        self.assertNotIn('meeting-service NOTIFICATIONS_EMAIL_ENABLED without mail credentials',
                         cr.safe_default_violations(envs))

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
        self.assertEqual(set(cr.GATEWAY_PREFIXES), families)
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
        options = (workflow.get('on') or workflow[True])['workflow_dispatch']['inputs']['operation']['options']
        self.assertEqual(tuple(options), cr.OPERATIONS)


if __name__ == '__main__':
    unittest.main()
