"""certificate_notify: when operators are told, and that delivery is verified."""
import unittest

import certificate_notify as n

FP = 'a' * 64
OK = {'status': 'WARN', 'band': 45, 'managedFingerprint': FP, 'presentedMatchesManaged': True, 'remainingDays': 45,
      'expiresAt': '2026-12-21T19:28:18+00:00', 'problem': None, 'checkedAt': 'now',
      'targets': [{'name': 'private-proxy', 'matchesManaged': True, 'stable': True, 'handshakes': 5, 'reloadAttempts': 0, 'recovered': None}]}
PUBLIC = {'fingerprint': FP, 'matchesManaged': True}


class FakeGitHub:
    def __init__(self, comments=(), drop=False):
        self.issue = {'number': 40, 'html_url': 'https://github.com/o/r/issues/40'}
        self.store = list(comments); self.drop = drop; self.created = None

    def tracking_issue(self, owner):
        return self.issue

    def comments(self, number):
        return self.store

    def post(self, number, body):
        self.created = {'id': 900 + len(self.store), 'html_url': 'https://github.com/o/r/issues/40#c', 'body': body}
        self.store.append(self.created); return self.created

    def read_comment(self, comment_id):
        return {} if self.drop else next(c for c in self.store if c.get('id') == comment_id)


class Notify(unittest.TestCase):
    def test_first_report_posts_and_is_read_back(self):
        gh = FakeGitHub()
        r = n.notify(gh, OK, PUBLIC, False, 'run', 'owner')
        self.assertEqual((r['posted'], r['delivered'], r['status'], r['reason']), (True, True, 'WARN', 'state changed'))
        self.assertIn('band: 45', gh.created['body'])

    def test_an_unchanged_state_is_not_reposted_but_alerts_repeat_daily(self):
        gh = FakeGitHub()
        n.notify(gh, OK, PUBLIC, False, 'run', 'owner')
        self.assertFalse(n.notify(gh, OK, PUBLIC, False, 'run', 'owner')['posted'])
        alert = dict(OK, status='ALERT', band=14)
        self.assertTrue(n.notify(gh, alert, PUBLIC, False, 'run', 'owner')['posted'])
        self.assertTrue(n.notify(gh, alert, PUBLIC, False, 'run', 'owner')['posted'])

    def test_a_public_certificate_other_than_the_managed_one_is_an_alert(self):
        r = n.notify(FakeGitHub(), OK, {'fingerprint': 'b' * 64, 'matchesManaged': False}, False, 'run', 'owner')
        self.assertEqual(r['status'], 'ALERT')
        self.assertEqual(n.effective_status({}, PUBLIC), 'ALERT', 'a missing monitor status is never OK')

    def test_delivery_is_only_reported_when_the_comment_reads_back(self):
        r = n.notify(FakeGitHub(drop=True), OK, PUBLIC, True, 'run', 'owner')
        self.assertTrue(r['posted'])
        self.assertFalse(r['delivered'])

    def test_comments_carry_no_secret_material(self):
        body = n.comment_body(OK, PUBLIC, 'delivery test', 'run')
        for word in ('PRIVATE KEY', 'BEGIN CERTIFICATE', 'token', 'password'):
            self.assertNotIn(word, body)


if __name__ == '__main__':
    unittest.main()
