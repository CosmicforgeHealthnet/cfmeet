"""Offline activation reconciliation tests: no VPS connections or data mutations."""
import unittest

import activation_manager as manager


class ActivationManagerTests(unittest.TestCase):
    def test_rollbacks_override_prior_success(self):
        journal = [{"operation": "activate-provisioning", "status": "done"},
                   {"operation": "activate-provisioning", "status": "rolled-back"}]
        envs = {"organizations": {"ORGANIZATIONS_PROVISIONING_ENABLED": "true"}}
        self.assertEqual(manager.operation_state("activate-provisioning", journal, envs,
                                                 {"organizations"}), "rolled_back")

    def test_completed_journal_without_matching_live_flags_is_drift(self):
        journal = [{"operation": "activate-provisioning", "status": "done"}]
        envs = {"organizations": {"ORGANIZATIONS_PROVISIONING_ENABLED": "false"}}
        self.assertEqual(manager.operation_state("activate-provisioning", journal, envs,
                                                 {"organizations"}), "drift")

    def test_untracked_live_flags_not_reenabled(self):
        envs = {"organizations": {"ORGANIZATIONS_PROVISIONING_ENABLED": "true"}}
        self.assertEqual(manager.operation_state("activate-provisioning", [], envs,
                                                 {"organizations"}), "untracked_enabled")

    def test_missing_service_is_not_pending_activation(self):
        self.assertEqual(manager.operation_state("activate-provisioning", [], {}, set()),
                         "service_not_running")

    def test_dependency_requires_verified_activation(self):
        ops = manager.assess([], {"organizations": {"ORGANIZATIONS_PROVISIONING_ENABLED": "false"}},
                             {"organizations"})
        by_op = {o["operation"]: o for o in ops}
        self.assertTrue(by_op["activate-provisioning"]["readyForApproval"])
        self.assertFalse(by_op["activate-organization-bindings"]["readyForApproval"])
        self.assertIn("activate-provisioning",
                      by_op["activate-organization-bindings"]["missingPrerequisites"])

    def test_empty_approval_and_reject_duplicate_or_sensitive_operations(self):
        self.assertEqual(manager.approved_list(""), [])
        self.assertEqual(manager.approved_list("activate-provisioning,activate-org-events"),
                         ["activate-provisioning", "activate-org-events"])
        with self.assertRaisesRegex(RuntimeError, "Duplicate"):
            manager.approved_list("activate-provisioning,activate-provisioning")
        for op in ("org-cutover-authoritative", "activate-personal-encryption",
                   "activate-notifications-routing", "activate-resend-email",
                   "activate-service-key-switch", "activate-gateway-calling-edge"):
            with self.subTest(operation=op), self.assertRaisesRegex(RuntimeError, "Not eligible"):
                manager.approved_list(op)

    def test_active_flags_and_journal_skip_redeployment(self):
        journal = [{"operation": "activate-provisioning", "status": "done"}]
        envs = {"organizations": {"ORGANIZATIONS_PROVISIONING_ENABLED": "true"}}
        self.assertEqual(manager.operation_state("activate-provisioning", journal, envs,
                                                 {"organizations"}), "active")


if __name__ == "__main__":
    unittest.main()
