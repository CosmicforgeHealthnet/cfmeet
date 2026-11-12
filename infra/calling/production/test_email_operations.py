"""Offline tests: Resend activation, email acceptance and diagnose (no VPS, no network)."""

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import coordinated_release as cr
import email_operations as mail
import production as p
from test_coordinated_release import isolated_work


class MailConfiguration(unittest.TestCase):
    def test_mail_summary_reports_presence_never_secrets(self):
        env = {'SMTP_HOST': 'smtp.gmail.com', 'SMTP_USERNAME': 'ops@gmail.com', 'SMTP_PASSWORD': 'hunter2-app',
               'SMTP_FROM_EMAIL': 'ops@gmail.com', 'RESEND_API_KEY_FILE': mail.KEY_TARGET, 'EMAIL_PROVIDER': 'resend'}
        out = json.dumps(mail.mail_summary(env))
        self.assertNotIn('hunter2', out)
        self.assertNotIn('ops@gmail.com', out)
        summary = mail.mail_summary(env)
        self.assertEqual(summary['smtpUsernameDomain'], 'gmail.com')
        self.assertTrue(summary['resendKeyMounted'])

    def test_notification_email_needs_a_working_transport(self):
        smtp = {'SMTP_HOST': 'h', 'SMTP_USERNAME': 'u', 'SMTP_PASSWORD': 'x'}
        resend = {'EMAIL_PROVIDER': 'resend', 'RESEND_API_KEY_FILE': mail.KEY_TARGET}
        self.assertTrue(cr.mail_available(smtp))
        self.assertTrue(cr.mail_available(resend))
        self.assertFalse(cr.mail_available({'SMTP_HOST': 'h'}))
        with isolated_work():
            self.assertEqual(cr.safe_default_violations({'meeting-service': dict(resend, NOTIFICATIONS_EMAIL_ENABLED='true')}), [])
            self.assertEqual(cr.safe_default_violations({'meeting-service': {'NOTIFICATIONS_EMAIL_ENABLED': 'true'}}),
                             ['meeting-service NOTIFICATIONS_EMAIL_ENABLED without mail credentials'])
        self.assertIn('NOTIFICATIONS_EMAIL_ENABLED', cr.first_party_feature_changes(resend)[1][1])


class LogEvidence(unittest.TestCase):
    def test_failure_signatures_are_counted_without_lines(self):
        text = ('2026-11-12T00:01:02.1Z WARN Email job 1 failed: SMTP error: permanent error (535): 5.7.8 '
                'Username and Password not accepted. BadCredentials ada@example.com\n'
                '2026-11-12T00:05:00.1Z WARN Email job 1 exceeded max retries, dropping\n'
                '2026-11-12T00:09:00.0Z INFO email accepted by provider\n')
        r = mail.scan(text, 24)
        self.assertEqual(r['smtp535']['count'], 1)
        self.assertEqual(r['badCredentials']['first'], '2026-11-12T00:01:02')
        self.assertEqual(r['droppedAfterRetries']['count'], 1)
        self.assertEqual(r['providerAccepted']['count'], 1)
        self.assertNotIn('ada@example.com', json.dumps(r))


class ResendActivation(unittest.TestCase):
    def test_refuses_without_a_key_and_mounts_it_read_only(self):
        with tempfile.TemporaryDirectory() as folder, patch.object(p, 'SECRETS', Path(folder)):
            with self.assertRaises(RuntimeError):
                mail.resend_changes(cr)
            (Path(folder) / mail.SECRET_KEY_FILE).write_text('re_test_123456789\n')
            overrides, mounts, webhook = mail.resend_changes(cr)
            self.assertEqual(overrides['EMAIL_PROVIDER'], 'resend')
            self.assertEqual(overrides['EMAIL_FROM_ADDRESS'], 'no-reply@cfmeet.com')
            self.assertEqual(mounts, [(Path(folder) / mail.SECRET_KEY_FILE, mail.KEY_TARGET)])
            self.assertFalse(webhook)
            self.assertNotIn('re_test', json.dumps(overrides))
            self.assertEqual((Path(folder) / mail.SECRET_KEY_FILE).stat().st_mode & 0o777, 0o400)
            (Path(folder) / mail.SECRET_WEBHOOK_FILE).write_text('whsec_fixture-not-a-secret-000000000000\n')
            overrides, mounts, webhook = mail.resend_changes(cr)
            self.assertTrue(webhook)
            self.assertEqual(overrides['RESEND_WEBHOOK_SECRET_FILE'], mail.WEBHOOK_TARGET)

    def test_activation_is_registered_and_failure_restores(self):
        self.assertIn('activate-resend-email', cr.ACTIVATIONS)
        self.assertIn('diagnose', cr.OPERATIONS)
        self.assertIn('email-acceptance', cr.OPERATIONS)
        calls = []
        with patch.object(mail, 'require_ledger', lambda c: None), \
                patch.object(mail, 'resend_changes', lambda c: ({'EMAIL_PROVIDER': 'resend'}, [], False)), \
                patch.object(cr, 'apply_activation', lambda *a, **k: calls.append((a, k))), \
                contextlib.redirect_stdout(io.StringIO()):
            mail.activate_resend_email(cr, 'a' * 40, '1-1', '')
        (args, kwargs), = calls
        self.assertEqual(args[0], 'activate-resend-email')
        self.assertEqual(args[3][0][0], 'meeting-service')
        self.assertTrue(callable(kwargs['post']))

    def test_acceptance_requires_resend_acceptance(self):
        env = {'INTERNAL_API_TOKEN': 't'}
        answers = iter([(202, {'deliveryId': 'd'}), (200, {'status': 'FAILED', 'provider': 'resend',
                                                          'lastErrorClass': 'PROVIDER_CONFIGURATION'})])
        with isolated_work(), patch.object(p, 'inspect', lambda n: {}), patch.object(p, 'env_of', lambda c: env), \
                patch.object(mail, 'internal', lambda *a, **k: next(answers)), \
                contextlib.redirect_stdout(io.StringIO()) as out:
            with self.assertRaises(RuntimeError):
                mail.email_acceptance_check(cr, '1-1', False)
        self.assertIn('PROVIDER_CONFIGURATION', out.getvalue())

    def test_acceptance_passes_on_resend_acceptance(self):
        env = {'INTERNAL_API_TOKEN': 't'}
        answers = iter([(202, {'deliveryId': 'd'}), (200, {'status': 'ACCEPTED', 'provider': 'resend'})])
        with isolated_work() as work, patch.object(p, 'inspect', lambda n: {}), \
                patch.object(p, 'env_of', lambda c: env), \
                patch.object(mail, 'internal', lambda *a, **k: next(answers)), \
                contextlib.redirect_stdout(io.StringIO()):
            (work / 'email-acceptance-recipient').write_text('ops@example.org\n')
            r = mail.email_acceptance_check(cr, '1-1', False)
        self.assertEqual(r['recipientDomain'], 'example.org')

    def test_recipient_labels(self):
        self.assertEqual(mail.labelled('delivered@resend.dev', 'x1'), 'delivered+x1@resend.dev')
        self.assertEqual(mail.labelled('ops+old@example.org', 'x1'), 'ops+x1@example.org')
        with isolated_work() as work:
            (work / 'email-acceptance-recipient').write_text('not an address')
            with self.assertRaises(RuntimeError):
                mail.acceptance_recipient(cr)


if __name__ == '__main__':
    unittest.main()
