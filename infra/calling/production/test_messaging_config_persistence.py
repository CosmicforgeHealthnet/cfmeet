"""Messaging dependency configuration must survive later coordinated releases,
roll back to the exact previous container, and never leak into isolated staging."""
import copy
import itertools
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import communications_release as release
import messaging_runtime as runtime

NET = release.p.NET
PRODUCTION_REDIS_PASSWORD = 'production-redis-password-fixture'
SCOPE = {'tenantId': 'tenant', 'applicationId': 'app', 'environmentId': 'env', 'serviceAccountId': 'svc', 'kind': 'PRODUCTION'}
_ids = itertools.count(1)


def running_messaging(image='sha256:787bb9a-messaging'):
    """The unconfigured production Messaging container as Docker inspects it."""
    return {'Id': 'c%063d' % next(_ids), 'Image': image, 'State': {'Running': True},
        'Config': {'Image': 'ghcr.io/cosmicforgehealthnet/cfmeet-messaging:787bb9a', 'User': '10001',
            'Cmd': None, 'Entrypoint': ['/app/messaging'], 'Labels': {},
            'Env': ['CF_RUNTIME_ENVIRONMENT=production', 'MESSAGING_E2EE_DEVICES_ENABLED=false',
                    'DATABASE_URL=postgres://messaging_runtime@cfmeet-core-db/cfmeet_messaging',
                    'MESSAGING_KT_LOG_ID=cfmeet.messaging:production', 'MESSAGING_REALTIME_REQUIRE_TLS=true',
                    'MESSAGING_TRANSPARENCY_SIGNING_KEY_FILE=/run/secrets/kt.pem']},
        'HostConfig': {'AutoRemove': False, 'PortBindings': {}, 'Binds': ['/opt/cfmeet-core/secrets/kt.pem:/run/secrets/kt.pem:ro'],
            'Mounts': [], 'Memory': 0, 'NanoCpus': 0, 'LogConfig': {'Type': 'json-file', 'Config': {}},
            'RestartPolicy': {'Name': 'unless-stopped', 'MaximumRetryCount': 0}},
        'NetworkSettings': {'Networks': {NET: {'Aliases': ['cfmeet-messaging'], 'IPAMConfig': {'IPv4Address': '172.30.0.9'}}}}}


def as_container(cfg, image_id):
    """What `docker inspect` returns for a container created from `cfg`."""
    config = {k: copy.deepcopy(v) for k, v in cfg.items() if k not in ('HostConfig', 'NetworkingConfig')}
    networks = {net: {'Aliases': ep.get('Aliases'), 'IPAMConfig': ep.get('IPAMConfig')}
                for net, ep in cfg['NetworkingConfig']['EndpointsConfig'].items()}
    return {'Id': 'c%063d' % next(_ids), 'Image': image_id, 'State': {'Running': True}, 'Config': config,
            'HostConfig': copy.deepcopy(cfg['HostConfig']), 'NetworkSettings': {'Networks': networks}}


def env(cfg):
    items = cfg['Env'] if 'Env' in cfg else cfg['Config']['Env']
    keys = [item.split('=', 1)[0] for item in items]
    assert len(keys) == len(set(keys)), 'duplicate environment entries'
    return dict(item.split('=', 1) for item in items)


def mounts(host):
    return {m['Target']: (m['Source'], m.get('ReadOnly')) for m in host.get('Mounts') or []}


class FakeDocker:
    """Containers are tracked by identity, so 'exact previous container' is checkable."""

    def __init__(self, containers):
        self.by_name = dict(containers)
        self.commands = []

    def run(self, *args, input=None):
        self.commands.append(args)
        if args[:3] == ('docker', 'ps', '-a'):
            return '\n'.join(self.by_name)
        if args[:2] == ('docker', 'rename'):
            self.by_name[args[3]] = self.by_name.pop(args[2])
        elif args[:2] == ('docker', 'stop'):
            self.by_name[args[-1]]['State']['Running'] = False
        elif args[:2] == ('docker', 'start'):
            self.by_name[args[-1]]['State']['Running'] = True
        return ''

    def inspect(self, name):
        return self.by_name[name]

    def api(self, method, path, body=None):
        self.commands.append((method, path))
        if path.startswith('/containers/create?name='):
            name = path.split('=', 1)[1]
            assert name not in self.by_name, 'container name reused'
            self.by_name[name] = as_container(body, body['Image'])


class Base(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(); self.addCleanup(self.folder.cleanup)
        root = Path(self.folder.name)
        self.messaging_work = root / 'messaging'; self.comm_work = root / 'communications'
        self.messaging_work.mkdir(); self.comm_work.mkdir()
        for name in ('managed.ring', 'storage-secret'):
            (self.messaging_work / name).write_text('fixture-' + name)
            (self.messaging_work / name).chmod(0o600)
        for name in ('messaging.trust.json', 'stage-managed.ring'):
            (self.comm_work / name).write_text('fixture')
        for target, value in ((release, self.comm_work), (runtime, self.messaging_work)):
            patcher = patch.object(target, 'WORK', value); patcher.start(); self.addCleanup(patcher.stop)
        for target in (release.os, runtime.os):
            patcher = patch.object(target, 'chown'); patcher.start(); self.addCleanup(patcher.stop)

    def configured_cfg(self, old):
        return runtime.runtime_config(old, release.p.env_of(old), 'cfmeet-msg-user', self.messaging_work / 'storage-secret',
            self.messaging_work / 'managed.ring', self.messaging_work / 'kt-checkpoints', PRODUCTION_REDIS_PASSWORD)


class Persistence(Base):
    def test_configure_keeps_the_exact_running_image_and_production_e2ee_disabled(self):
        old = running_messaging(); snapshot = copy.deepcopy(old)
        cfg = self.configured_cfg(old)
        self.assertEqual(old, snapshot)
        self.assertEqual(cfg['Image'], old['Image'])
        values = env(cfg)
        self.assertEqual(values['MESSAGING_E2EE_DEVICES_ENABLED'], 'false')
        for key, value in runtime.runtime_overrides(env(old), 'cfmeet-msg-user', PRODUCTION_REDIS_PASSWORD).items():
            self.assertEqual(values[key], value)
        self.assertEqual(mounts(cfg['HostConfig']), {
            '/run/messaging/managed.ring': (str(self.messaging_work / 'managed.ring'), True),
            '/run/messaging/storage.secret': (str(self.messaging_work / 'storage-secret'), True),
            '/var/lib/messaging-kt': (str(self.messaging_work / 'kt-checkpoints'), False)})
        self.assertEqual(cfg['HostConfig']['Binds'], old['HostConfig']['Binds'])
        self.assertEqual(cfg['HostConfig']['LogConfig']['Config'], {'max-size': '20m', 'max-file': '5'})
        self.assertGreater(cfg['HostConfig']['Memory'], 0); self.assertGreater(cfg['HostConfig']['NanoCpus'], 0)

    def test_configure_refuses_production_e2ee(self):
        old = running_messaging(); old['Config']['Env'][1] = 'MESSAGING_E2EE_DEVICES_ENABLED=true'
        with self.assertRaises(AssertionError):
            self.configured_cfg(old)

    def test_later_release_image_replacement_retains_every_messaging_dependency(self):
        configured = as_container(self.configured_cfg(running_messaging()), 'sha256:787bb9a-messaging')
        with patch.object(release.p, 'inspect', return_value=configured), \
             patch.object(release, 'image', return_value='sha256:0a5d9dd-messaging'):
            name, cfg = release.configured('messaging', '0' * 40, False, SCOPE)
        self.assertEqual(name, 'cfmeet-messaging')
        self.assertEqual(cfg['Image'], 'sha256:0a5d9dd-messaging')
        before, after = env(configured), env(cfg)
        for key, value in before.items():
            if key.startswith('MESSAGING_') or key in ('CF_RUNTIME_ENVIRONMENT', 'DATABASE_URL'):
                self.assertEqual(after[key], value, key)
        self.assertEqual(after['MESSAGING_E2EE_DEVICES_ENABLED'], 'false')
        self.assertEqual(after['CF_COMMUNICATIONS_PERSONAL_TENANT_ID'], 'tenant')
        expected = mounts(configured['HostConfig'])
        expected['/run/communications/trust.json'] = (str(self.comm_work / 'messaging.trust.json'), True)
        self.assertEqual(mounts(cfg['HostConfig']), expected)
        for key in ('Binds', 'Memory', 'NanoCpus', 'LogConfig', 'RestartPolicy', 'PortBindings'):
            self.assertEqual(cfg['HostConfig'][key], configured['HostConfig'][key], key)
        endpoint = cfg['NetworkingConfig']['EndpointsConfig'][NET]
        self.assertEqual((endpoint['Aliases'], endpoint['IPAMConfig']), (['cfmeet-messaging'], {'IPv4Address': '172.30.0.9'}))

    def test_repeated_releases_do_not_duplicate_or_drift_configuration(self):
        current = as_container(self.configured_cfg(running_messaging()), 'sha256:a')
        first = None
        for image in ('sha256:b', 'sha256:c', 'sha256:d'):
            with patch.object(release.p, 'inspect', return_value=current), patch.object(release, 'image', return_value=image):
                _, cfg = release.configured('messaging', '0' * 40, False, SCOPE)
            current = as_container(cfg, image)
            shape = (env(cfg), mounts(cfg['HostConfig']), cfg['HostConfig']['Binds'])
            first = first or shape
            self.assertEqual(shape, first)

    def test_failed_release_rollback_restores_the_exact_previous_container(self):
        configured = as_container(self.configured_cfg(running_messaging()), 'sha256:787bb9a-messaging')
        snapshot = copy.deepcopy(configured)
        docker = FakeDocker({'cfmeet-messaging': configured})
        with patch.object(release.p, 'run', side_effect=docker.run), patch.object(release.p, 'inspect', side_effect=docker.inspect), \
             patch.object(release.p, 'docker_api', side_effect=docker.api), patch.object(release.p, 'regression'), \
             patch.object(release, 'image', return_value='sha256:0a5d9dd-messaging'):
            record = dict(sha='0' * 40, runId='42', replacements=[], newHub=False)
            name, cfg = release.configured('messaging', '0' * 40, False, SCOPE)
            release.replace(name, cfg, record)
            self.assertIsNot(docker.by_name['cfmeet-messaging'], configured)
            release.rollback(record)
        restored = docker.by_name['cfmeet-messaging']
        self.assertIs(restored, configured)
        self.assertTrue(restored['State']['Running'])
        restored_copy = copy.deepcopy(restored); restored_copy['State'] = snapshot['State']
        self.assertEqual(restored_copy, snapshot)
        self.assertEqual(record['replacements'][0]['original']['Id'], snapshot['Id'])
        failed = [n for n in docker.by_name if n.startswith('cfmeet-messaging-communications-failed-42-')]
        self.assertEqual(len(failed), 1)
        self.assertFalse(any(c[:2] == ('docker', 'rm') for c in docker.commands))

    def test_failed_configure_restores_the_exact_previous_container(self):
        old = running_messaging(); snapshot = copy.deepcopy(old)
        docker = FakeDocker({'cfmeet-messaging': old})
        def probe(name):
            if name == 'cfmeet-messaging':
                raise RuntimeError('dependency probe failed')
            return {}
        with patch.object(release.p, 'run', side_effect=docker.run), patch.object(release.p, 'inspect', side_effect=docker.inspect), \
             patch.object(release.p, 'docker_api', side_effect=docker.api), patch.object(release, 'wait'), \
             patch.object(runtime, 'scanner', return_value={}), patch.object(runtime, 'dependency_probe', side_effect=probe), \
             patch.object(release.p, 'secret', return_value=PRODUCTION_REDIS_PASSWORD), self.assertRaises(RuntimeError):
            runtime._configure('0' * 40, '77', old, release.p.env_of(old), self.messaging_work / 'managed.ring',
                               'cfmeet-msg-user', self.messaging_work / 'storage-secret')
        self.assertIs(docker.by_name['cfmeet-messaging'], old)
        self.assertTrue(old['State']['Running'])
        old_copy = copy.deepcopy(old); old_copy['State'] = snapshot['State']
        self.assertEqual(old_copy, snapshot)
        self.assertIn('cfmeet-messaging-runtime-failed-77', docker.by_name)
        journal = json.loads((self.messaging_work / 'runtime-rollback-77.private.json').read_text())
        self.assertEqual(journal['original']['Id'], snapshot['Id'])

    def outer_configure(self, scanner_existed):
        names = ['cfmeet-messaging'] + (['cfmeet-messaging-clamav'] if scanner_existed else [])
        docker = FakeDocker({n: {'State': {'Running': True}} for n in names})
        def fail(*args):
            docker.by_name.setdefault('cfmeet-messaging-clamav', {'State': {'Running': True}})
            raise RuntimeError('ClamAV readiness failed')
        baseline = self.comm_work / 'messaging-baseline-1.json'; baseline.write_text('{}')
        stopped = []
        with patch.object(runtime.p, 'ROOT', Path(self.folder.name)), patch.object(runtime.p, 'identity'), \
             patch.object(runtime.p, 'inspect', return_value=running_messaging()), \
             patch.object(runtime.p, 'run', side_effect=docker.run), patch.object(runtime, 'available_kib', return_value=8 * 2**20), \
             patch('messaging_release.database_summary', return_value={'conversations': 0}), \
             patch.object(runtime, 'storage_identity', return_value=('user', self.messaging_work / 'storage-secret')), \
             patch.object(runtime, '_configure', side_effect=fail), \
             patch.object(runtime.subprocess, 'run', side_effect=lambda args, **kw: stopped.append(args)), \
             self.assertRaises(RuntimeError):
            runtime.configure('0' * 40, '78')
        return stopped

    def test_failed_configure_stops_a_scanner_it_created(self):
        self.assertEqual(self.outer_configure(False), [['docker', 'stop', '--time', '20', 'cfmeet-messaging-clamav']])

    def test_failed_configure_never_touches_a_preexisting_scanner(self):
        self.assertEqual(self.outer_configure(True), [])

    def test_configure_refuses_without_memory_headroom(self):
        with patch.object(runtime.p, 'ROOT', Path(self.folder.name)), patch.object(runtime.p, 'identity'), \
             patch.object(runtime.p, 'inspect', return_value=running_messaging()), \
             patch('messaging_release.database_summary', return_value={'conversations': 0}), \
             patch.object(runtime, 'available_kib', return_value=3 * 2**20), \
             patch.object(runtime, 'storage_identity') as storage, self.assertRaises(AssertionError):
            (self.comm_work / 'messaging-baseline-1.json').write_text('{}')
            runtime.configure('0' * 40, '79')
        storage.assert_not_called()


# Every runtime dependency must be explicitly classified for isolated staging.
STAGE_DROPPED = {'MESSAGING_REALTIME_REDIS_URL', 'MESSAGING_CLAMAV_ADDRESS'}
STAGE_DROPPED_PREFIXES = ('MESSAGING_ATTACHMENTS_S3_', 'MESSAGING_PUSH_', 'MESSAGING_KT_WITNESS')
STAGE_REPLACED = {'MESSAGING_MANAGED_KEYRING_FILE': '/run/communications/managed.ring',
                  'MESSAGING_KT_CHECKPOINT_FILE': '/var/lib/messaging-kt/checkpoint.json'}
STAGE_SAFE = {'MESSAGING_E2EE_DEVICES_ENABLED', 'MESSAGING_METRICS_ADDRESS'}


class StagingIsolation(Base):
    def staged(self):
        configured = as_container(self.configured_cfg(running_messaging()), 'sha256:787bb9a-messaging')
        with patch.object(release.p, 'inspect', return_value=configured), patch.object(release, 'image', return_value='sha256:candidate'):
            return configured, release.configured('messaging', '0' * 40, True, SCOPE, 'postgres://stage_runtime@db/cfmeet_comm_stage_messaging')

    def test_every_production_dependency_is_classified_for_staging(self):
        for key in runtime.runtime_overrides({'CF_RUNTIME_ENVIRONMENT': 'production'}, 'u', 'p'):
            self.assertTrue(key in STAGE_DROPPED or key.startswith(STAGE_DROPPED_PREFIXES) or key in STAGE_REPLACED or key in STAGE_SAFE,
                            'unclassified Messaging dependency for isolated staging: ' + key)

    def test_isolated_staging_never_receives_production_secrets_or_state(self):
        configured, (name, cfg) = self.staged()
        values = env(cfg)
        self.assertEqual(name, 'cfmeet-comm-stage-messaging')
        for key in values:
            self.assertFalse(key in STAGE_DROPPED or key.startswith(STAGE_DROPPED_PREFIXES[:1] + STAGE_DROPPED_PREFIXES[2:]), key)
        for key, value in STAGE_REPLACED.items():
            self.assertEqual(values[key], value)
        self.assertEqual(values['MESSAGING_PUSH_PROVIDER'], 'none')
        self.assertEqual(values['MESSAGING_KT_LOG_ID'], 'cfmeet.messaging:isolated-release-test')
        self.assertEqual(values['DATABASE_URL'], 'postgres://stage_runtime@db/cfmeet_comm_stage_messaging')
        self.assertEqual(values['MESSAGING_E2EE_DEVICES_ENABLED'], 'false')
        text = json.dumps(cfg)
        self.assertNotIn(str(self.messaging_work), text)
        self.assertNotIn(PRODUCTION_REDIS_PASSWORD, text)
        self.assertNotIn('cfmeet-messaging-clamav', text)
        self.assertNotIn('cfmeet_messaging', values['DATABASE_URL'])
        stage_mounts = mounts(cfg['HostConfig'])
        self.assertEqual(stage_mounts['/var/lib/messaging-kt'], (str(self.comm_work / 'staging-kt-checkpoints'), False))
        self.assertEqual(stage_mounts['/run/communications/managed.ring'], (str(self.comm_work / 'stage-managed.ring'), True))
        self.assertNotIn('/run/messaging/managed.ring', stage_mounts)
        self.assertNotIn('/run/messaging/storage.secret', stage_mounts)
        self.assertEqual(cfg['HostConfig']['PortBindings'], {})
        self.assertEqual(cfg['HostConfig']['Memory'], 768 * 1024 * 1024)
        self.assertEqual(cfg['NetworkingConfig']['EndpointsConfig'][NET], {'Aliases': ['cfmeet-comm-stage-messaging']})
        self.assertEqual(cfg['Labels']['com.cfmeet.communications.candidate'], 'true')

    def test_staging_leaves_the_production_container_definition_untouched(self):
        configured = as_container(self.configured_cfg(running_messaging()), 'sha256:787bb9a-messaging')
        snapshot = copy.deepcopy(configured)
        with patch.object(release.p, 'inspect', return_value=configured), patch.object(release, 'image', return_value='sha256:candidate'):
            release.configured('messaging', '0' * 40, True, SCOPE, 'postgres://stage')
        self.assertEqual(configured, snapshot)


if __name__ == '__main__':
    unittest.main()
