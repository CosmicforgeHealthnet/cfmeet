import copy
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import Mock, patch

import meeting_recording_media as m


def container():
    return {'Id': 'old-container-id', 'Image': 'immutable-image', 'State': {'Running': True},
        'Config': {'Cmd': ['--config', '/etc/livekit.yaml'], 'Env': ['PRESERVE=setting'],
                   'Labels': {'original': 'label'}, 'ExposedPorts': {'7880/tcp': {}}},
        'HostConfig': {'NetworkMode': 'bridge', 'RestartPolicy': {'Name': 'unless-stopped'},
            'Memory': 1024, 'CapDrop': ['NET_RAW'], 'Binds': ['/config:/etc/livekit.yaml:ro'],
            'PortBindings': {'7880/tcp': [{'HostIp': '', 'HostPort': '7880'}],
                             '7881/tcp': [{'HostIp': '', 'HostPort': '7881'}],
                             '3478/udp': [{'HostIp': '', 'HostPort': '3478'}]}},
        'Mounts': [{'Type': 'bind', 'Source': '/config', 'Destination': '/etc/livekit.yaml'}],
        'NetworkSettings': {'Networks': {'bridge': {}, m.p.NET: {'Aliases': ['lk-server', 'old-container-id'],
                                            'IPAMConfig': {'IPv4Address': '', 'IPv6Address': ''}}}}}


def configuration():
    return {'keys': {'existing': 'secret', 'second': 'preserved'}, 'redis': {'password': 'preserved'},
            'rtc': {'tcp_port': 7881, 'port_range_start': 50000, 'port_range_end': 60000,
                    'node_ip': '192.0.2.1', 'use_external_ip': True},
            'webhook': {'urls': ['calling', 'meeting']}, 'turn': {'enabled': True}}


class MediaPlan(unittest.TestCase):
    def test_mux_preserves_every_other_config_setting_and_docker_setting(self):
        before, old = configuration(), container()
        frozen = copy.deepcopy((before, old))
        after, payload, change = m.plan(before, old)
        self.assertTrue(change)
        self.assertEqual((before, old), frozen)
        expected = copy.deepcopy(before)
        expected['rtc'].update(udp_port=7882)
        expected['rtc'].pop('port_range_start'); expected['rtc'].pop('port_range_end')
        self.assertEqual(after, expected)
        host = copy.deepcopy(old['HostConfig'])
        host['PortBindings']['7882/udp'] = m.BINDING
        self.assertEqual(payload['HostConfig'], host)
        self.assertEqual(payload['Image'], old['Image'])
        self.assertEqual(payload['Env'], old['Config']['Env'])
        self.assertNotIn('Aliases', payload['NetworkingConfig']['EndpointsConfig']['bridge'])
        self.assertEqual(payload['NetworkingConfig']['EndpointsConfig'][m.p.NET]['Aliases'], ['lk-server'])
        updated = copy.deepcopy(old)
        updated['HostConfig'] = payload['HostConfig']
        self.assertFalse(m.plan(after, updated)[2])

    def test_unknown_shared_container_or_custom_media_config_is_never_replaced(self):
        for variant in ('host', 'compose', 'volume', 'static-ip', 'custom-port', 'custom-range', 'binding'):
            config, old = configuration(), container()
            if variant == 'host': old['HostConfig']['NetworkMode'] = 'host'
            if variant == 'compose': old['Config']['Labels']['com.docker.compose.project'] = 'foreign'
            if variant == 'volume': old['Mounts'][0]['Type'] = 'volume'
            if variant == 'static-ip': old['NetworkSettings']['Networks'][m.p.NET]['IPAMConfig']['IPv4Address'] = '192.0.2.2'
            if variant == 'custom-port': config['rtc']['udp_port'] = 9000
            if variant == 'custom-range': config['rtc']['port_range_end'] = 55000
            if variant == 'binding': old['HostConfig']['PortBindings']['7882/udp'] = [{'HostIp': '', 'HostPort': '9000'}]
            with self.subTest(variant=variant), self.assertRaises(RuntimeError):
                m.plan(config, old)

    def test_signer_and_mount_must_identify_the_existing_shared_sfu(self):
        with patch.object(m.media, 'calling_key', return_value=('existing', 'wrong')):
            with self.assertRaisesRegex(RuntimeError, 'identity'):
                m.known_identity(configuration(), container())
        old = container()
        old['Config']['Env'].append('LIVEKIT_KEYS=override')
        with self.assertRaisesRegex(RuntimeError, 'Alternate'):
            m.known_identity(configuration(), old)


class MediaReplacement(unittest.TestCase):
    def test_idle_replacement_and_each_failure_restore_the_exact_old_container_and_file(self):
        for failure in (None, 'create-response', 'start', 'readiness'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                root = Path(directory); file = root/'config'; file.write_text(json.dumps(configuration()))
                original = file.read_text()
                old = container(); old['Mounts'][0]['Source'] = str(file)
                world = {m.NAME: copy.deepcopy(old)}; events = []
                def command(*args):
                    if args[:3] == ('docker', 'ps', '-a'): return '\n'.join(world)
                    if args[:2] == ('docker', 'rename'):
                        world[args[3]] = world.pop(args[2]); events.append('rename')
                    elif args[:2] == ('docker', 'stop'): events.append('stop')
                    elif args[:2] == ('docker', 'rm'): world.pop(args[-1]); events.append('remove-new')
                    elif args[:2] == ('docker', 'start'): events.append('restore-start')
                    return ''
                def api(method, path, payload=None):
                    if '/create?' in path:
                        world[m.NAME] = {'Image': payload['Image'], 'Config': payload, 'Id': 'replacement'}
                        events.append('create')
                        if failure == 'create-response': raise RuntimeError('response lost')
                    else:
                        events.append('start-new')
                        if failure == 'start': raise RuntimeError('start')
                yaml = SimpleNamespace(safe_load=json.loads, safe_dump=lambda d, **_: json.dumps(d))
                cr = SimpleNamespace(emit=Mock())
                with patch.dict('sys.modules', yaml=yaml), patch.object(m.media, 'CONFIG', file), \
                        patch.object(m.p, 'ROOT', root), patch.object(m.p, 'inspect', side_effect=lambda n: world[n]), \
                        patch.object(m.media, 'calling_key', return_value=('existing', 'secret')), \
                        patch.object(m, 'idle') as idle, patch.object(m.p, 'run', side_effect=command), \
                        patch.object(m.p, 'docker_api', side_effect=api), \
                        patch.object(m, 'wait_ready', side_effect=[RuntimeError('readiness'), None] if failure == 'readiness' else None):
                    if failure:
                        with self.assertRaises(RuntimeError): m.ensure(cr, '123-1')
                        self.assertEqual(world[m.NAME], old)
                        self.assertEqual(file.read_text(), original)
                        cr.emit.assert_not_called()
                    else:
                        m.ensure(cr, '123-1')
                        self.assertEqual(json.loads(file.read_text())['rtc']['udp_port'], 7882)
                        self.assertTrue(cr.emit.call_args.args[1]['keysPreserved'])
                    self.assertEqual(idle.call_count, 2)

    def test_active_media_refuses_before_any_docker_mutation(self):
        with patch.object(m.media, 'calling_key', return_value=('k', 's')), \
                patch.object(m.media, 'participants', return_value=1), patch.object(m.p, 'lk_request') as request:
            with self.assertRaisesRegex(RuntimeError, 'Active media'): m.idle()
            request.assert_not_called()

