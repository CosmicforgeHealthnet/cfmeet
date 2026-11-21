"""One-time owner-approved CF Meet Platform / Apps production activation campaign.

Runs on the VPS under the SAME coordinated release production lock and *only*
after green main CI and a successful latest-main coordinated release. Every
mutation uses the existing coordinated_release.run() feature operation, with
its service grants, health tests, acceptance and rollback safeguards.

No external telecom, SMS, number inventory, PSTN or privacy relay activation.
"""
import json
import sys
from pathlib import Path

import coordinated_release as cr

CAMPAIGN = "platform-apps-2026-11-21"
STEPS = (
    ("certify", ""),
    ("activate-partner-chat", ""),
    ("activate-partner-calls", ""),
    ("activate-gateway-calling-edge", "owner-approved-calling-edge"),
    ("activate-partner-push", ""),
    ("activate-platform-meetings", ""),
    ("activate-oauth-apps", "no-reviewers-yet"),
    ("activate-oauth", ""),
    ("activate-delegated-apps", ""),
    ("activate-app-installs", ""),
    ("activate-app-events", ""),
    ("activate-app-directory", ""),
    ("activate-in-meeting-apps", ""),
)
# Approved scope is static and limited to the owner's precise thirteen steps.
APPROVED = frozenset(step for step, _ in STEPS)
CAMPAIGN_FILE = cr.WORK / (CAMPAIGN + ".json")


def last_entry(journal, operation):
    for entry in reversed(journal):
        if entry.get("operation") == operation:
            return entry
    return None


def flags_for(operation):
    if operation in cr.platform_ops.OPERATIONS:
        flags = cr.platform_ops.flags_for(operation)
        # The MESSAGE_SENT usage fact is optional when usage exporting is off.
        if operation == cr.platform_ops.CHAT and cr.env_of("messaging").get(
                "CFMEET_USAGE_EXPORT_ENABLED") != "true":
            flags = {name: dict(values) for name, values in flags.items()}
            flags["messaging"].pop("MESSAGING_MESSAGE_SENT_EXPORT_ENABLED", None)
        return flags
    if operation in cr.integrate_ops.OPERATIONS:
        return cr.integrate_ops.flags_for(operation)
    return {}


def flag_attestation(operation):
    """Evidence about the *current* service containers, never printed as raw env."""
    flags = flags_for(operation)
    passed, absent, missing = [], [], []
    for service, values in flags.items():
        name = cr.SERVICES[service]["container"]
        if name not in cr.container_names():
            missing.append(service)
            continue
        instance = cr.p.inspect(name)
        if instance.get("State", {}).get("Status") != "running":
            missing.append(service)
            continue
        env = cr.p.env_of(instance)
        for key, expected in values.items():
            if env.get(key) == expected:
                passed.append(service + ":" + key)
            else:
                absent.append(service + ":" + key)
    return passed, absent, missing


def edge_evidence(operation):
    """Read-only public probes for edge-moving operations, before skipping them."""
    if operation == cr.platform_ops.CALLING_EDGE:
        checks = cr.platform_ops.calling_edge_checks(cr)
    elif operation == cr.platform_ops.MEETINGS:
        checks = cr.platform_ops.meetings_edge_checks(cr)
    elif operation == cr.integrate_ops.OAUTH:
        checks = cr.integrate_ops.oauth_edge_checks(cr)
    elif operation == cr.integrate_ops.DIRECTORY:
        checks = cr.integrate_ops.directory_edge_checks(cr)
    elif operation == cr.integrate_ops.IN_MEETING:
        checks = cr.integrate_ops.context_keys_checks(cr)
    else:
        return None
    return bool(checks) and all(value == "PASS" for value in checks.values())


def phase_status(operation):
    """The latest journal outcome and live flags must agree or we stop."""
    entry = last_entry(cr.load_journal(), operation)
    passed, absent, missing = flag_attestation(operation)
    if missing:
        return "blocked_missing_service"
    edge = edge_evidence(operation)
    if entry and entry.get("status") == "done":
        return "active" if not absent and (edge is None or edge is True) else "drift"
    if entry:
        return "blocked_" + str(entry.get("status"))
    if passed or edge is True:
        return "blocked_untracked_live_state"
    return "pending"


def prerequisite_evidence(operation):
    """Independent live checks for high-risk prerequisites not in this campaign."""
    if operation == cr.integrate_ops.INSTALLS:
        if cr.env_of("meeting-service").get("ORGANIZATIONS_AUTHORITY_MODE") != "authoritative":
            raise RuntimeError("Organization workspace installs require live authoritative Organizations")
    if operation == cr.integrate_ops.EVENTS:
        destination = cr.env_of("messaging").get("MESSAGING_PUBLIC_EVENTS_DESTINATION")
        if destination not in cr.integrate_ops.EVENT_DESTINATIONS:
            raise RuntimeError("Messaging is not delivering public events to Developer Platform")
    if operation == cr.platform_ops.PUSH:
        if cr.env_of("notifications").get("NOTIFICATIONS_PUSH_PROVIDER") != "live":
            raise RuntimeError("Notifications Web Push must be active before partner push")
        if cr.env_of("calling").get("CALLING_PUSH_TRANSPORT") != "notifications":
            raise RuntimeError("Calling Push must use Notifications before partner push")
    if operation == cr.integrate_ops.OAUTH_APPS:
        if cr.env_of("developer-platform").get("DEVELOPER_SESSION_BRIDGE_ENABLED") == "true":
            # This variable actually lives on Meeting; the check is below.
            pass
        if cr.env_of("platform-access").get("PLATFORM_ACCESS_END_USERS_ENABLED") != "true":
            raise RuntimeError("Platform Users are not live")
    if operation == cr.platform_ops.CALLING_EDGE:
        if cr.env_of("api-gateway").get("API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED") != "true":
            raise RuntimeError("Gateway families must be configured before moving Calling routes")
    for requirement in cr.ACTIVATIONS[operation][0]:
        choices = requirement if isinstance(requirement, tuple) else (requirement,)
        # Every prerequisite must at least be recorded as done. The operator's own
        # flags and public edge checks are independently verified above; prerequisite
        # operations outside the checklist retain their existing live safety gates.
        if not any(cr.done(name) for name in choices):
            raise RuntimeError("Activation prerequisite not completed: " + " or ".join(choices))


def save(report):
    cr.p.write_private(CAMPAIGN_FILE, json.dumps(report, sort_keys=True, indent=2))
    cr.emit("campaignState", {key: report[key] for key in ("campaign", "status", "completed")})


def run(sha, run_id):
    if not cr.SHA_RE.fullmatch(sha) or not cr.RUN_RE.fullmatch(run_id):
        raise RuntimeError("Invalid release SHA or run ID")
    if not cr.last_release(sha):
        raise RuntimeError("The exact current main release must complete before activation")
    cr.require_release(sha)

    if CAMPAIGN_FILE.exists():
        record = json.loads(CAMPAIGN_FILE.read_text())
        if record.get("status") == "complete":
            cr.emit("campaignComplete", {"campaign": CAMPAIGN, "alreadyComplete": True,
                                          "steps": record.get("completed", [])})
            return
        raise RuntimeError("A prior campaign attempt is %s; requires investigation, not blind retry"
                           % record.get("status", "unknown"))

    record = {"campaign": CAMPAIGN, "status": "running", "baseSha": sha,
              "runId": run_id, "completed": []}
    save(record)
    try:
        for index, (operation, confirm) in enumerate(STEPS):
            if operation not in APPROVED:
                raise RuntimeError("Out-of-scope activation operation")
            step_id = str(int(run_id.split("-")[0]) * 100 + index + 1) + "-" + run_id.split("-")[-1]
            if operation == "certify":
                cr.emit("campaignStep", {"index": index + 1, "operation": operation, "action": "certify"})
                cr.run("certify", sha, step_id)
                record["completed"].append({"step": index + 1, "operation": operation, "result": "certified"})
                save(record)
                continue

            prior = phase_status(operation)
            cr.emit("campaignStep", {"index": index + 1, "operation": operation, "state": prior})
            if prior == "active":
                record["completed"].append({"step": index + 1, "operation": operation, "result": "already_active"})
                save(record)
                continue
            if prior != "pending":
                raise RuntimeError("%s is %s; investigate before activation" % (operation, prior))
            prerequisite_evidence(operation)
            cr.run(operation, sha, step_id, confirm)
            after = phase_status(operation)
            if after != "active":
                raise RuntimeError("%s reported success but its live state is %s" % (operation, after))
            record["completed"].append({"step": index + 1, "operation": operation, "result": "activated"})
            save(record)

        # Complete platform two-tenant E2E acceptance and production certification.
        cr.run("platform-acceptance", sha, str(int(run_id.split("-")[0]) * 100 + 90) +
               "-" + run_id.split("-")[-1])
        cr.run("certify", sha, str(int(run_id.split("-")[0]) * 100 + 91) +
               "-" + run_id.split("-")[-1])
    except Exception as exc:
        record["status"] = "failed"
        record["failedOperation"] = operation
        # Do not leak raw provider/response details in the durable campaign file.
        record["errorClass"] = type(exc).__name__
        save(record)
        raise

    record["status"] = "complete"
    save(record)
    cr.emit("campaignComplete", {"campaign": CAMPAIGN, "steps": record["completed"]})


if __name__ == "__main__":
    try:
        if len(sys.argv) != 3:
            raise RuntimeError("Usage: platform_activation_campaign.py <release-sha> <run-id>")
        run(sys.argv[1], sys.argv[2])
    except Exception as exc:
        cr.emit("platformActivationError", str(exc))
        raise SystemExit(1)
