import datetime as dt
import json
import os
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

import audit_release as ar


class ReleaseHandoff(unittest.TestCase):
    def fake(self, swap_sources=True, fail=None):
        events = []
        def release(sha, run_id, before_source_swap):
            if swap_sources:
                before_source_swap()
            events.append('images')
            if fail:
                raise fail
        cr = SimpleNamespace(SERVICES={'messaging': {'container': 'cfmeet-messaging'}},
            container_names=lambda: ['cfmeet-messaging'], release_all=release,
            verify=lambda *_: events.append('verify'))
        return cr, events

    def test_source_upgrade_disables_before_images_then_prepares_and_enables_after(self):
        cr, events = self.fake()
        with patch.dict(os.environ, {'CFMEET_AUDIT_ACTIVATIONS': 'disabled'}), \
                patch.object(ar.policy, 'enabled', return_value=True), \
                patch.object(ar.policy, 'pause_for_release', side_effect=lambda *_: events.append('pause')), \
                patch.object(ar.policy, 'activate', side_effect=lambda *args: events.append(args[-1])) as activate:
            ar.release(cr, 'sha', '123-1')
        self.assertEqual(events, ['pause', 'images', 'prepare', 'enable', 'verify'])
        self.assertNotEqual(activate.call_args_list[0].args[2], activate.call_args_list[1].args[2])

    def test_unchanged_sources_keep_fast_mode_without_any_restart(self):
        cr, events = self.fake(swap_sources=False)
        with patch.dict(os.environ, {'CFMEET_AUDIT_ACTIVATIONS': 'disabled'}), \
                patch.object(ar.policy, 'enabled', return_value=True), \
                patch.object(ar.policy, 'pause_for_release') as pause, patch.object(ar.policy, 'activate') as activate:
            ar.release(cr, 'sha', '123-1')
        pause.assert_not_called()
        activate.assert_not_called()
        self.assertEqual(events, ['images'])

    def test_failed_upgrade_never_reenables_fast_mode_or_runs_campaign(self):
        cr, events = self.fake(fail=RuntimeError('image health'))
        with patch.dict(os.environ, {'CFMEET_AUDIT_ACTIVATIONS': 'enabled'}), \
                patch.object(ar.policy, 'enabled', return_value=True), \
                patch.object(ar.policy, 'pause_for_release', side_effect=lambda *_: events.append('pause')), \
                patch.object(ar.policy, 'activate') as activate, patch.object(ar, 'run') as campaign:
            with self.assertRaises(RuntimeError):
                ar.release(cr, 'sha', '123-1')
        self.assertEqual(events, ['pause', 'images'])
        activate.assert_not_called()
        campaign.assert_not_called()

    def test_failed_pause_prevents_source_swaps(self):
        cr, events = self.fake()
        with patch.object(ar.policy, 'enabled', return_value=True), \
                patch.object(ar.policy, 'pause_for_release', side_effect=RuntimeError('no ACK')):
            with self.assertRaises(RuntimeError):
                ar.release(cr, 'sha', '123-1')
        self.assertEqual(events, [])

    def test_campaign_is_opt_in_and_runs_after_verified_release(self):
        for setting, expected in [('enabled', True), ('disabled', False), ('', False)]:
            cr, events = self.fake()
            with patch.dict(os.environ, {'CFMEET_AUDIT_ACTIVATIONS': setting}), \
                    patch.object(ar.policy, 'enabled', return_value=False), \
                    patch.object(ar, 'run', side_effect=lambda *_: events.append('campaign')) as campaign:
                ar.release(cr, 'sha', '123-1')
            self.assertEqual(campaign.called, expected)
            self.assertEqual(events, ['images', 'campaign', 'verify'] if expected else ['images'])

    def test_owned_monitoring_exists_before_log_inventory_is_reconciled(self):
        cr, events = self.fake()
        with patch.dict(os.environ, {'CFMEET_AUDIT_ACTIVATIONS': 'enabled'}), \
                patch.object(ar.policy, 'enabled', return_value=False), \
                patch.object(ar, 'run', side_effect=lambda *_: events.append('campaign')):
            ar.release(cr, 'sha', '123-1', after_release=lambda: events.append('monitoring'))
        self.assertEqual(events, ['images', 'monitoring', 'campaign', 'verify'])


class Campaign(unittest.TestCase):
    def test_order_and_parent_attribution_under_one_release(self):
        with tempfile.TemporaryDirectory() as directory:
            events = []
            cr = SimpleNamespace(require_release=Mock(), emit=Mock())
            with patch.object(ar.p, 'ROOT', Path(directory)), \
                    patch.object(ar, 'ensure_witness', side_effect=lambda *_: events.append('witness')), \
                    patch.object(ar, 'ensure_logs', side_effect=lambda *_: events.append('logs')), \
                    patch.object(ar, 'ensure_recording', side_effect=lambda *_: events.append('recording')), \
                    patch.object(ar, 'ensure_send_policy', side_effect=lambda *_: events.append('send-policy')):
                ar.run(cr, 'a'*40, '123-1')
            self.assertEqual(events, ['witness', 'logs', 'recording', 'send-policy'])
            state = json.loads((Path(directory)/'audit-activation-campaign.json').read_text())
            self.assertEqual(set(state['completed']), set(ar.VERSIONS))
            self.assertNotIn('running', state)
            self.assertEqual(cr.emit.call_args.args[1]['result'], 'PASS')

    def test_failure_never_retries_on_unrelated_merges_and_never_emits_exception_text(self):
        with tempfile.TemporaryDirectory() as directory:
            cr = SimpleNamespace(require_release=Mock(), emit=Mock())
            root = Path(directory)
            with patch.object(ar.p, 'ROOT', root), patch.object(ar, 'ensure_witness'), \
                    patch.object(ar, 'ensure_logs', side_effect=RuntimeError('private-token')) as logs, \
                    patch.object(ar, 'ensure_recording') as recording, patch.object(ar, 'ensure_send_policy') as send:
                with self.assertRaises(RuntimeError):
                    ar.run(cr, 'a'*40, '123-1')
                ar.run(cr, 'b'*40, '124-1')
                self.assertEqual(logs.call_count, 1)
                self.assertEqual(cr.emit.call_args.args[1]['result'], 'BLOCKED')
                with patch.dict(ar.VERSIONS, logs=2):
                    with self.assertRaises(RuntimeError):
                        ar.run(cr, 'c'*40, '125-1')
                    ar.run(cr, 'd'*40, '126-1')
                self.assertEqual(logs.call_count, 2, 'one retry per reviewed stage version')
                recording.assert_not_called()
                send.assert_not_called()
            self.assertNotIn('private-token', (root/'audit-activation-campaign.json').read_text())
            self.assertNotIn('private-token', str(cr.emit.call_args_list))

    def test_interrupted_stage_is_blocked_without_repeating_mutations(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root/'audit-activation-campaign.json').write_text(json.dumps(
                {'completed': {}, 'running': {'stage': 'recording', 'version': 1}}))
            cr = SimpleNamespace(require_release=Mock(), emit=Mock())
            with patch.object(ar.p, 'ROOT', root), patch.object(ar, 'ensure_witness') as witness:
                ar.run(cr, 'a'*40, '123-1')
            witness.assert_not_called()
            self.assertEqual(cr.emit.call_args.args[1]['result'], 'BLOCKED')

    def test_existing_witness_is_proved_without_rotating_or_replacing_its_image(self):
        cr = SimpleNamespace(env_of=lambda _: {'MESSAGING_KT_WITNESSES_FILE': ar.witness.TARGET,
            'MESSAGING_KT_WITNESS_QUORUM': '1'}, container_names=lambda: [ar.witness.NAME])
        with patch.object(ar.p, 'inspect', return_value={'State': {'Running': True}}), \
                patch.object(ar.witness, 'prove_current_head') as prove, patch.object(ar.witness, 'activate') as activate:
            ar.ensure_witness(cr, 'sha', '123-1')
        prove.assert_called_once_with(cr)
        activate.assert_not_called()

    def test_recording_preparation_and_acceptance_have_distinct_child_runs(self):
        with patch.object(ar.recording, 'facts', return_value={'recordingEnabled': False}), \
                patch.object(ar.recording, 'activate') as activate, \
                patch.object(ar.audit, 'recording_acceptance_proven', return_value=True):
            ar.ensure_recording(Mock(), 'sha', '123-1')
        self.assertEqual([call.args[-1] for call in activate.call_args_list], ['prepare', 'enable-30-days'])
        self.assertNotEqual(activate.call_args_list[0].args[2], activate.call_args_list[1].args[2])

    def test_enabled_recording_without_acceptance_is_preserved_but_not_certified(self):
        with patch.object(ar.recording, 'facts', return_value={'recordingEnabled': True, 'recordingStorageConfigured': True}), \
                patch.object(ar.audit, 'recording_acceptance_proven', return_value=False), \
                patch.object(ar.recording, 'activate') as activate:
            with self.assertRaisesRegex(RuntimeError, 'acceptance receipt'):
                ar.ensure_recording(Mock(), 'sha', '123-1')
        activate.assert_not_called()

    def test_log_health_requires_coverage_freshness_and_no_failed_streams(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            unit = root/'collector.service'
            unit.write_text('# CF Meet owned service log collector\nTasksMax=2048\n')
            (root/'log-collector').mkdir()
            (root/'logs/services').mkdir(parents=True)
            (root/'log-collector/config.json').write_text(json.dumps(dict(directory=str(root/'logs/services'), containers=['cfmeet-messaging'])))
            cr = SimpleNamespace(SERVICES={'messaging': {'container': 'cfmeet-messaging'}})
            def run(*args):
                return 'cfmeet-messaging' if args[0] == 'docker' else 'active'
            for age, failures, expected in [(0, {}, True), (90, {}, False), (0, {'cfmeet-messaging': 1}, False)]:
                (root/'logs/services/health.json').write_text(json.dumps(dict(
                    at=(dt.datetime.now(dt.timezone.utc)-dt.timedelta(seconds=age)).isoformat(),
                    threadsAlive=1, containers=1, failures=failures)))
                with patch.object(ar.p, 'ROOT', root), patch.object(ar.audit, 'LOG_UNIT', unit), patch.object(ar.p, 'run', run):
                    self.assertEqual(ar.logs_ready(cr), expected)

    def test_invalid_active_policy_falls_back_before_reporting_failure(self):
        cr = SimpleNamespace(env_of=lambda _: {}, emit=Mock())
        with patch.object(ar.policy, 'enabled', return_value=True), \
                patch.object(ar.policy, 'facts', return_value={}), \
                patch.object(ar.policy, 'pause_for_release') as pause:
            with self.assertRaises(RuntimeError):
                ar.ensure_send_policy(cr, 'sha', '123-1')
        pause.assert_called_once_with(cr, 'sha', '123-1-audit-send-invalid')


if __name__ == '__main__':
    unittest.main()
