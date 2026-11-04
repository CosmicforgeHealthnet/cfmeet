"""Controlled app-to-app Calling beta activation; never certifies enterprise or capture.

Requires existing Calling release module. Existing full certification gates remain
unchanged; this operation has intentionally narrower scope and must be reported as beta.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

import production as prod

BASE = Path("/opt/cfmeet-core")
BFF_CREDENTIAL = BASE / "secrets" / "cfmeet-web-bff.credential"
FORBIDDEN = (
    "CALLING_PUBLIC_OUTBOUND_ENABLED",
    "CALLING_PUBLIC_INBOUND_ENABLED",
    "CALLING_CALLBACK_RELAY_ENABLED",
    "CALLING_SYNTHETIC_INTERCONNECT_ENABLED",
    "ENTERPRISE_CALLING_ENABLED",
    "VOICEMAIL_ENABLED",
    "CALL_RECORDING_ENABLED",
)
APP = (
    "CALLING_NEW_CALLS_ENABLED",
    "CALLING_APP_CALLING_ENABLED",
)


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def bff_status(path, authorized=True):
    cmd = [
        "curl", "-sS", "--fail-with-body", "--max-time", "8",
        "--resolve", "api.cfmeet.com:443:127.0.0.1",
        "-o", "/dev/null", "-w", "%{http_code}",
    ]
    if not authorized:
        # 401 is expected for anonymous requests, so don't use --fail.
        cmd.remove("--fail-with-body")
    if authorized:
        require(BFF_CREDENTIAL.is_file(), "CF Meet web BFF credential unavailable")
        token = BFF_CREDENTIAL.read_text().strip()
        require(bool(token), "CF Meet web BFF credential empty")
        cmd.extend(["-H", "Authorization: Bearer " + token])
    cmd.append("https://api.cfmeet.com" + path)
    process = subprocess.run(cmd, text=True, capture_output=True)
    require(process.returncode == 0, "Public Calling API request failed")
    return process.stdout.strip()


def smoke(label):
    prod.identity()
    prod.regression()
    require(bff_status("/v1/calls") == "200",
            "Authorized web BFF cannot list calls")
    require(bff_status("/v1/calls", authorized=False) == "401",
            "Anonymous calls API authorization regression")
    require(bff_status("/v1/calling/signaling", authorized=False) == "401",
            "Anonymous signaling authorization regression")
    actual = prod.env_of(prod.inspect("cfmeet-calling"))
    for flag in FORBIDDEN:
        require(actual.get(flag, "false") == "false",
                "Unsafe feature unexpectedly enabled: " + flag)
    require(actual.get("CALLING_MEDIA_PROVIDER") == "livekit",
            "Calling media provider is not LiveKit")
    require(actual.get("CALLING_CAPTURE_PROVIDER") == "livekit-egress",
            "Calling capture provider unexpectedly changed")
    require(actual.get("CALLING_APP_CALLING_ENABLED") == "true",
            "App-to-app Calling switch not enabled")
    require(not prod.inspect("cfmeet-calling")["HostConfig"].get("PortBindings"),
            "Calling must not have host-published ports")
    print(label + ": verified Calling, Meetings, Messaging and Platform readiness, anonymous isolation")


def main():
    require(len(sys.argv) == 3, "Expected git commit SHA and workflow run ID")
    sha, run_id = sys.argv[1:]
    require(len(sha) == 40 and all(c in "0123456789abcdef" for c in sha),
            "Invalid release commit")
    require(run_id.isdigit(), "Invalid release run ID")
    prod.identity()
    for name in ("cfmeet-production-backend", "cfmeet-platform-access",
                 "cfmeet-messaging", "lk-server", "cfmeet-calling-redis",
                 "cfmeet-calling-egress"):
        require(prod.inspect(name)["State"]["Running"], "Dependency not running: " + name)
    require(prod.inspect("cfmeet-calling-egress")["State"]["Health"]["Status"] == "healthy",
            "Egress is not healthy")
    smoke("PRE_ACTIVATION")
    before = prod.env_of(prod.inspect("cfmeet-calling"))
    for flag in FORBIDDEN:
        require(before.get(flag, "false") == "false",
                "Unsafe feature already enabled: " + flag)
    require(before.get("CALLING_NEW_CALLS_ENABLED") in ("true", "false"),
            "Unknown master calling switch")
    if before.get("CALLING_NEW_CALLS_ENABLED") == "true":
        print("APP_BETA: already enabled; no container replaced")
        return

    overrides = {
        "CALLING_NEW_CALLS_ENABLED": "true",
        "CALLING_APP_CALLING_ENABLED": "true",
        **{flag: "false" for flag in FORBIDDEN},
    }
    # Preserve the previous rollback journal outside the new transaction; never
    # overwrite or delete the earlier rollback container.
    old_journal = BASE / "calling-activation-rollback.json"
    if old_journal.is_file():
        snapshot = BASE / "production-evidence" / ("app-beta-previous-rollback-" + run_id + ".json")
        prod.write_private(snapshot, old_journal.read_text())
    # This helper clones immutable image, runtime settings, mounts and networks;
    # it restores the old container itself if recreation or startup fails.
    prod.recreate_calling(overrides, require_acceptance=False)
    try:
        actual = prod.env_of(prod.inspect("cfmeet-calling"))
        for flag, expected in overrides.items():
            require(actual.get(flag) == expected,
                    "Deployed feature flag incorrect: " + flag)
        smoke("POST_ACTIVATION")
    except Exception:
        # recreation completed; the original is retained as the new rollback.
        prod.rollback_calling()
        print("APP_BETA: post-activation smoke failed; restored previous Calling")
        raise

    evidence = {
        "schema": 1,
        "status": "APP_BETA_ENABLED_NOT_PRODUCTION_CERTIFIED",
        "git_sha": sha,
        "github_run_id": run_id,
        "timestamp": int(time.time()),
        "running_calling_image": prod.inspect("cfmeet-calling")["Image"],
        "effective_flags": {k: prod.env_of(prod.inspect("cfmeet-calling")).get(k, "false")
                            for k in APP + FORBIDDEN},
        "scope": "web app-to-app Calling; carrier, enterprise, voicemail, recording off",
        "pending": [
            "two-real-account voice/video call acceptance",
            "mobile push/background calling acceptance",
            "enterprise permissions and production flows",
            "voicemail/recording encryption and end-to-end consent",
            "off-server backup and restore",
        ],
    }
    prod.write_private(BASE / "production-evidence" /
                       ("app-beta-" + run_id + ".json"), json.dumps(evidence, indent=2))
    print("APP_BETA_ENABLED: true")
    print("PUBLIC_PSTN_ENTERPRISE_VOICEMAIL_RECORDING: disabled")
    print("FULL_APP_PRODUCTION_CERTIFICATION: pending live two-account acceptance")
    print("ROLLBACK: existing cfmeet-calling-production workflow operation=rollback")


if __name__ == "__main__":
    os.umask(0o077)
    try:
        main()
    except Exception as exc:
        # Do not expose tokens, HTTP contents, signed artifact URLs or service env.
        print("APP_BETA_RELEASE_FAILED:", type(exc).__name__)
        sys.exit(1)
