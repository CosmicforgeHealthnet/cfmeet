import unittest
import messaging_release as release


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
