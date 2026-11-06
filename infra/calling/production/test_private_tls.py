"""Private TLS proxy: durable certificate mounts, material validation and renewal hook."""
import datetime
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

import private_tls as tls


def openssl(*args, cwd):
    subprocess.run(['openssl', *args], cwd=cwd, check=True, capture_output=True)


def make_ca(folder):
    openssl('req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-keyout', 'root.key', '-out', 'root.pem',
            '-days', '3650', '-subj', '/CN=CI Root', '-addext', 'basicConstraints=critical,CA:true',
            '-addext', 'keyUsage=critical,keyCertSign,cRLSign', cwd=folder)
    openssl('req', '-newkey', 'rsa:2048', '-nodes', '-keyout', 'int.key', '-out', 'int.csr', '-subj', '/CN=CI Int', cwd=folder)
    (folder / 'int.ext').write_text('basicConstraints=critical,CA:true,pathlen:0\nkeyUsage=critical,keyCertSign,cRLSign\n')
    openssl('x509', '-req', '-in', 'int.csr', '-CA', 'root.pem', '-CAkey', 'root.key', '-CAcreateserial',
            '-out', 'int.pem', '-days', '3650', '-extfile', 'int.ext', cwd=folder)
    return folder / 'root.pem'


def issue(folder, n, days=90, name=tls.HOSTNAME, start=None):
    """A leaf for `name` signed by the CI intermediate, laid out like certbot: numbered
    archive files and relative live/ symlinks."""
    archive, live = folder / 'archive' / tls.HOSTNAME, folder / 'live' / tls.HOSTNAME
    archive.mkdir(parents=True, exist_ok=True); live.mkdir(parents=True, exist_ok=True)
    openssl('req', '-newkey', 'rsa:2048', '-nodes', '-keyout', str(archive / ('privkey%d.pem' % n)),
            '-out', 'leaf.csr', '-subj', '/CN=' + name, cwd=folder)
    (folder / 'leaf.ext').write_text('subjectAltName=DNS:%s\nextendedKeyUsage=serverAuth\nkeyUsage=critical,digitalSignature,keyEncipherment\n' % name)
    args = ['x509', '-req', '-in', 'leaf.csr', '-CA', 'int.pem', '-CAkey', 'int.key', '-CAcreateserial',
            '-out', 'leaf.pem', '-days', str(days), '-extfile', 'leaf.ext']
    openssl(*args, cwd=folder)
    (archive / ('privkey%d.pem' % n)).chmod(0o600)
    leaf, inter = (folder / 'leaf.pem').read_text(), (folder / 'int.pem').read_text()
    (archive / ('cert%d.pem' % n)).write_text(leaf)
    (archive / ('chain%d.pem' % n)).write_text(inter)
    (archive / ('fullchain%d.pem' % n)).write_text(leaf + inter)
    for kind in ('cert', 'chain', 'fullchain', 'privkey'):
        link = live / (kind + '.pem')
        tmp = live / (kind + '.pem.new')
        tmp.symlink_to('../../archive/%s/%s%d.pem' % (tls.HOSTNAME, kind, n))
        os.replace(tmp, link)  # certbot repoints atomically
    return live


class Material(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.folder = Path(self.dir.name)
        self.ca = str(make_ca(self.folder))

    def tearDown(self):
        self.dir.cleanup()

    def test_valid_lineage_is_accepted_with_its_facts(self):
        live = issue(self.folder, 1)
        facts = tls.validate_material(live, ca=self.ca)
        self.assertIn(tls.HOSTNAME, facts['names'])
        self.assertGreaterEqual(facts['remainingDays'], 85)
        self.assertEqual(facts['keyMode'], '0o600')
        self.assertEqual(len(facts['fingerprint']), 64)

    def test_renewal_changes_what_the_live_names_resolve_to(self):
        first = tls.validate_material(issue(self.folder, 1), ca=self.ca)
        second = tls.validate_material(issue(self.folder, 2), ca=self.ca)
        self.assertNotEqual(first['fingerprint'], second['fingerprint'])

    def test_wrong_name_short_expiry_foreign_key_and_open_key_are_refused(self):
        live = issue(self.folder, 1, name='other.example')
        with self.assertRaisesRegex(AssertionError, 'does not name'):
            tls.validate_material(live, ca=self.ca)
        live = issue(self.folder, 2, days=3)
        with self.assertRaisesRegex(AssertionError, 'too soon'):
            tls.validate_material(live, ca=self.ca)
        live = issue(self.folder, 3)
        other = issue(self.folder, 4)
        (live / 'privkey.pem').unlink()
        (live / 'privkey.pem').symlink_to('../../archive/%s/privkey3.pem' % tls.HOSTNAME)
        (live / 'fullchain.pem').unlink()
        (live / 'fullchain.pem').symlink_to('../../archive/%s/fullchain4.pem' % tls.HOSTNAME)
        with self.assertRaisesRegex(AssertionError, 'does not match'):
            tls.validate_material(other, ca=self.ca)
        live = issue(self.folder, 5)
        (live / 'privkey.pem').resolve().chmod(0o644)
        with self.assertRaisesRegex(AssertionError, 'readable'):
            tls.validate_material(live, ca=self.ca)

    def test_a_chain_from_another_root_is_refused(self):
        live = issue(self.folder, 1)
        other = Path(self.dir.name) / 'other'
        other.mkdir()
        with self.assertRaisesRegex(AssertionError, 'does not verify'):
            tls.validate_material(live, ca=str(make_ca(other)))


PROXY = {
    'Id': 'f' * 64, 'Image': 'sha256:proxy', 'Name': '/cfmeet-core-tls',
    'Config': {'Image': 'nginx:stable-alpine', 'Hostname': 'ffffffffffff', 'Env': ['PATH=/usr/bin'], 'Cmd': ['nginx', '-g', 'daemon off;']},
    'HostConfig': {'RestartPolicy': {'Name': 'unless-stopped'}, 'PortBindings': {}, 'Binds': None, 'Mounts': [
        {'Type': 'bind', 'Source': '/opt/cfmeet-core/tls-proxy/nginx.conf', 'Target': '/etc/nginx/nginx.conf', 'ReadOnly': True},
        {'Type': 'bind', 'Source': '/etc/letsencrypt/archive/api.cfmeet.com/fullchain4.pem', 'Target': '/etc/nginx/tls/fullchain.pem', 'ReadOnly': True},
        {'Type': 'bind', 'Source': '/etc/letsencrypt/archive/api.cfmeet.com/privkey4.pem', 'Target': '/etc/nginx/tls/privkey.pem', 'ReadOnly': True}]},
    'Mounts': [
        {'Source': '/opt/cfmeet-core/tls-proxy/nginx.conf', 'Destination': '/etc/nginx/nginx.conf', 'RW': False},
        {'Source': '/etc/letsencrypt/archive/api.cfmeet.com/fullchain4.pem', 'Destination': '/etc/nginx/tls/fullchain.pem', 'RW': False},
        {'Source': '/etc/letsencrypt/archive/api.cfmeet.com/privkey4.pem', 'Destination': '/etc/nginx/tls/privkey.pem', 'RW': False}],
    'NetworkSettings': {'Networks': {'cfmeet-core-private': {'Aliases': ['api.cfmeet.com', 'ffffffffffff'], 'IPAddress': '172.23.0.4'}}},
}
CONF = 'http {\n  ssl_certificate /etc/nginx/tls/fullchain.pem;\n  ssl_certificate_key /etc/nginx/tls/privkey.pem;\n  server { listen 8443 ssl; }\n}\n'


class DurableShape(unittest.TestCase):
    def test_pinned_archive_files_become_read_only_lineage_directories(self):
        payload = tls.durable_payload(PROXY)
        mounts = payload['HostConfig']['Mounts']
        self.assertEqual([m['Target'] for m in mounts],
                         ['/etc/nginx/nginx.conf', str(tls.LIVE), str(tls.ARCHIVE)])
        self.assertTrue(all(m['ReadOnly'] for m in mounts))
        self.assertTrue(all(m['Source'] == m['Target'] for m in mounts[1:]))
        self.assertFalse(any('fullchain4' in json.dumps(m) for m in mounts))
        self.assertEqual(payload['Image'], 'sha256:proxy')
        self.assertNotIn('Hostname', payload)
        self.assertEqual(payload['HostConfig']['RestartPolicy'], {'Name': 'unless-stopped'})
        self.assertEqual(payload['NetworkingConfig']['EndpointsConfig']['cfmeet-core-private']['Aliases'],
                         ['api.cfmeet.com', 'cfmeet-core-tls'])

    def test_published_ports_or_unexpected_mounts_are_refused(self):
        bad = json.loads(json.dumps(PROXY)); bad['HostConfig']['PortBindings'] = {'8443/tcp': [{'HostPort': '8443'}]}
        with self.assertRaises(AssertionError): tls.durable_payload(bad)
        bad = json.loads(json.dumps(PROXY)); bad['HostConfig']['Mounts'] = bad['HostConfig']['Mounts'][:2]
        with self.assertRaises(AssertionError): tls.durable_payload(bad)

    def test_configuration_points_at_the_live_names_exactly_once(self):
        text = tls.durable_conf(CONF)
        for directive in tls.NEW_DIRECTIVES:
            self.assertEqual(text.count(directive), 1)
        self.assertNotIn('/etc/nginx/tls/', text)
        with self.assertRaises(AssertionError): tls.durable_conf(text)

    def test_only_the_lineage_mount_layout_counts_as_durable(self):
        self.assertFalse(tls.is_durable(PROXY, CONF))
        durable = json.loads(json.dumps(PROXY))
        durable['Mounts'] = [durable['Mounts'][0]] + [{'Source': str(d), 'Destination': str(d), 'RW': False} for d in (tls.LIVE, tls.ARCHIVE)]
        self.assertTrue(tls.is_durable(durable, tls.durable_conf(CONF)))
        self.assertFalse(tls.is_durable(durable, CONF))
        writable = json.loads(json.dumps(durable)); writable['Mounts'][1]['RW'] = True
        self.assertFalse(tls.is_durable(writable, tls.durable_conf(CONF)))


class StartupWait(unittest.TestCase):
    def test_a_proxy_that_is_still_starting_is_waited_for_within_the_bound(self):
        calls, now = [], [0.0]
        def probe(address, port):
            calls.append(address)
            if len(calls) < 3:
                raise AssertionError('Presented certificate does not verify (connect: Connection refused)')
            return {'fingerprint': 'f'}
        def sleep(seconds):
            now[0] += seconds
        self.assertEqual(tls.presented_eventually('172.23.0.9', probe=probe, sleep=sleep, clock=lambda: now[0]), {'fingerprint': 'f'})
        self.assertEqual(len(calls), 3)

    def test_a_proxy_that_never_verifies_still_fails_with_its_reason(self):
        now = [0.0]
        def probe(address, port):
            raise AssertionError('Presented certificate does not verify (verify error:num=20)')
        def sleep(seconds):
            now[0] += seconds
        with self.assertRaisesRegex(AssertionError, 'num=20'):
            tls.presented_eventually('172.23.0.9', timeout=5, probe=probe, sleep=sleep, clock=lambda: now[0])
        self.assertGreaterEqual(now[0], 5)

    def test_failure_classes_carry_no_certificate_or_key_material(self):
        self.assertEqual(tls.handshake_failure('', 'connect:errno=111'), 'connect: connect:errno')
        self.assertEqual(tls.handshake_failure('verify error:num=20:unable to get local issuer certificate\n-----BEGIN CERTIFICATE-----\nAAA', ''),
                         'verify error:num=20:unable to get local issuer certificate')


class RenewalHook(unittest.TestCase):
    def test_hook_runs_only_for_this_lineage_and_validates_before_reloading(self):
        hook = tls.hook_source()
        self.assertIn('*" %s "*' % tls.HOSTNAME, hook)
        self.assertIn('--reload', hook)
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'hook'
            path.write_text(hook.replace('exec /usr/bin/python3', 'echo RAN'))
            path.chmod(0o700)
            other = subprocess.run([str(path)], env={'RENEWED_DOMAINS': 'cfmeet.com www.cfmeet.com'}, capture_output=True, text=True)
            ours = subprocess.run([str(path)], env={'RENEWED_DOMAINS': 'cfmeet.com %s' % tls.HOSTNAME}, capture_output=True, text=True)
        self.assertEqual((other.returncode, other.stdout), (0, ''))
        self.assertIn('RAN', ours.stdout)

    def test_monitor_is_valid_python_that_never_reloads_unvalidated_material(self):
        source = tls.monitor_source()
        compile(source, 'certificate-monitor.py', 'exec')
        self.assertIn("if problem is None and (mode == '--reload' or current != fp)", source)
        self.assertIn("'nginx', '-t'", source)

    def test_monitor_reports_an_alert_for_a_broken_lineage(self):
        with tempfile.TemporaryDirectory() as folder:
            folder = Path(folder)
            live = folder / 'live'; live.mkdir()
            (live / 'fullchain.pem').write_text('not a certificate')
            (live / 'privkey.pem').write_text('')
            saved = (tls.LIVE, tls.STATUS, tls.CONTAINER)
            tls.LIVE, tls.STATUS, tls.CONTAINER = live, folder / 'status.json', 'cfmeet-ci-absent-proxy'
            try:
                script = folder / 'monitor.py'; script.write_text(tls.monitor_source())
            finally:
                tls.LIVE, tls.STATUS, tls.CONTAINER = saved
            r = subprocess.run(['python3', str(script), '--check'], capture_output=True, text=True)
            status = json.loads((folder / 'status.json').read_text())
        self.assertEqual(r.returncode, 1)
        self.assertEqual(status['status'], 'ALERT')
        self.assertEqual(status['problem'], 'chain incomplete')
        self.assertFalse(status['reloaded'])


if __name__ == '__main__':
    unittest.main()
