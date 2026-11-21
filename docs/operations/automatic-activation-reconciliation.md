# CF Meet production activation reconciliation

## Purpose

Code release and feature activation are separate. The automatic production release updates
changed backend service images on the VPS. The activation reconciliation workflow audits
the currently running services **after every successful automatic release** and can
apply only explicitly approved safe activations. It does not merge pull requests or
change the frontend.

Workflow: .github/workflows/cfmeet-activation-reconcile.yml
Runtime: infra/calling/production/activation_manager.py
Existing activation engine: infra/calling/production/coordinated_release.py

## What gets checked

The manager reads the **entire** existing VPS release journal (not the last ten
entries printed by the old inspect operation) and inspects the state and selected
boolean/non-secret configuration values of the running registered containers.
It never publishes full environments, credentials, private keys, or webhook secrets.

Every operation registered in coordinated_release.ACTIVATIONS is classified.
States include active, pending, recorded_unverified, untracked_enabled, rolled_back,
failed, incomplete, drift, partial_configuration, service_not_running, manual_review.

An activation counts as **active** for automation only if:
- its most recent journal record completed successfully; and
- all required runtime flags match the expected values; and
- the relevant service containers are running.

A historical successful record that was later rolled back is **not active**.
Neither a failed/rolled-back operation nor live-state drift is replayed automatically.

## Normal read-only workflow

Every successful CF Meet automatic production deploy run causes the activation
workflow to run in read-only mode by default. Its GitHub Actions summary lists
state counts and operations ready for explicit approval. The workflow uploads the
full machine-readable non-secret inventory as a JSONL artifact.

The two jobs share the deploy-backend-production concurrency lock so they never
change the VPS simultaneously. A superseded main commit is not activated.

## Approving a safe subset

For a one-time approved set, open the CF Meet production activation reconciliation
workflow in GitHub Actions. Run it on main, select mode=apply and supply the
comma-separated eligible operation names in approved_operations.

For continuing automatic activation after future successful backend releases,
set the GitHub Actions repository or environment variable CFMEET_AUTO_ACTIVATIONS
to an **explicit** comma-separated allowlisted set. The variable defaults to
unset; with no approvals, audits still run and **no feature state changes**.

Approving a list does not override any prerequisite or existing safety requirement.
The manager delegates each operation to coordinated_release.run, which enforces
its existing trust grants, release-SHA matching, health checks, journal updates,
resource checks and rollback. Dependencies must be verified active; an approved
operation already active is skipped. An operation found failed, rolled back or
drifted stops the apply rather than silently replaying it.

## Separate owner-gated operations

Never put irreversible Organizations cutovers, external telecom, public numbering,
SMS, PSTN, privacy relay, carrier activation, Resend/provider credentials,
private end-user encryption, parameterized notification routing, production
reviewer approvals, gateway edge reconfiguration or cryptographic key rotation into
automatic approvals. These still use the existing separately confirmed coordinated
production release operations and require their own checks.

Operations lacking sufficient live runtime evidence are reported as manual-review
or recorded-but-unverified; the manager does not assume their historical success
means a live feature is active.

This release does NOT automatically enable external telecom, SMS/phone OTP,
PSTN, number inventory/porting, or privacy relay.

## Verifying a live activation

Use the GitHub Actions summary and saved activation-audit.jsonl artifact.
Check live service health, public/private API routing, the complete journal
and owner-governed feature state. To retry a rollback, failure, drift, or
parameterized operation, use the existing coordinated production release workflow
after investigating why it is not currently active.
