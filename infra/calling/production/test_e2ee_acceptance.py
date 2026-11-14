"""Offline checks of the production multi-device E2EE acceptance operation."""
import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import e2ee_acceptance as e


class E2eeAcceptance(unittest.TestCase):
    def test_report_line_is_parsed_and_absence_fails(self):
        out = 'PASS x\nE2EE_ACCEPTANCE_REPORT {"verdict":"PASS","checks":{"a":"PASS"}}\n'
        self.assertEqual(e.parse_report(out)['verdict'], 'PASS')
        self.assertEqual(e.parse_report('panic')['verdict'], 'FAIL')

    def test_storage_query_counts_only_and_refuses_injection(self):
        sql = e.plaintext_sql('0b9d1c8e-1111-4a2b-9c3d-123456789abc', ["it's a phrase"])
        self.assertIn("it''s a phrase", sql)
        self.assertIn('count(*)', sql)
        self.assertNotIn('SELECT mls_ciphertext', sql)
        with self.assertRaises(AssertionError):
            e.plaintext_sql("x' OR 1=1 --", [])

    def test_release_status_requires_the_flag_and_the_mounted_record(self):
        self.assertIsNone(e.release_status({'MESSAGING_E2EE_DEVICES_ENABLED': 'false'}))
        self.assertIsNone(e.release_status({'MESSAGING_E2EE_DEVICES_ENABLED': 'true'}))
        with tempfile.TemporaryDirectory() as d:
            record = Path(d) / 'r.json'
            record.write_text(json.dumps({'decisionId': 'D', 'decidedAt': 'T', 'independentAssessment': 'NOT_PERFORMED',
                                          'acceptedResidualRisks': ['NO_INDEPENDENT_CRYPTOGRAPHIC_ASSESSMENT'], 'scope': 'x'}))
            with patch.object(e, 'RELEASE_SOURCE', record):
                status = e.release_status({'MESSAGING_E2EE_DEVICES_ENABLED': 'true',
                                           'MESSAGING_E2EE_PRODUCTION_RELEASE_FILE': e.RELEASE_TARGET})
        self.assertEqual(status['independentAssessment'], 'NOT_PERFORMED')
        self.assertNotIn('scope', status)

    def test_blocked_when_e2ee_is_not_activated(self):
        with patch.object(e.p, 'inspect', return_value={}), \
                patch.object(e.p, 'env_of', return_value={'CF_RUNTIME_ENVIRONMENT': 'production'}):
            with self.assertRaisesRegex(RuntimeError, 'not activated'):
                e.execute('a' * 40, '1')


if __name__ == '__main__':
    unittest.main()
