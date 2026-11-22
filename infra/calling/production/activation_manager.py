"""State-aware CF Meet production activation reconciliation.

Reads full VPS journal and running container flags. Never auto-applies unapproved,
unknown, irreversible, externally gated or parameterized feature activations.
All mutations still use coordinated_release.run under the shared production lock.
"""
import collections
import json
import sys

import coordinated_release as cr

# Strict, runtime-attested allowlist: separate, explicit approval still required.
PROBES = {
    "activate-provisioning": (("organizations", "ORGANIZATIONS_PROVISIONING_ENABLED", "true"),),
    "activate-organization-bindings": (("organizations", "ORGANIZATIONS_SERVICE_BINDINGS_ENABLED", "true"),),
    "activate-org-events": (
        ("meeting-service", "ORGANIZATION_EVENTS_ENABLED", "true"),
        ("organizations", "ORGANIZATIONS_EVENT_DESTINATIONS", "cfmeet.meeting-service"),
    ),
    "activate-enterprise-calling": (
        ("meeting-service", "CALLING_ORGANIZATION_FORWARDING_ENABLED", "true"),
        ("meeting-service", "ORGANIZATION_CALLING_SCOPE", "organization"),
    ),
    "activate-developer-platform": (
        ("developer-platform", "DEVELOPER_PLATFORM_GATEWAY_API_ENABLED", "true"),
        ("api-gateway", "API_GATEWAY_PUBLIC_ROUTING_ENABLED", "true"),
    ),
    "activate-developer-console": (("meeting-service", "DEVELOPER_SESSION_BRIDGE_ENABLED", "true"),),
    "activate-web-origins": (
        ("api-gateway", "API_GATEWAY_CORS_ALLOWED_ORIGINS", ",".join(cr.WEB_ORIGINS)),
    ),
    "activate-token-ceiling": (("api-gateway", "API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE", "600"),),
    "activate-client-address-limits": (
        ("meeting-service", "AUTH_CLIENT_IP_HEADER", "x-real-ip"),
        ("meeting-service", "AUTH_TRUSTED_PROXY_HOPS", "1"),
    ),
    "activate-database-pools": (
        ("messaging", "MESSAGING_DATABASE_MAX_CONNECTIONS", "25"),
        ("calling", "CALLING_DATABASE_MAX_CONNECTIONS", "20"),
        ("meeting-service", "DATABASE_MAX_CONNECTIONS", "20"),
        ("notifications", "NOTIFICATIONS_DATABASE_MAX_CONNECTIONS", "16"),
        ("notifications-2", "NOTIFICATIONS_DATABASE_MAX_CONNECTIONS", "16"),
    ),
    "activate-usage-export": (
        ("calling", "CFMEET_USAGE_EXPORT_ENABLED", "true"),
        ("messaging", "CFMEET_USAGE_EXPORT_ENABLED", "true"),
        ("meeting-service", "CFMEET_USAGE_EXPORT_ENABLED", "true"),
    ),
    "activate-notifications-push": (
        ("notifications", "NOTIFICATIONS_PUSH_PROVIDER", "live"),
        ("notifications-2", "NOTIFICATIONS_PUSH_PROVIDER", "live"),
    ),
    "activate-messaging-push-mirror": (("messaging", "MESSAGING_PUSH_REGISTRATION_MIRROR", "on"),),
    "activate-calling-push": (("calling", "CALLING_PUSH_TRANSPORT", "notifications"),),
}
IRREVERSIBLE = frozenset({
    "org-cutover-import", "org-cutover-authoritative", "org-cutover-first-activation",
})
NEVER_AUTOMATE = frozenset({
    "activate-resend-email", "activate-notifications-email",
    "activate-personal-encryption", "activate-web-push",
    "activate-notifications-routing", "activate-notification-fence",
    "activate-meeting-in-app-notices", "activate-preference-authority",
    "activate-service-key-publish", "activate-service-key-switch", "activate-service-key-retire",
    "activate-gateway-routing", "activate-gateway-families", "activate-gateway-docs",
    "activate-edge-rate-limits", "activate-public-internal-denial",
    "activate-gateway-calling-edge", "activate-production-access-review",
    "activate-oauth-apps", "activate-oauth", "activate-app-installs",
})
AUTO_ELIGIBLE = frozenset(PROBES) - NEVER_AUTOMATE - IRREVERSIBLE


def latest_status(journal, operation):
    """Last recorded outcome wins; rolled-back success is not active."""
    for entry in reversed(journal):
        if entry.get("operation") == operation:
            return entry.get("status", "unknown")
    return None


def missing_requirements(operation, states):
    missing = []
    for item in cr.ACTIVATIONS[operation][0]:
        if isinstance(item, tuple):
            if not any(states.get(alt) == "active" for alt in item):
                missing.append(" OR ".join(item))
        elif states.get(item) != "active":
            missing.append(item)
    return missing


def operation_state(operation, journal, envs, running):
    latest = latest_status(journal, operation)
    checks = PROBES.get(operation)
    if not checks:
        if latest == "done":
            return "recorded_unverified"
        if latest == "rolled-back":
            return "rolled_back"
        if latest == "failed":
            return "failed"
        if latest == "running":
            return "incomplete"
        return "manual_review"
    needed = {service for service, _, _ in checks}
    if not needed.issubset(running):
        return "service_not_running"
    passed = sum(envs.get(service, {}).get(key) == expected for service, key, expected in checks)
    if latest == "done":
        return "active" if passed == len(checks) else "drift"
    if latest == "rolled-back":
        return "rolled_back"
    if latest == "failed":
        return "failed"
    if latest == "running":
        return "incomplete"
    if passed == len(checks):
        return "untracked_enabled"
    if passed:
        return "partial_configuration"
    return "pending"


def classify(operation):
    if operation.startswith("org-cutover-"):
        return "organization_cutover"
    if any(v in operation for v in ("notification", "push", "preference")):
        return "notifications"
    if any(v in operation for v in ("gateway", "oauth", "app")):
        return "gateway_and_apps"
    if any(v in operation for v in ("developer", "usage")):
        return "developer_platform"
    if any(v in operation for v in ("organization", "enterprise", "provision")):
        return "organizations"
    if "calling" in operation or "partner-calls" in operation:
        return "calling"
    if "messaging" in operation or "chat" in operation:
        return "messaging"
    return "platform_and_security"


def assess(journal, envs, running):
    states = {name: operation_state(name, journal, envs, running) for name in cr.ACTIVATIONS}
    result = []
    for name in cr.ACTIVATIONS:
        status = states[name]
        unmet = missing_requirements(name, states)
        result.append({
            "operation": name,
            "group": classify(name),
            "state": status,
            "eligibility": "approval_required" if name in AUTO_ELIGIBLE else
                           ("owner_gated" if name in IRREVERSIBLE or name in NEVER_AUTOMATE else "manual_review"),
            "readyForApproval": name in AUTO_ELIGIBLE and status == "pending" and not unmet,
            "missingPrerequisites": unmet,
        })
    return result


def live_snapshot():
    """Read only. Never emit raw environment values, secret names or credentials."""
    running, envs, containers = set(), {}, {}
    for name, service in cr.SERVICES.items():
        try:
            c = cr.p.inspect(service["container"])
            state = c.get("State", {}).get("Status")
            containers[name] = state
            if state == "running":
                running.add(name)
                envs[name] = cr.p.env_of(c)
        except (RuntimeError, KeyError):
            containers[name] = "missing"
    return cr.load_journal(), envs, running, containers


def audit(sha):
    journal, envs, running, containers = live_snapshot()
    operations = assess(journal, envs, running)
    release = cr.last_release()
    report = {
        "requestedMainSha": sha,
        "lastCompletedReleaseSha": release["sha"] if release else None,
        "releaseMatchesMain": bool(release and release["sha"] == sha),
        "serviceStates": containers,
        "operationCounts": dict(sorted(collections.Counter(o["state"] for o in operations).items())),
        "operations": operations,
        "readyForApproval": [o["operation"] for o in operations if o["readyForApproval"]],
        "excludedFromAutomaticActivation": sorted(NEVER_AUTOMATE | IRREVERSIBLE),
    }
    cr.emit("activationAudit", report)
    return report


def approved_list(raw):
    selected = [s.strip() for s in raw.split(",") if s.strip()]
    if len(selected) != len(set(selected)):
        raise RuntimeError("Duplicate activation approvals are not permitted")
    forbidden = sorted(set(selected) - AUTO_ELIGIBLE)
    if forbidden:
        raise RuntimeError("Not eligible for this activation manager: " + ", ".join(forbidden))
    return selected


def apply(sha, run_id, raw):
    selected = approved_list(raw)
    if not selected:
        return audit(sha)
    if not cr.last_release(sha):
        raise RuntimeError("Refusing activations: no completed production release of main SHA")
    pending, outcomes = set(selected), []
    while pending:
        report = audit(sha)
        statuses = {row["operation"]: row for row in report["operations"]}
        progressed = False
        for name in selected:
            if name not in pending:
                continue
            state = statuses[name]["state"]
            if state == "active":
                outcomes.append({"operation": name, "result": "already_active"})
                pending.remove(name)
                progressed = True
                break
            if state != "pending":
                raise RuntimeError("%s is %s; investigate rather than replay" % (name, state))
            if statuses[name]["missingPrerequisites"]:
                continue
            base, attempt = run_id.split("-", 1)
            step_id = "%d-%s" % (int(base) * 1000 + len(outcomes) + 1, attempt)
            cr.emit("activationStarting", {"operation": name, "runId": step_id})
            cr.run(name, sha, step_id)
            journal, envs, running, _ = live_snapshot()
            after = operation_state(name, journal, envs, running)
            if after != "active":
                raise RuntimeError("%s post-activation runtime attestation is %s" % (name, after))
            outcomes.append({"operation": name, "result": "activated"})
            pending.remove(name)
            progressed = True
            break
        if not progressed:
            missing = {name: statuses[name]["missingPrerequisites"] for name in sorted(pending)}
            raise RuntimeError("Unapproved or unverifiable prerequisites: " + json.dumps(missing))
    cr.emit("activationApplied", {"outcomes": outcomes})
    return audit(sha)


def main(args):
    if len(args) not in (3, 4):
        raise RuntimeError("Usage: activation_manager.py <audit|apply> <sha> <run-id> [approved-csv]")
    mode, sha, run_id = args[:3]
    if not cr.SHA_RE.fullmatch(sha) or not cr.RUN_RE.fullmatch(run_id):
        raise RuntimeError("Invalid SHA or run id")
    if mode == "audit" and len(args) == 3:
        audit(sha)
    elif mode == "apply" and len(args) == 4:
        apply(sha, run_id, args[3])
    else:
        raise RuntimeError("Invalid audit/apply arguments")


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except Exception as exc:
        cr.emit("activationReconciliationError", str(exc))
        raise SystemExit(1)
