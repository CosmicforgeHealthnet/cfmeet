"""Image scope includes first-time CF Meet containers without duplicate image builds."""
import unittest
import coordinated_release as cr
import release_scope as rs


class FirstTimeScopeTests(unittest.TestCase):
    def test_unchanged_deployment(self):
        present = {cr.SERVICES[n]['container'] for n in cr.ORDER}
        result = rs.include_missing_services(dict(changed=[], unchanged=list(cr.IMAGES)), present)
        self.assertEqual(result['changed'], [])
        self.assertEqual(result['unchanged'], list(cr.IMAGES))
        self.assertEqual(result['missingServices'], [])

    def test_missing_service_always_builds_image(self):
        present = {cr.SERVICES[n]['container'] for n in cr.ORDER}
        present.remove(cr.SERVICES['organizations']['container'])
        result = rs.include_missing_services(dict(changed=[], unchanged=list(cr.IMAGES)), present)
        self.assertEqual(result['changed'], ['organizations'])
        self.assertIn('organizations', result['missingServices'])
        self.assertNotIn('organizations', result['unchanged'])

    def test_missing_replica_reuses_shared_image_once(self):
        present = {cr.SERVICES[n]['container'] for n in cr.ORDER}
        present.remove(cr.SERVICES['notifications-2']['container'])
        result = rs.include_missing_services(dict(changed=['messaging'], unchanged=[]), present)
        self.assertIn('notifications', result['changed'])
        self.assertEqual(result['changed'].count('notifications'), 1)
        self.assertIn('messaging', result['changed'])


if __name__ == '__main__':
    unittest.main()
