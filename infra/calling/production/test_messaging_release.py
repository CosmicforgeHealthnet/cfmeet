import unittest
import messaging_release as release
import messaging_runtime as runtime


class MessagingInspection(unittest.TestCase):
    def test_summary_never_emits_secret_values(self):
        marker = 'PRIVATE-CREDENTIAL-MUST-NOT-LEAK'
        env = {key: marker for keys in release.DEPENDENCIES.values() for key in keys}
        env.update(DATABASE_URL=marker, CF_SERVICE_AUTH_PRIVATE_KEY_FILE=marker)
        value = release.configuration_summary(env)
        self.assertNotIn(marker, str(value))
        self.assertTrue(all(value['dependencies_configured'].values()))

    def test_incomplete_dependencies_are_not_reported_configured(self):
        value = release.configuration_summary({'MESSAGING_PUSH_APNS_KEY_FILE': 'private'})
        self.assertFalse(any(value['dependencies_configured'].values()))

    def test_no_activation_override_operation(self):
        with self.assertRaises(AssertionError):
            release.run('messaging-enable-e2ee', 'a' * 40, '123')

    def test_runtime_configuration_preserves_production_guard_and_rejects_override(self):
        env = {'CF_RUNTIME_ENVIRONMENT': 'production', 'MESSAGING_E2EE_DEVICES_ENABLED': 'false'}
        value = runtime.runtime_overrides(env, 'fixture-user', 'fixture password /@')
        self.assertEqual(value['MESSAGING_E2EE_DEVICES_ENABLED'], 'false')
        self.assertEqual(value['MESSAGING_ATTACHMENTS_S3_SERVER_SIDE_ENCRYPTION'], 'AES256')
        self.assertNotIn('CF_COMMUNICATIONS_PERSONAL_TENANT_ID', value)
        self.assertFalse(any('ALLOWED_PROFILE' in key for key in value))
        env['MESSAGING_E2EE_DEVICES_ENABLED'] = 'true'
        with self.assertRaises(AssertionError): runtime.runtime_overrides(env, 'fixture', 'fixture')

    def test_ingress_never_exposes_internal_admin_or_metrics_and_rejects_public_upstream(self):
        value = runtime.nginx_config('172.23.0.3', '172.23.0.5')
        self.assertEqual(value.count('location '), 3)
        self.assertNotIn('/internal', value)
        self.assertNotIn('/metrics', value)
        self.assertNotIn('/v1/messaging/policy', value)
        self.assertIn('proxy_ssl_verify on;', value)
        self.assertIn('access_log off;', value)
        with self.assertRaises(AssertionError): runtime.nginx_config('8.8.8.8', '172.23.0.5')
