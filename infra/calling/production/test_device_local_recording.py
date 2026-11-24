import unittest
from types import SimpleNamespace
from unittest.mock import Mock
import meeting_recording_operations as mr


class DeviceOnlyCutover(unittest.TestCase):
    def fixture(self, count='0'):
        return SimpleNamespace(require_release=Mock(), release=SimpleNamespace(db_sql=Mock(return_value=count)),
            SERVICES={'calling': {'db': 'calls'}, 'meeting-service': {'db': 'meetings'}},
            env_of=Mock(return_value={'CALL_RECORDING_ENABLED': 'false', mr.FLAG: 'false'}),
            apply_activation=Mock(), emit=Mock())

    def test_cutover_fences_both_capture_owners_and_verifies_flags(self):
        cr = self.fixture()
        mr.activate_device_local(cr, 'sha', '123', 'device-only')
        cr.require_release.assert_called_once_with('sha')
        args = cr.apply_activation.call_args
        self.assertEqual(args.args[3], [('calling', {'CALL_RECORDING_ENABLED': 'false'}),
                                       ('meeting-service', {mr.FLAG: 'false'})])
        args.kwargs['post']()
        cr.env_of.return_value = {'CALL_RECORDING_ENABLED': 'true'}
        with self.assertRaisesRegex(RuntimeError, 'did not switch off'):
            args.kwargs['post']()

    def test_active_capture_or_wrong_confirmation_never_mutates(self):
        for confirm, count in [('device-only', '1'), ('enable', '0')]:
            cr = self.fixture(count)
            with self.assertRaises(RuntimeError):
                mr.activate_device_local(cr, 'sha', '123', confirm)
            cr.apply_activation.assert_not_called()
