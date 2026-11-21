# Owner-authorized Platform and Apps production activation campaign — 21 November 2026

## Source and authorization

A 13-step production checklist was supplied for CF Meet: read-only certification;
activate-partner-chat, activate-partner-calls, activate-gateway-calling-edge,
activate-partner-push, activate-platform-meetings, activate-oauth-apps,
activate-oauth, activate-delegated-apps, activate-app-installs,
activate-app-events, activate-app-directory and activate-in-meeting-apps.

The owner explicitly authorized completion of the whole checklist and the
investigation required to do so. Confirmation values from that checklist:
- activate-gateway-calling-edge: owner-approved-calling-edge
- activate-oauth-apps: no-reviewers-yet, valid only if zero named app reviewers
- all other operations: no confirmation

The SHA shown in the older checklist is stale. This campaign is tied to
the SHA that successfully completes the latest automatic main release.

## Runner

- Workflow: .github/workflows/cfmeet-platform-apps-activation-campaign.yml
- VPS code: infra/calling/production/platform_activation_campaign.py
- Safety regression tests: test_platform_activation_campaign.py
- Existing owning engine: coordinated_release.py and its Platform/Apps operations

The new workflow triggers **only** when the automatic backend production release
completes successfully on main. It checks that the release is still the current
main head, takes the same deploy-backend-production lock as coordinated release,
and checks the exact release images running on the VPS before any activation.

The operations are sequential, not a parallel matrix. They retain the
existing flags, signing-key grants, public routing, webhook, readiness, per-stage
two-tenant acceptance and rollback checks. The script verifies the current
runtime flag for each operation and the public endpoints for edge-moving
operations, not only historical success in the release journal. If a prior
activation is both journaled and live, it is skipped. If it has failed,
been rolled back, drifted, or was changed without a journal, the campaign
**stops** and requires investigation.

The campaign state is durable on the VPS:
  /opt/cfmeet-core/coordinated/platform-apps-2026-11-21.json
Once a campaign is marked complete, it cannot mutate the VPS again.
A failed or interrupted campaign is never automatically replayed; inspect
the journal and test reports before a separately approved retry.
A failed step retains the existing per-step rollback behavior. Previously
completed successful activations are not torn down as a blanket rollback.

After step 13, the runner executes full platform acceptance followed by
fresh read-only live certification. The workflow reports the completed
operations and writes the JSONL log as a GitHub Actions artifact.

External telecom, SMS/phone OTP, phone number provisioning, carrier calls,
PSTN and Privacy Relay remain disabled and are never activated here.

## Meaning of success

The workflow must complete green AND emit campaignComplete before calling
the 13-step flow complete. Green backend CI, a healthy container, a prior
journal record, or a GitHub workflow registration alone does not mean a
feature was successfully activated.
