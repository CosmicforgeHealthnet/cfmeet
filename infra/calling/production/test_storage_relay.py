"""Storage routes: no Docker address persists in host nginx; the relay stays confined."""
import unittest
from unittest.mock import patch

import messaging_runtime as runtime
import storage_relay as relay

CALLING = ('location ^~ /cfmeet-calling-artifacts/ {\n    limit_except GET HEAD { deny all; }\n'
           '    proxy_set_header Host $http_host;\n    proxy_pass http://172.23.0.9:9000;\n}\n')


class Routes(unittest.TestCase):
    def test_store_address_is_replaced_by_the_loopback_relay_once(self):
        text = relay.relayed(CALLING)
        self.assertIn('proxy_pass http://127.0.0.1:19000;', text)
        self.assertNotIn('172.23.0.9', text)
        self.assertIn('limit_except GET HEAD { deny all; }', text)  # policy unchanged
        self.assertIn('proxy_set_header Host $http_host;', text)  # S3 signatures still verify
        self.assertEqual(relay.relayed(text), text)

    def test_public_or_ambiguous_upstreams_are_refused(self):
        with self.assertRaises(AssertionError):
            relay.relayed(CALLING.replace('172.23.0.9', '8.8.8.8'))
        with self.assertRaises(AssertionError):
            relay.relayed(CALLING + CALLING.replace('172.23.0.9', '172.23.0.10'))
        with self.assertRaises(AssertionError):
            relay.relayed('location / { return 404; }\n')

    def test_messaging_routes_render_with_the_relay_and_still_accept_a_private_address(self):
        rendered = runtime.nginx_config(relay.ADDRESS)
        self.assertIn('proxy_pass http://127.0.0.1:19000;', rendered)
        self.assertNotRegex(rendered, r'proxy_pass http://172\.')
        self.assertIn('proxy_pass http://172.23.0.5:9000;', runtime.nginx_config('172.23.0.5'))
        with self.assertRaises(AssertionError):
            runtime.nginx_config('8.8.8.8')
        with self.assertRaises(AssertionError):
            runtime.nginx_config('127.0.0.1:2375')


class Relay(unittest.TestCase):
    def test_relay_resolves_the_store_by_name_and_forwards_bytes_only(self):
        text = relay.relay_config()
        self.assertIn('set $peer cfmeet-calling-artifacts:9000;', text)
        self.assertIn('resolver 127.0.0.11', text)
        self.assertIn('stream {', text)
        self.assertNotIn('http {', text)

    def test_relay_publishes_loopback_only_and_is_confined(self):
        args = relay.relay_create_args('sha256:proxy')
        self.assertIn('127.0.0.1:19000:9000', args)
        self.assertFalse(any(a.startswith('0.0.0.0') or a == '19000:9000' for a in args))
        for flag in ('--read-only', '--cap-drop', '--security-opt', '--memory', '--pids-limit'):
            self.assertIn(flag, args)
        self.assertEqual(args[args.index('--user') + 1], '101:101')

    def test_routes_use_the_relay_only_when_it_runs_confined(self):
        running = {'State': {'Running': True}, 'Image': 'sha256:proxy', 'NetworkSettings': {'Networks': {relay.RELAY_NET: {}, relay.p.NET: {}}},
                   'HostConfig': {'PortBindings': {'9000/tcp': [{'HostIp': '127.0.0.1', 'HostPort': '19000'}]}, 'ReadonlyRootfs': True, 'CapDrop': ['ALL']}}
        store = {'NetworkSettings': {'Networks': {relay.p.NET: {'IPAddress': '172.23.0.9'}}}}
        def inspect(name):
            return {'cfmeet-core-tls': {'Image': 'sha256:proxy'}, relay.RELAY: running, relay.storage.NAME: store}[name]
        with patch.object(relay, 'names', return_value=[relay.RELAY]), patch.object(relay.p, 'inspect', side_effect=inspect):
            self.assertEqual(relay.store_upstream(), '127.0.0.1:19000')
            running['HostConfig']['PortBindings'] = {'9000/tcp': [{'HostIp': '0.0.0.0', 'HostPort': '19000'}]}
            self.assertEqual(relay.store_upstream(), '172.23.0.9:9000')
        with patch.object(relay, 'names', return_value=[]), patch.object(relay.p, 'inspect', side_effect=inspect):
            self.assertEqual(relay.store_upstream(), '172.23.0.9:9000')


if __name__ == '__main__':
    unittest.main()
