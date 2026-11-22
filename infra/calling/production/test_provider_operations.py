import json
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch

import provider_operations as providers


class ProviderIntegration(unittest.TestCase):
    def test_no_integration_mutates_without_its_exact_confirmation(self):
        cr = Mock()
        for function in (providers.activate_phone, providers.activate_native_push):
            with self.assertRaisesRegex(RuntimeError, 'confirm'):
                function(cr, 'a' * 40, '123', '')
        cr.require_release.assert_not_called()
        cr.apply_activation.assert_not_called()

    def test_missing_credentials_prevent_activation(self):
        cr = Mock()
        with patch.object(providers, 'root_file', side_effect=FileNotFoundError):
            with self.assertRaises(FileNotFoundError):
                providers.activate_phone(cr, 'a' * 40, '123', 'twilio-verify-ready')
        cr.apply_activation.assert_not_called()

    def test_twilio_service_must_belong_to_account_before_mounting_anything(self):
        cr = Mock()
        cfg = {'accountSid': 'AC' + 'a' * 32, 'serviceSid': 'VA' + 'b' * 32}
        with patch.object(providers, 'root_file', side_effect=[('/config', json.dumps(cfg)), ('/token', 'secret' * 6)]), \
                patch.object(providers, 'provider_get', return_value={'sid': cfg['serviceSid'], 'account_sid': 'foreign'}):
            with self.assertRaisesRegex(RuntimeError, 'belong'):
                providers.activate_phone(cr, 'a' * 40, '123', 'twilio-verify-ready')
        cr.apply_activation.assert_not_called()

    def test_provider_endpoints_refuse_redirect_destinations_and_arbitrary_paths(self):
        for host, path in [('example.com', '/domains'), ('api.resend.com', '/emails'),
                           ('verify.twilio.com', '/v2/Services/VA123/Verifications')]:
            with self.assertRaisesRegex(RuntimeError, 'Unapproved'):
                providers.provider_get(host, path, 'secret')

    def test_native_metadata_cannot_override_provider_hosts_or_secret_mounts(self):
        cfg = {'apns': {'keyId': 'A' * 10, 'teamId': 'B' * 10, 'topic': 'com.cfmeet.app'},
               'fcm': {'projectId': 'cfmeet-production'}}
        env, files, platforms = providers.native_config(cfg)
        self.assertEqual(platforms, ['APNS', 'FCM'])
        self.assertEqual(env['NOTIFICATIONS_PUSH_APNS_KEY_FILE'], providers.APNS_TARGET)
        self.assertEqual(env['NOTIFICATIONS_PUSH_FCM_SERVICE_ACCOUNT_FILE'], providers.FCM_TARGET)
        cfg['apns']['baseUrl'] = 'https://example.com'
        with self.assertRaisesRegex(RuntimeError, 'APNs'):
            providers.native_config(cfg)

    def test_dns_diagnostics_emit_only_allowlisted_fields_and_never_credentials(self):
        cr = SimpleNamespace(emit=Mock())
        with patch.object(providers, 'root_file', return_value=('/key', 're_secret')), \
             patch.object(providers, 'provider_get', side_effect=[
                 {'data': [{'name': 'cfmeet.com', 'id': 'a' * 8 + '-' + 'b' * 4 + '-' + 'c' * 4 + '-' + 'd' * 4 + '-' + 'e' * 12}]},
                 {'status': 'verified', 'api_key': 'secret', 'records': [{'record': 'SPF', 'status': 'verified', 'value': 'secret'}]}]):
            result = providers.resend_domain_facts(cr)
        self.assertEqual(result['status'], 'verified')
        self.assertNotIn('secret', json.dumps(cr.emit.call_args.args))


if __name__ == '__main__':
    unittest.main()
