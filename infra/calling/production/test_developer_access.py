"""Offline tests: production-access review (named reviewers, manual decisions only)."""

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import coordinated_release as cr
import developer_access as da
import production as p
from test_coordinated_release import isolated_work

ALICE = 'github-alice=0b6f1c1e-2c1a-4a59-9d3e-2f6c8c1d9a01'


class Reviewers(unittest.TestCase):
    def test_reviewers_are_explicit_login_to_user_id_entries(self):
        self.assertEqual(da.parse_reviewers(ALICE + ', ' + ALICE.replace('alice', 'bob')),
                         {'github-alice': '0b6f1c1e-2c1a-4a59-9d3e-2f6c8c1d9a01',
                          'github-bob': '0b6f1c1e-2c1a-4a59-9d3e-2f6c8c1d9a01'})
        for bad in ('', 'alice', 'github-alice', 'github-alice=not-a-uuid', 'github-a;b=' + ALICE.split('=')[1]):
            with self.assertRaises(RuntimeError):
                da.parse_reviewers(bad)

    def test_activation_is_ordered_granted_and_refuses_without_reviewers(self):
        self.assertEqual(cr.ACTIVATIONS['activate-production-access-review'][0], ('activate-developer-console',))
        grants = [g for g in cr.GRANTS if g[0] == 'activate-production-access-review']
        self.assertEqual(grants, [('activate-production-access-review', 'developer-platform', 'operator-console',
                                   ('developer-platform.administer',))])
        self.assertEqual(cr.service_id('operator-console'), 'cfmeet.operator-console')
        with isolated_work():
            with self.assertRaisesRegex(RuntimeError, 'No production-access reviewers'):
                da.activate(cr, 'a' * 40, '1-1', '')

    def test_activation_enables_submissions_only_with_reviewers_and_manual_policy(self):
        calls = []
        with isolated_work() as work, patch.object(cr, 'env_of', lambda n: {}), \
                patch.object(da, 'image_has_tool', lambda c: True), \
                patch.object(da, 'ensure_operator_key', lambda c: None), \
                patch.object(cr, 'apply_activation', lambda *a, **k: calls.append(a)), \
                contextlib.redirect_stdout(io.StringIO()):
            (work / da.REVIEWERS_FILE).write_text(ALICE + '\n')
            da.activate(cr, 'a' * 40, '1-1', '')
        (op, _, _, changes), = calls
        self.assertEqual(op, 'activate-production-access-review')
        for service, flags in changes:
            self.assertIn(service, ('developer-platform', 'developer-platform-worker'))
            self.assertEqual(flags['DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS'], 'github-alice')
            self.assertEqual(flags['DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED'], 'true')
            self.assertNotIn('DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL', flags)

    def test_auto_approval_is_refused(self):
        with isolated_work() as work, \
                patch.object(cr, 'env_of', lambda n: {'DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL': 'true'}):
            (work / da.REVIEWERS_FILE).write_text(ALICE)
            with self.assertRaisesRegex(RuntimeError, 'Automatic'):
                da.activate(cr, 'a' * 40, '1-1', '')

    def test_certify_never_creates_the_operator_key(self):
        with tempfile.TemporaryDirectory() as folder, patch.object(p, 'SECRETS', Path(folder)):
            with self.assertRaises(RuntimeError):
                cr.issuer_key('operator-console')
            self.assertEqual(list(Path(folder).iterdir()), [])


class Review(unittest.TestCase):
    def run_review(self, doc, answer=None):
        calls = []
        with isolated_work() as work, patch.object(cr, 'done', lambda op: True), \
                patch.object(da, 'tool', lambda c, args, op=None, uid=None: calls.append((args, op, uid)) or (
                    answer or {'result': {'id': doc.get('requestId'), 'state': 'APPROVED'}})), \
                patch.object(da, 'notify_applicant', lambda *a: 'queued'), \
                contextlib.redirect_stdout(io.StringIO()) as out:
            (work / da.REVIEWERS_FILE).write_text(ALICE)
            (work / da.REVIEW_FILE).write_text(json.dumps(doc))
            da.review(cr, 'a' * 40, '1-1', '')
            self.assertFalse((work / da.REVIEW_FILE).exists(), 'the command is consumed')
        return calls, out.getvalue()

    def test_a_named_reviewer_approves_and_the_applicant_is_notified(self):
        rid = '9f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f'
        calls, out = self.run_review({'command': 'approve', 'operator': 'github-alice', 'requestId': rid})
        self.assertEqual(calls, [(['approve', rid], 'github-alice', '0b6f1c1e-2c1a-4a59-9d3e-2f6c8c1d9a01')])
        self.assertIn('"applicantNotification": "queued"', out)

    def test_unnamed_operators_and_reasonless_rejections_are_refused(self):
        rid = '9f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f'
        with self.assertRaisesRegex(RuntimeError, 'not a named'):
            self.run_review({'command': 'approve', 'operator': 'github-mallory', 'requestId': rid})
        with self.assertRaisesRegex(RuntimeError, 'reason code'):
            self.run_review({'command': 'reject', 'operator': 'github-alice', 'requestId': rid})
        with self.assertRaisesRegex(RuntimeError, 'Unknown'):
            self.run_review({'command': 'auto-approve', 'operator': 'github-alice'})


if __name__ == '__main__':
    unittest.main()
