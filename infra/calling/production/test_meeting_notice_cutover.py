import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch

import notification_operations as no


class MeetingNoticeCutover(unittest.TestCase):
    def test_drain_covers_both_the_producer_and_the_receiving_inbox(self):
        producer, receiver = Mock(return_value='2'), Mock(return_value='3')
        cr = SimpleNamespace(meeting_psql=producer, release=SimpleNamespace(db_sql=receiver),
                             SERVICES={'notifications': {'db': 'notifications'}}, emit=Mock())
        self.assertEqual(no.meeting_in_app_backlog(cr), 5)
        self.assertIn("NOT shadow", producer.call_args.args[0])
        self.assertIn("state NOT IN ('ACKED','EXPIRED','SUPERSEDED')", producer.call_args.args[0])
        self.assertIn("state IN ('PENDING','HELD')", receiver.call_args.args[1])

    def test_email_is_not_activated_while_an_old_in_app_copy_can_still_be_processed(self):
        cr = SimpleNamespace(require_release=Mock(), begin=Mock())
        facts = dict(email_provider='resend', resend_done=True, meeting_in_app=False)
        with patch.object(no, 'routing_facts', return_value=facts), \
                patch.object(no, 'fence_row', return_value={'mode': 'SHADOW'}), \
                patch.object(no, 'meeting_in_app_backlog', return_value=1), \
                patch.object(no, 'routing_row') as row:
            with self.assertRaisesRegex(RuntimeError, 'drain'):
                no.activate_routing(cr, 'sha', '123', 'meeting-notification-email-active')
        row.assert_not_called()
        cr.begin.assert_not_called()

    def test_the_producer_fence_also_requires_the_old_copies_to_be_drained(self):
        facts = dict(protocol=1, publications_done=True, meeting_in_app=False,
                     service_modes={'EMAIL': 'ACTIVE', 'IN_APP': 'ACTIVE'}, meeting_in_app_backlog=1)
        args = ('meeting-service', no.MEETING_IN_APP_CATEGORY, 'FENCED_NEW', 'SHADOW')
        self.assertIn('drained', no.fence_gate(*args, facts))
        facts['meeting_in_app_backlog'] = 0
        self.assertIsNone(no.fence_gate(*args, facts))
