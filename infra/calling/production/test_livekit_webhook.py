"""livekit_webhook: only a webhook block is ever added, never a different one overwritten."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import livekit_webhook as w  # noqa: E402

try:
    import yaml  # noqa: F401
    HAVE_YAML = True
except ImportError:
    HAVE_YAML = False

BASE = """port: 7880
keys:
  calling-key: calling-secret
  meeting-key: meeting-secret
rtc:
  tcp_port: 7881
  udp_port: 7882
redis:
  address: cfmeet-calling-redis:6379
  password: "p"
"""


class Target(unittest.TestCase):
    def test_host_network_uses_the_loopback_proxy(self):
        self.assertEqual(w.target_url({'HostConfig': {'NetworkMode': 'host'}}), w.HOST_URL)

    def test_core_network_member_uses_the_calling_container(self):
        c = {'HostConfig': {'NetworkMode': 'bridge'},
             'NetworkSettings': {'Networks': {'cfmeet-core-private': {}}}}
        self.assertEqual(w.target_url(c, 'cfmeet-core-private'), w.BRIDGE_URL)

    def test_no_route_is_none(self):
        c = {'HostConfig': {'NetworkMode': 'bridge'}, 'NetworkSettings': {'Networks': {'other': {}}}}
        self.assertIsNone(w.target_url(c, 'cfmeet-core-private'))

    def test_callback_path_is_callings(self):
        for url in (w.HOST_URL, w.BRIDGE_URL):
            self.assertTrue(url.endswith('/internal/provider/media/events'))


@unittest.skipUnless(HAVE_YAML, 'PyYAML not installed')
class Plan(unittest.TestCase):
    def test_adds_exactly_one_block_and_preserves_everything_else(self):
        import yaml
        action, text = w.plan(BASE, w.HOST_URL, 'calling-key')
        self.assertEqual(action, 'add')
        after = yaml.safe_load(text)
        self.assertEqual(after['webhook'], {'api_key': 'calling-key', 'urls': [w.HOST_URL]})
        self.assertEqual({k: v for k, v in after.items() if k != 'webhook'}, yaml.safe_load(BASE))
        self.assertTrue(text.startswith(BASE.rstrip()))

    def test_already_present_is_a_no_op(self):
        _, text = w.plan(BASE, w.HOST_URL, 'calling-key')
        self.assertEqual(w.plan(text, w.HOST_URL, 'calling-key'), ('present', text))

    def test_a_different_existing_webhook_is_never_overwritten(self):
        other = BASE + 'webhook:\n  api_key: meeting-key\n  urls:\n    - http://elsewhere/hook\n'
        with self.assertRaisesRegex(RuntimeError, 'different webhook'):
            w.plan(other, w.HOST_URL, 'calling-key')

    def test_key_must_be_one_of_livekits_keys(self):
        with self.assertRaisesRegex(RuntimeError, "not one of LiveKit's keys"):
            w.plan(BASE, w.HOST_URL, 'unknown-key')

    def test_rendered_scalars_are_quoted(self):
        text = w.render('a: 1\n', {'api_key': 'k: v', 'urls': ['http://x/y']})
        self.assertIn('api_key: "k: v"', text)


class Operations(unittest.TestCase):
    def test_unknown_operation_refused(self):
        with self.assertRaises(RuntimeError):
            w.run('livekit-webhook-x', 'a' * 40, '1')


if __name__ == '__main__':
    unittest.main()
