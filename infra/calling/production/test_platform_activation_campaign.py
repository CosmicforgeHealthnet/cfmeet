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


if __name__ == "__main__":
    unittest.main()
