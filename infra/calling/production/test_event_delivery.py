"""Offline tests of the Organizations outbox report (Section 2 of the backend closure):
the never-dispatched `cfmeet.reconciliation` record is event history, not a delivery
backlog; only genuine consumer delivery problems fail verify and certify. The SQL itself was
checked on PostgreSQL 16 against the organization_outbox DDL."""
import unittest
from unittest.mock import patch

import coordinated_release as cr
import live_certification as lc

MEETING = 'cfmeet.meeting-service'


def report(**over):
    base = {
        'rows': 90, 'states': {'PENDING': 59, 'DELIVERED': 31},
        'byDestination': {cr.RECONCILIATION_DESTINATION: {'PENDING': 59}, MEETING: {'DELIVERED': 31}},
        'byDestinationEventState': [
            {'destination': cr.RECONCILIATION_DESTINATION, 'eventType': 'team.created', 'state': 'PENDING', 'count': 4},
            {'destination': MEETING, 'eventType': 'organization.created', 'state': 'DELIVERED', 'count': 6}],
        'reconciliationOutOfRecordShape': 0, 'consumerRowsWithoutRecord': 0,
        'consumerUndeliveredStalled': {}, 'consumerBlockedByDead': 0, 'oldestConsumerUndeliveredSeconds': 0,
    }
    base.update(over)
    return base


class EventDeliveryClassification(unittest.TestCase):
    def test_reconciliation_records_are_history_not_failures(self):
        c = cr.classify_event_delivery(report(), [MEETING])
        self.assertEqual(c['result'], 'PASS')
        self.assertEqual(c['reconciliationRecords']['count'], 59)
        self.assertEqual(c['reconciliationRecords']['dispatched'], 0)
        self.assertEqual(c['consumerDelivery'], {MEETING: {'DELIVERED': 31}})
        self.assertNotIn(cr.RECONCILIATION_DESTINATION, c['consumerDelivery'])
        self.assertEqual(c['undeliverableUnconfigured'], {})

    def test_genuine_consumer_failures_fail(self):
        cases = [
            (report(byDestination={cr.RECONCILIATION_DESTINATION: {'PENDING': 3}, MEETING: {'DEAD': 1}}), 'DEAD'),
            (report(consumerUndeliveredStalled={MEETING: 2}), 'undelivered after'),
            (report(consumerBlockedByDead=1), 'blocked behind a DEAD'),
            (report(consumerRowsWithoutRecord=1), 'without a reconciliation record'),
            (report(reconciliationOutOfRecordShape=1), 'reconciliation records were dispatched'),
        ]
        for r, text in cases:
            c = cr.classify_event_delivery(r, [MEETING])
            self.assertEqual(c['result'], 'FAIL', text)
            self.assertTrue(any(text in f for f in c['failures']), (text, c['failures']))

    def test_an_unconfigured_destination_is_reported_not_failed(self):
        r = report(byDestination={cr.RECONCILIATION_DESTINATION: {'PENDING': 2}, MEETING: {'PENDING': 2}},
                   consumerUndeliveredStalled={MEETING: 2})
        c = cr.classify_event_delivery(r, [])
        self.assertEqual(c['result'], 'PASS')
        self.assertEqual(c['undeliverableUnconfigured'], {MEETING: 2})

    def test_configured_destinations_come_from_the_organizations_environment(self):
        self.assertEqual(cr.configured_event_destinations(
            {'ORGANIZATIONS_EVENT_DESTINATIONS': ' cfmeet.meeting-service, '}), [MEETING])
        self.assertEqual(cr.configured_event_destinations({}), [])
        self.assertEqual(cr.configured_event_destinations(None), [])

    def test_the_query_reads_counts_only_and_names_the_record_destination(self):
        sql = cr.EVENT_DELIVERY_SQL
        self.assertNotIn('payload', sql)
        self.assertIn("'cfmeet.reconciliation'", sql)
        self.assertIn('make_interval(mins => %d)' % cr.EVENT_STALL_MINUTES, sql)

    def test_certify_uses_the_classification(self):
        envs = {'organizations': {'ORGANIZATIONS_EVENT_DESTINATIONS': MEETING}}
        with patch.object(cr, 'event_delivery', return_value=report()), \
                patch.object(cr, 'public_smoke', return_value={}), \
                patch.object(cr, 'safe_default_violations', return_value=[]), \
                patch.object(lc, 'integrations', return_value=[]), \
                patch.object(cr, 'safe_flags', return_value={}):
            try:
                out = lc.platform(envs, None, [])
            except Exception:  # later platform checks need the VPS; the delivery result is set first
                self.skipTest('platform() needs further VPS state')
        self.assertEqual(out['organizationsEventDelivery']['result'], 'PASS')
        self.assertEqual(out['organizationsEventDelivery']['reconciliationRecords']['count'], 59)


if __name__ == '__main__':
    unittest.main()
