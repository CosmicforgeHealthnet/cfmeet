"""Safety tests for the owner-approved, one-time production activation campaign."""
import unittest
from unittest.mock import patch

import platform_activation_campaign as campaign


class CampaignSafetyTests(unittest.TestCase):
    def test_exact_scope_and_confirmations(self):
        self.assertEqual(len(campaign.STEPS), 13)
        self.assertEqual(campaign.STEPS[0], ("certify", ""))
        self.assertEqual(campaign.STEPS[1], ("activate-partner-chat", ""))
        self.assertEqual(campaign.STEPS[-1], ("activate-in-meeting-apps", ""))
        self.assertEqual(dict(campaign.STEPS)["activate-gateway-calling-edge"],
                         "owner-approved-calling-edge")
        self.assertEqual(dict(campaign.STEPS)["activate-oauth-apps"], "no-reviewers-yet")
        self.assertFalse({"org-cutover-authoritative", "activate-notifications-email",
                          "activate-service-key-switch", "activate-sms",
                          "activate-privacy-relay"} & campaign.APPROVED)

    def test_last_entry_respects_rollback_order(self):
        journal = [{"operation": "activate-partner-chat", "status": "done"},
                   {"operation": "activate-partner-chat", "status": "rolled-back"}]
        self.assertEqual(campaign.last_entry(journal, "activate-partner-chat")["status"],
                         "rolled-back")

    def test_last_entry_is_not_any_old_success(self):
        journal = [{"operation": "activate-partner-chat", "status": "done"},
                   {"operation": "activate-partner-chat", "status": "failed"}]
        self.assertEqual(campaign.last_entry(journal, "activate-partner-chat")["status"],
                         "failed")

    def test_flags_are_taken_from_actual_service_catalogue(self):
        found = campaign.cr.platform_ops.flags_for("activate-partner-calls")
        self.assertEqual(found["calling"]["CALLING_USER_TOKEN_EXCHANGE_ENABLED"], "true")
        apps = campaign.cr.integrate_ops.flags_for("activate-app-installs")
        self.assertEqual(apps["organizations"]["ORGANIZATIONS_APP_INSTALLS_ENABLED"], "true")

    def test_in_meeting_apps_requires_live_flag(self):
        flags = campaign.cr.integrate_ops.flags_for("activate-in-meeting-apps")
        self.assertEqual(flags["meeting-service"]["MEETING_IN_MEETING_APPS_ENABLED"], "true")

    def test_rolled_back_is_blocked_even_with_matching_flags(self):
        with patch.object(campaign.cr, "load_journal", return_value=[
                 {"operation": "activate-partner-calls", "status": "done"},
                 {"operation": "activate-partner-calls", "status": "rolled-back"}]), \
             patch.object(campaign, "flag_attestation", return_value=(["on"], [], [])), \
             patch.object(campaign, "edge_evidence", return_value=None):
            self.assertEqual(campaign.phase_status("activate-partner-calls"), "blocked_rolled-back")

    def test_success_without_runtime_attestation_is_drift(self):
        with patch.object(campaign.cr, "load_journal", return_value=[
                 {"operation": "activate-partner-calls", "status": "done"}]), \
             patch.object(campaign, "flag_attestation", return_value=([], ["missing"], [])), \
             patch.object(campaign, "edge_evidence", return_value=None):
            self.assertEqual(campaign.phase_status("activate-partner-calls"), "drift")

    def test_no_history_with_live_flags_is_not_automatically_replayed(self):
        with patch.object(campaign.cr, "load_journal", return_value=[]), \
             patch.object(campaign, "flag_attestation", return_value=(["live"], [], [])), \
             patch.object(campaign, "edge_evidence", return_value=False):
            self.assertEqual(campaign.phase_status("activate-partner-calls"), "blocked_untracked_live_state")

    def test_new_phase_is_pending(self):
        with patch.object(campaign.cr, "load_journal", return_value=[]), \
             patch.object(campaign, "flag_attestation", return_value=([], ["off"], [])), \
             patch.object(campaign, "edge_evidence", return_value=None):
            self.assertEqual(campaign.phase_status("activate-partner-calls"), "pending")


MEETINGS_FAILED = {"campaign": campaign.CAMPAIGN, "status": "failed",
                   "baseSha": "ae44f8a5e3f305c86a6d04249af6aa7a8d3810ab",
                   "repairSha": "745f1525d3d1f9d9080f846214bd989e334eccff", "repairAttempts": 1,
                   "failedOperation": "activate-platform-meetings", "errorClass": "RuntimeError",
                   "completed": []}
NEW_SHA = "b" * 40


class RepairGateTests(unittest.TestCase):
    def authorize(self, record, sha=NEW_SHA, journal_status="failed", flags=([], ["off"], [])):
        record = dict(record)
        journal = [{"operation": record.get("failedOperation"), "status": journal_status}]
        with patch.object(campaign.cr, "load_journal", return_value=journal), \
             patch.object(campaign, "flag_attestation", return_value=flags), \
             patch.object(campaign.cr, "emit"):
            return campaign.authorize_repair(record, sha, "1-1"), record

    def test_rolled_back_meetings_failure_is_repaired_once_on_a_newer_release(self):
        operation, record = self.authorize(MEETINGS_FAILED)
        self.assertEqual(operation, "activate-platform-meetings")
        self.assertEqual((record["status"], record["repairAttempts"], record["repairSha"]),
                         ("running", 2, NEW_SHA))
        self.assertEqual(record["repairFromSha"], MEETINGS_FAILED["repairSha"])

    def test_the_original_partner_calls_repair_is_still_recognised(self):
        record = dict(MEETINGS_FAILED, failedOperation="activate-partner-calls", repairAttempts=0)
        record.pop("repairSha")
        self.assertEqual(self.authorize(record)[0], "activate-partner-calls")

    def test_a_second_meetings_failure_is_not_retried_again(self):
        _, repaired = self.authorize(MEETINGS_FAILED)
        repaired.update(status="failed")
        with self.assertRaisesRegex(RuntimeError, "not authorized"):
            self.authorize(repaired, sha="c" * 40)

    def test_the_same_release_is_never_retried(self):
        with self.assertRaisesRegex(RuntimeError, "not authorized"):
            self.authorize(MEETINGS_FAILED, sha=MEETINGS_FAILED["repairSha"])

    def test_unlisted_operations_and_error_classes_are_refused(self):
        for change in ({"failedOperation": "activate-oauth"}, {"errorClass": "KeyError"},
                       {"status": "running"}, {"repairAttempts": 0}):
            with self.assertRaisesRegex(RuntimeError, "not authorized"):
                self.authorize(dict(MEETINGS_FAILED, **change))

    def test_retry_requires_an_attested_rollback(self):
        for journal_status, flags in (("rolled-back", ([], ["off"], [])), ("failed", (["on"], [], [])),
                                      ("failed", ([], ["off"], ["meeting-service"])), ("failed", ([], [], []))):
            with self.assertRaisesRegex(RuntimeError, "rollback not independently verified"):
                self.authorize(MEETINGS_FAILED, journal_status=journal_status, flags=flags)


if __name__ == "__main__":
    unittest.main()
