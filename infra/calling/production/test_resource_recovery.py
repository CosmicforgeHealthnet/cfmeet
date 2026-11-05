import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import resource_recovery as r

SHA_RUNNING = 'a' * 40
SHA_ROLLBACK = 'b' * 40


def container(name, image_id, tag='', running=True, log='', rotated=False, labels=None, ports=None):
    return {'Name': '/' + name, 'Image': image_id, 'LogPath': log,
            'State': {'Running': running, 'StartedAt': '2026-11-05T05:13:00Z'},
            'Config': {'Image': tag, 'Labels': labels or {}},
            'HostConfig': {'PortBindings': ports or {}, 'Memory': 768 * 2**20,
                           'LogConfig': {'Type': 'json-file', 'Config': {'max-size': '20m'} if rotated else {}}}}


def image(image_id, tags, created='2026-11-01', source=''):
    return {'Id': image_id, 'RepoTags': tags, 'RepoDigests': [], 'Created': created, 'Size': 2**30,
            'Config': {'Labels': {'org.opencontainers.image.source': source} if source else {}}}


class RemovableImages(unittest.TestCase):
    def setUp(self):
        self.folder = tempfile.TemporaryDirectory(); self.addCleanup(self.folder.cleanup)
        root = Path(self.folder.name); (root / 'communications').mkdir()
        (root / 'communications' / 'rollback.private.json').write_text('{"sha":"%s"}' % SHA_ROLLBACK)
        (root / 'calling-activation-rollback.json').write_text('{"previous":"cfmeet-meeting-service:prod-534af95"}')
        patcher = patch.object(r, 'ROOT', root); patcher.start(); self.addCleanup(patcher.stop)

    def test_only_unused_unreferenced_cfmeet_images_are_selected(self):
        containers = [container('cfmeet-calling', 'sha256:run', 'ghcr.io/cosmicforgehealthnet/cfmeet-calling:' + SHA_RUNNING),
                      container('cfmeet-production-backend-rollback-1', 'sha256:inuse-meetings', running=False)]
        images = [
            image('sha256:run', ['ghcr.io/cosmicforgehealthnet/cfmeet-calling:' + SHA_RUNNING]),
            image('sha256:rollback', ['ghcr.io/cosmicforgehealthnet/cfmeet-messaging:' + SHA_ROLLBACK]),
            image('sha256:other', ['karrygo-api:latest']),
            image('sha256:other-dangling', []),
            image('sha256:other-sourced', ['admin-server:1'], source='https://github.com/someone/admin-server'),
            image('sha256:journal', ['cfmeet-meeting-service:prod-534af95'], created='2026-10-20'),
            image('sha256:old', ['cfmeet-meeting-service:prod-fd757bc'], created='2026-10-06'),
            image('sha256:newest', ['cfmeet-meeting-service:prod-6c39351'], created='2026-10-25'),
            image('sha256:inuse-meetings', ['cfmeet-meeting-service:prod-9999999'], created='2026-10-30'),
            image('sha256:latest', ['cfmeet-meeting-service:prod-latest'], created='2026-10-01'),
            image('sha256:probe', ['cfmeet-calling-web-probe:1.56.1']),
            image('sha256:shared', ['cfmeet-calling-api-probe:aa9d', 'ghcr.io/cosmicforgehealthnet/cfmeet-calling:' + SHA_ROLLBACK]),
        ]
        self.assertEqual(sorted(r.removable_images(containers, images)),
                         ['cfmeet-calling-web-probe:1.56.1', 'cfmeet-meeting-service:prod-fd757bc'])

    def test_long_lived_unrotated_logs_only(self):
        containers = [container('lk-server', 'x', log='/var/lib/docker/containers/1/1-json.log'),
                      container('cfmeet-core-db', 'x', log='/var/lib/docker/containers/2/2-json.log', rotated=True),
                      container('cfmeet-calling', 'x', log='/var/lib/docker/containers/3/3-json.log'),
                      container('karrygo-api', 'x', log='/var/lib/docker/containers/4/4-json.log'),
                      container('cfmeet-calling-redis', 'x', running=False, log='/var/lib/docker/containers/5/5-json.log')]
        config = r.logrotate_config(containers)
        self.assertIn('/1/1-json.log', config)
        for absent in ('/2/', '/3/', '/4/', '/5/'):
            self.assertNotIn(absent, config)
        self.assertIn('copytruncate', config)
        self.assertNotIn('postrotate', config)


class StageSafety(unittest.TestCase):
    def test_active_stage_refuses_to_stop_anything(self):
        candidates = [container('cfmeet-comm-stage-calling', 'x', labels={r.CANDIDATE_LABEL: 'true'})]
        calls = []
        with patch.object(r, 'memory', return_value={}), patch.object(r, 'containers', return_value=candidates), \
             patch.object(r, 'stage_db_activity', return_value={'stage_active_calls': 1, 'stage_db_client_connections': 0}), \
             patch.object(r, 'sh', side_effect=lambda *a, **k: calls.append(a) or ''), self.assertRaises(AssertionError):
            r.recover('1')
        self.assertEqual(calls, [])

    def test_unlabeled_or_published_candidates_are_refused(self):
        for c in (container('cfmeet-comm-stage-calling', 'x'),
                  container('cfmeet-comm-stage-calling', 'x', labels={r.CANDIDATE_LABEL: 'true'}, ports={'80/tcp': [{}]})):
            calls = []
            with patch.object(r, 'memory', return_value={}), patch.object(r, 'containers', return_value=[c]), \
                 patch.object(r, 'stage_db_activity', return_value={'stage_active_calls': 0, 'stage_db_client_connections': 0}), \
                 patch.object(r, 'sh', side_effect=lambda *a, **k: calls.append(a) or ''), self.assertRaises(AssertionError):
                r.recover('1')
            self.assertEqual(calls, [])


if __name__ == '__main__':
    unittest.main()
