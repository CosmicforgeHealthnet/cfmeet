"""certificate_monitor: lineage validation, alert bands, stable presentation, bounded
activation and recovery. Real openssl material; no network or Docker needed."""
import datetime
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import certificate_monitor as m
from test_private_tls import issue, make_ca

HOST = 'api.cfmeet.com'


class Lineage(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = Path(self.dir.name)
        self.ca = str(make_ca(self.root))

    def tearDown(self):
        self.dir.cleanup()

    def cfg(self):
        return {'lineage': str(self.root / 'live' / HOST), 'hostname': HOST, 'ca': self.ca, 'thresholds': [45, 30, 14, 7]}

    def test_a_valid_lineage_has_no_problem_and_reports_days_left(self):
        issue(self.root, 1, days=90)
        r = m.managed(self.cfg())
        self.assertIsNone(r['problem'])
        self.assertEqual(len(r['fingerprint']), 64)
        self.assertIn(r['remainingDays'], (88, 89, 90))

    def test_an_expired_certificate_is_never_loaded(self):
        issue(self.root, 1, days=30)
        later = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=31)
        r = m.managed(self.cfg(), now=later)
        self.assertEqual(r['problem'], 'certificate expired')
        self.assertLess(r['remainingDays'], 0)

    def test_wrong_hostname_foreign_root_mismatched_key_and_open_key_are_refused(self):
        issue(self.root, 1, name='other.example')
        self.assertEqual(m.managed(self.cfg())['problem'], 'chain does not verify for ' + HOST)
        issue(self.root, 2)
        cfg = self.cfg(); other = self.root / 'other'; other.mkdir()
        cfg['ca'] = str(make_ca(other))
        self.assertEqual(m.managed(cfg)['problem'], 'chain does not verify for ' + HOST)
        live = issue(self.root, 3)
        (live / 'privkey.pem').unlink(); (live / 'privkey.pem').symlink_to('../../archive/%s/privkey2.pem' % HOST)
        self.assertEqual(m.managed(self.cfg())['problem'], 'key does not match')
        live = issue(self.root, 4)
        (live / 'privkey.pem').resolve().chmod(0o640)
        self.assertEqual(m.managed(self.cfg())['problem'], 'private key readable by group or others')

    def test_a_missing_or_truncated_chain_is_reported(self):
        self.assertEqual(m.managed(self.cfg())['problem'], 'chain incomplete')
        live = issue(self.root, 1)
        (live / 'fullchain.pem').unlink(); (live / 'fullchain.pem').symlink_to('../../archive/%s/cert1.pem' % HOST)
        self.assertEqual(m.managed(self.cfg())['problem'], 'chain incomplete')


class Bands(unittest.TestCase):
    def test_each_threshold_and_expiry_has_its_band(self):
        t = [45, 30, 14, 7]
        cases = {90: None, 46: None, 45: 45, 31: 45, 30: 30, 15: 30, 14: 14, 8: 14, 7: 7, 0: 7, -1: 'EXPIRED', None: 'EXPIRED'}
        for days, expected in cases.items():
            self.assertEqual(m.band(days, t), expected, days)

    def test_warn_at_45_and_30_alert_at_14_7_expired_or_any_mismatch(self):
        ok_lineage, target = {'problem': None}, [{'matchesManaged': True}]
        t = [45, 30, 14, 7]
        self.assertEqual(m.classify(ok_lineage, target, None, t), 'OK')
        self.assertEqual(m.classify(ok_lineage, target, 45, t), 'WARN')
        self.assertEqual(m.classify(ok_lineage, target, 30, t), 'WARN')
        for b in (14, 7, 'EXPIRED'):
            self.assertEqual(m.classify(ok_lineage, target, b, t), 'ALERT')
        self.assertEqual(m.classify({'problem': 'key does not match'}, target, None, t), 'ALERT')
        self.assertEqual(m.classify(ok_lineage, [{'matchesManaged': False}], None, t), 'ALERT')


class Presentation(unittest.TestCase):
    cfg = {'hostname': HOST, 'ca': '/ca', 'stableHandshakes': 5, 'reloadAttempts': 3}

    def test_stable_requires_every_independent_handshake_to_agree(self):
        with patch.object(m, 'handshake', side_effect=['a'] * 5), patch.object(m.time, 'sleep'):
            self.assertEqual(m.stable(self.cfg, '10.0.0.1', 8443)[0], 'a')
        with patch.object(m, 'handshake', side_effect=['a', 'a', 'old', 'a', 'a']), patch.object(m.time, 'sleep'):
            self.assertIsNone(m.stable(self.cfg, '10.0.0.1', 8443)[0], 'one old worker answer is not stable')
        with patch.object(m, 'handshake', side_effect=[None] * 5), patch.object(m.time, 'sleep'):
            self.assertIsNone(m.stable(self.cfg, '10.0.0.1', 8443)[0])

    def test_reload_waits_for_a_complete_new_worker_generation(self):
        before = m.accepting_workers('PID COMMAND\n 8 nginx: master process nginx\n 9 nginx: worker process\n 10 nginx: worker process\n')
        during = m.accepting_workers(' 9 nginx: worker process is shutting down\n 10 nginx: worker process\n 17 nginx: worker process\n')
        after = m.accepting_workers(' 9 nginx: worker process is shutting down\n 17 nginx: worker process\n 18 nginx: worker process\n')
        self.assertEqual(before, {'9', '10'})
        self.assertTrue(during & before)
        self.assertFalse(after & before)

    def fake(self, reload_results):
        target = m.Target({'name': 'private-proxy', 'kind': 'container', 'container': 'c', 'network': 'n', 'port': 8443})
        target.reload = lambda results=iter(reload_results): next(results)
        target.address = lambda: '10.0.0.1'
        return target

    def test_activation_retries_a_bounded_number_of_times_then_reports_what_is_presented(self):
        sleeps = []
        with patch.object(m, 'stable', side_effect=[('old', ['old']), ('new', ['new'] * 5)]):
            shown, attempts, _ = m.activate(self.cfg, self.fake([True, True, True]), 'new', sleep=sleeps.append)
        self.assertEqual((shown, attempts), ('new', 2))
        self.assertEqual(sleeps, [1])
        with patch.object(m, 'stable', return_value=('old', ['old'] * 5)):
            shown, attempts, _ = m.activate(self.cfg, self.fake([False, False, False]), 'new', sleep=sleeps.append)
        self.assertEqual((shown, attempts), ('old', 3), 'a refused reload (nginx -t) keeps the running certificate')

    def test_invalid_material_is_never_activated_and_the_running_certificate_stays(self):
        cfg = dict(self.cfg, thresholds=[45, 30, 14, 7],
                   targets=[{'name': 'private-proxy', 'kind': 'container', 'container': 'c', 'network': 'n', 'port': 8443}])
        lineage = {'fingerprint': 'new', 'expiresAt': 'x', 'remainingDays': 60, 'problem': 'key does not match'}
        with patch.object(m, 'managed', return_value=lineage), patch.object(m.Target, 'ensure_running', return_value=None), \
             patch.object(m.Target, 'address', return_value='10.0.0.1'), patch.object(m, 'stable', return_value=('old', ['old'] * 5)), \
             patch.object(m, 'activate') as activate:
            status = m.check(cfg, '--reload')
        activate.assert_not_called()
        self.assertEqual(status['status'], 'ALERT')
        self.assertFalse(status['reloaded'])
        self.assertEqual(status['targets'][0]['presentedFingerprint'], 'old')

    def test_a_stale_target_is_repaired_by_the_daily_check(self):
        cfg = dict(self.cfg, thresholds=[45, 30, 14, 7],
                   targets=[{'name': 'host-nginx', 'kind': 'host-nginx', 'address': '127.0.0.1', 'port': 443}])
        lineage = {'fingerprint': 'new', 'expiresAt': 'x', 'remainingDays': 80, 'problem': None}
        with patch.object(m, 'managed', return_value=lineage), patch.object(m, 'stable', return_value=('old', ['old'] * 5)), \
             patch.object(m, 'activate', return_value=('new', 1, ['new'] * 5)):
            status = m.check(cfg, '--check')
        self.assertEqual(status['status'], 'OK')
        self.assertEqual(status['targets'][0]['reloadAttempts'], 1)
        self.assertTrue(status['presentedMatchesManaged'])

    def test_a_stopped_proxy_container_is_started_again(self):
        calls = []
        def run(*args, **kw):
            calls.append(args)
            if args[:3] == ('docker', 'inspect', '-f') and args[3] == '{{.State.Running}}':
                return 0, 'false\n', ''
            if args[:2] == ('docker', 'inspect'):
                return 0, '172.23.0.4\n', ''
            return 0, '', ''
        target = m.Target({'name': 'private-proxy', 'kind': 'container', 'container': 'c', 'network': 'n', 'port': 8443})
        with patch.object(m, 'run', side_effect=run):
            self.assertEqual(target.ensure_running(), 'started')
        self.assertIn(('docker', 'start', 'c'), calls)


class Main(unittest.TestCase):
    def test_status_is_written_atomically_and_alert_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as folder:
            folder = Path(folder)
            config = folder / 'c.json'
            config.write_text(json.dumps({'lineage': str(folder / 'missing'), 'hostname': HOST, 'ca': '/nonexistent',
                                          'status': str(folder / 'status.json'), 'thresholds': [45, 30, 14, 7], 'targets': []}))
            with patch.object(m, 'run', return_value=(0, '', '')):
                code = m.main(['--check', '--config', str(config)])
            status = json.loads((folder / 'status.json').read_text())
            mode = (folder / 'status.json').stat().st_mode & 0o777
            leftovers = list(folder.glob('*.tmp'))
        self.assertEqual(code, 1)
        self.assertEqual((status['status'], status['problem']), ('ALERT', 'chain incomplete'))
        self.assertEqual(mode, 0o600)
        self.assertEqual(leftovers, [])


if __name__ == '__main__':
    unittest.main()
