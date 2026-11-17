"""Offline tests of the production monitoring stack (monitoring_stack.py)."""
import json
import re
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import coordinated_release as cr
import monitoring_stack as ms
import production as p

REPO = Path(__file__).resolve().parents[3]
MONITORING = REPO / 'infra' / 'monitoring'
ALERTMANAGER = (MONITORING / 'alertmanager' / 'alertmanager.yml').read_text()


def yaml_or_skip(test):
    try:
        import yaml
    except ImportError:
        test.skipTest('PyYAML unavailable')
    return yaml


class Alertmanager(unittest.TestCase):
    def test_without_receiver_secrets_no_integration_remains_and_routing_is_kept(self):
        yaml = yaml_or_skip(self)
        text = ms.render_alertmanager(ALERTMANAGER, set())
        self.assertNotIn('webhook_configs', text)
        self.assertNotIn('slack_configs', text)
        self.assertNotRegex(text, r'(?m)^\s+-?\s*(api_)?url_file:')
        doc = yaml.safe_load(text)
        self.assertEqual([r['name'] for r in doc['receivers']], ['pager', 'operations', 'info-only'])
        self.assertTrue(all(set(r) == {'name'} for r in doc['receivers']))
        self.assertEqual(doc['route'], yaml.safe_load(ALERTMANAGER)['route'])

    def test_each_secret_enables_only_its_own_integration(self):
        yaml = yaml_or_skip(self)
        doc = yaml.safe_load(ms.render_alertmanager(ALERTMANAGER, {'webhook_configs'}))
        receivers = {r['name']: r for r in doc['receivers']}
        for name in ('pager', 'operations'):
            self.assertEqual(receivers[name]['webhook_configs'][0]['url_file'],
                             '/etc/alertmanager/secrets/alert-webhook-url')
            self.assertNotIn('slack_configs', receivers[name])
        self.assertEqual(set(receivers['info-only']), {'name'})
        both = ms.render_alertmanager(ALERTMANAGER, set(ms.RECEIVER_SECRETS))
        self.assertEqual(both, ALERTMANAGER if ALERTMANAGER.endswith('\n') else ALERTMANAGER + '\n')

    def test_committed_routing_reads_every_receiver_from_a_mounted_secret_file(self):
        targets = {'/etc/alertmanager/secrets/' + t for _s, _f, t in ms.RECEIVER_SECRETS.values()}
        found = set(re.findall(r'(?:api_url_file|url_file): (\S+)', ALERTMANAGER))
        self.assertEqual(found, targets)
        self.assertNotIn('email_configs', ALERTMANAGER)
        self.assertNotRegex(ALERTMANAGER, r'(?i)smtp|resend')

    def test_drop_integration_keeps_siblings_and_following_keys(self):
        text = 'a:\n  - name: x\n    webhook_configs:\n      - url_file: /f\n    other: 1\n  - name: y\n'
        self.assertEqual(ms.drop_integration(text, 'webhook_configs'), 'a:\n  - name: x\n    other: 1\n  - name: y\n')


class ScrapeJobs(unittest.TestCase):
    def envs(self, **extra):
        envs = {name: {} for name in cr.SERVICES}
        envs.update(extra)
        return envs

    def test_only_existing_non_loopback_listeners_are_scraped(self):
        envs = self.envs(messaging={'MESSAGING_METRICS_ADDRESS': '0.0.0.0:9197'},
                         calling={'CALLING_METRICS_ADDRESS': '127.0.0.1:9464'})
        jobs = {j['job_name']: j for j in ms.scrape_jobs(cr.SERVICES, envs)}
        self.assertEqual(jobs['messaging']['static_configs'], [{'targets': ['cfmeet-messaging:9197']}])
        for absent in ('calling', 'platform-access', 'telephony', 'meeting-service', 'security-scan-gateway',
                       'developer-platform-heartbeat'):
            self.assertNotIn(absent, jobs)
        self.assertEqual(jobs['notifications']['static_configs'][0]['targets'],
                         ['cfmeet-notifications:8102', 'cfmeet-notifications-2:8102'])

    def test_no_job_is_invented_for_a_service_without_a_metrics_route(self):
        envs = self.envs(**{name: {'X_METRICS_ADDRESS': '0.0.0.0:1'} for name in
                            ('api-gateway', 'developer-platform', 'organizations', 'communications')})
        jobs = {j['job_name'] for j in ms.scrape_jobs(cr.SERVICES, envs)}
        self.assertTrue(jobs.isdisjoint({'api-gateway', 'developer-platform', 'organizations', 'communications'}))

    def test_health_probes_cover_every_running_service_with_a_listener(self):
        envs = self.envs()
        del envs['telephony']
        jobs = {j['job_name']: j for j in ms.scrape_jobs(cr.SERVICES, envs)}
        probes = {c['labels']['service']: c['targets'][0] for c in jobs['health-probes']['static_configs']}
        self.assertEqual(probes['api-gateway'], 'http://cfmeet-api-gateway:8099/ready')
        self.assertEqual(probes['developer-platform'], 'http://cfmeet-developer-platform:8098/ready')
        self.assertEqual(probes['organizations'], 'http://cfmeet-organizations:8101/ready')
        self.assertEqual(probes['communications'], 'http://cfmeet-communications:8080/ready')
        self.assertNotIn('developer-platform-worker', probes)  # no listener: heartbeat instead
        self.assertNotIn('telephony', probes)  # not running
        relabel = jobs['health-probes']['relabel_configs'][-1]
        self.assertEqual(relabel, {'target_label': '__address__', 'replacement': 'cfmeet-blackbox:9115'})

    def test_meeting_metrics_use_the_internal_token_file_only(self):
        jobs = {j['job_name']: j for j in ms.scrape_jobs(cr.SERVICES, self.envs(), '/etc/prometheus/secrets/t')}
        job = jobs['meeting-service']
        self.assertEqual(job['metrics_path'], '/internal/metrics')
        self.assertEqual(job['http_headers'], {'X-Internal-Token': {'files': ['/etc/prometheus/secrets/t']}})

    def test_every_job_a_rule_names_can_be_produced(self):
        with tempfile.TemporaryDirectory() as out:
            produced = set(ms.render_example(cr.SERVICES, MONITORING, out))
            prometheus = json.loads((Path(out) / 'prometheus' / 'prometheus.yml').read_text())
        self.assertEqual(prometheus['rule_files'], ['/etc/prometheus/rules/*.rules.yml'])
        self.assertEqual(prometheus['alerting']['alertmanagers'][0]['static_configs'][0]['targets'],
                         ['cfmeet-alertmanager:9093'])
        named = set()
        for rules in (MONITORING / 'prometheus' / 'rules').glob('*.rules.yml'):
            named.update(re.findall(r'job="([a-z-]+)"', rules.read_text()))
        self.assertTrue(named, 'rules name no job')
        self.assertLessEqual(named, produced)

    def test_listener_target(self):
        self.assertEqual(ms.listener_target('c', '0.0.0.0:9197'), 'c:9197')
        self.assertEqual(ms.listener_target('c', '[::]:9197'), 'c:9197')
        for value in (None, '', '127.0.0.1:9197', 'localhost:1', 'nonsense', '0.0.0.0:x'):
            self.assertIsNone(ms.listener_target('c', value), value)


class Containers(unittest.TestCase):
    def test_every_container_is_private_bounded_read_only_and_unprivileged(self):
        for component in ms.CONTAINERS:
            args = ms.create_args(component, 'h' * 32, '/opt/cfmeet-core/monitoring/x')
            self.assertNotIn('--publish', args)
            self.assertNotIn('-p', args)
            self.assertEqual(args[args.index('--network') + 1], p.NET)
            self.assertEqual(args[args.index('--user') + 1], '65534:65534')
            for flag in ('--read-only', '--memory', '--memory-swap', '--pids-limit'):
                self.assertIn(flag, args)
            self.assertEqual(args[args.index('--cap-drop') + 1], 'ALL')
            self.assertIn(ms.IMAGES[component], args)
            self.assertRegex(ms.IMAGES[component], r'^[a-z0-9_./-]+:[A-Za-z0-9._-]+@sha256:[0-9a-f]{64}$')
            mounts = [args[i + 1] for i, a in enumerate(args) if a == '--mount']
            self.assertTrue(any(m.endswith(',readonly') and m.startswith('type=bind') for m in mounts))

    def test_ci_validates_the_images_that_are_deployed(self):
        workflow = (REPO / '.github/workflows/service-boundaries.yml').read_text()
        for component in ms.IMAGES:
            self.assertIn(ms.IMAGES[component], workflow, component)

    def test_only_its_own_containers_are_touched(self):
        for name in ms.CONTAINERS.values():
            ms._own(name)
            self.assertTrue(name.startswith('cfmeet-'))
            self.assertFalse(cr.owned(name), 'release ownership must not cover monitoring containers')
        for other in ('cfmeet-api-gateway', 'cosmicforge-scan-gateway', 'cfmeet-core-db', 'lk-server'):
            with self.assertRaises(RuntimeError):
                ms._own(other)

    def test_publish_is_content_addressed_and_prune_keeps_the_live_directory(self):
        with tempfile.TemporaryDirectory() as root, patch.object(ms, 'ROOT', Path(root)), \
                patch.object(ms.os, 'chown', lambda *a: None):
            first = ms.staging_dir('blackbox')
            (first / 'blackbox.yml').write_text('a')
            one = ms.publish('blackbox', first)
            again = ms.staging_dir('blackbox')
            (again / 'blackbox.yml').write_text('a')
            self.assertEqual(ms.publish('blackbox', again), one)
            changed = ms.staging_dir('blackbox')
            (changed / 'blackbox.yml').write_text('b')
            two = ms.publish('blackbox', changed)
            self.assertNotEqual(one, two)
            self.assertTrue(one.exists() and two.exists())
            ms.prune('blackbox', two)
            self.assertFalse(one.exists())
            self.assertTrue(two.exists())
            ms.prune('blackbox', None)
            self.assertFalse(two.exists())


class ReleaseIntegration(unittest.TestCase):
    def test_a_monitoring_failure_never_fails_the_release(self):
        events = []
        with patch.object(ms, 'deploy', side_effect=RuntimeError('boom https://x.invalid/secret')):
            self.assertIsNone(ms.deploy_after_release(cr, lambda k, v: events.append((k, v))))
        self.assertEqual(events[0][0], 'monitoringStack')
        self.assertEqual(events[0][1]['status'], 'NOT_UPDATED')
        self.assertNotIn('x.invalid', json.dumps(events))

    def test_operation_and_workflow_wiring(self):
        self.assertIn('deploy-monitoring', cr.OPERATIONS)
        workflow = (REPO / '.github/workflows/cfmeet-coordinated-release.yml').read_text()
        self.assertIn('cp -R infra/monitoring infra/calling/production/monitoring-config', workflow)
        for secret, filename, _target in ms.RECEIVER_SECRETS.values():
            self.assertIn('secrets.%s' % secret, workflow)
            self.assertIn('%s:%s' % (secret, filename), workflow)

    def test_heartbeat_role_is_read_only_and_limited_to_one_table(self):
        sql = ms.MONITOR_SQL.format(role=ms.MONITOR_ROLE, password='x', db=ms.MONITOR_DB) \
            + ms.MONITOR_GRANT_SQL.format(role=ms.MONITOR_ROLE, table=ms.HEARTBEAT_TABLE)
        self.assertIn('default_transaction_read_only = on', sql)
        grants = re.findall(r'GRANT ([A-Z, ]+) ON', sql)
        self.assertEqual(sorted(grants), ['CONNECT', 'SELECT', 'USAGE'])
        self.assertIn('ON TABLE devplat_worker_heartbeats', sql)
        self.assertNotRegex(sql, r'(?i)\b(INSERT|UPDATE|DELETE|TRUNCATE|ALL PRIVILEGES|SUPERUSER )\b')
        collector = (MONITORING / 'sql-exporter' / 'developer-platform-worker.collector.yml').read_text()
        for query in re.findall(r'query: \|\n((?:      .*\n)+)', collector):
            self.assertTrue(query.strip().upper().startswith('SELECT'), query)
            self.assertIn('devplat_worker_heartbeats', query)


if __name__ == '__main__':
    unittest.main()
