#!/usr/bin/env python3
"""Generate the frontend endpoint reference from the served OpenAPI contracts.

    python3 scripts/contracts/frontend_reference.py          # write
    python3 scripts/contracts/frontend_reference.py --check  # fail if stale (CI)

Output: docs/frontend/cfmeet-frontend-endpoint-reference.md. One row per operation a CF Meet
client (web, mobile, Developer Portal) may call, grouped by surface and Swagger tag, with
method, path, authentication, summary, documented status codes and the error codes named in
the contract. Internal, operator, provider-callback and infrastructure operations are excluded
using the gateway catalogue (infra/api-gateway/route-manifest.json) and path prefixes.

The reference is derived, never hand-edited: the contract stays the single source of truth.
"""
from __future__ import annotations

import json
import re
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CONTRACTS = ROOT / "packages" / "contracts"
MANIFEST = ROOT / "infra" / "api-gateway" / "route-manifest.json"
OUT = ROOT / "docs" / "frontend" / "cfmeet-frontend-endpoint-reference.md"
METHODS = ("get", "post", "put", "patch", "delete")

# (surface title, gateway domain, contract file, base URL a client uses, how clients reach it)
SURFACES = [
    ("Meetings, accounts, organizations, workspaces (Meeting Service)", "meetings",
     "openapi/meeting-service.json", "NEXT_PUBLIC_API_URL",
     "Direct to the Meeting Service with the account session (`Authorization: Bearer <access token>`); "
     "guests use `X-Participant-Token`."),
    ("Communications Hub (contacts, CF Meet ID / QR, timeline, activity)", "communications",
     "communications/http/v1.openapi.json", "NEXT_PUBLIC_API_URL",
     "Served by the Meeting Service host under `/api/v2/communications/*` with the account session."),
    ("Calling (personal and enterprise)", "calling",
     "calling/http/v1.openapi.json", "NEXT_PUBLIC_CALLING_API_URL",
     "Web: through the same-origin server adapter `/api/calling/*` and `/api/calling-signaling/*` "
     "(the adapter holds the platform credential; the browser never does). Signaling and media "
     "credentials are per device / per session. Organization calling: `/api/v2/organizations/{id}/calling/{path}`."),
    ("Messaging (MANAGED and PRIVATE_E2EE)", "messaging",
     "messaging/http/v1.openapi.json", "NEXT_PUBLIC_MESSAGING_URL",
     "First-party clients go through the Meeting Service relay `/api/v2/messaging/relay/{path}` "
     "(personal) or `/api/v2/organizations/{id}/messaging/relay/{path}` (organization), then "
     "realtime with a session token -> ticket -> WebSocket."),
    ("Developer Portal (Developer Platform)", "developer-platform",
     "platform-developer/http/v1.openapi.json", "NEXT_PUBLIC_DEVELOPER_API_BASE_URL",
     "Developer console with the CF Meet account session bridged to the Developer Platform "
     "(`userSession`); machine clients use `platformCredential`."),
]

FRONTEND_CLASSES = {"PUBLIC_CUSTOMER", "EXCLUDED"}
SKIP_PREFIXES = ("/internal/", "/api/internal/", "/health", "/ready", "/metrics", "/swagger", "/api-docs", "/openapi")
ERROR_CODE = re.compile(r"\b[A-Z][A-Z0-9]+(?:_[A-Z0-9]+)+\b")
NOT_ERROR = {"ORGANIZATION_ONLY", "PUBLIC_PASSCODE", "INVITE_ONLY", "PRIVATE_E2EE", "FIRST_PARTY", "SUMMARY"}


def load(path: Path):
    return json.loads(path.read_text())


def resolve(doc, obj):
    """Follow a local $ref once (responses / parameters)."""
    if isinstance(obj, dict) and isinstance(obj.get("$ref"), str) and obj["$ref"].startswith("#/"):
        node = doc
        for part in obj["$ref"][2:].split("/"):
            node = node.get(part, {})
        return node
    return obj


def auth_of(doc, op) -> str:
    sec = op.get("security", doc.get("security"))
    if sec is None:
        return "none (public)"
    names = []
    for req in sec:
        names.append(" + ".join(req.keys()) if req else "none")
    labels = {
        "bearer_auth": "account session",
        "userSession": "account session (developer bridge)",
        "participant_token": "participant token",
        "platformCredential": "platform credential (server-side only)",
        "api_key_auth": "legacy API key",
        "clientToken": "Messaging client token",
        "customerCredential": "customer credential (server-side only)",
        "realtimeTicket": "realtime ticket",
        "signalingCredential": "signaling credential",
        "mediaCredential": "media credential",
        "none": "optional / public",
    }
    out = []
    for n in names:
        out.append(" + ".join(labels.get(p.strip(), p.strip()) for p in n.split(" + ")))
    return "; ".join(dict.fromkeys(out)) or "none (public)"


def codes_of(doc, op):
    statuses, errors = [], set()
    for status, resp in sorted(op.get("responses", {}).items()):
        statuses.append(status)
        resp = resolve(doc, resp)
        text = json.dumps(resp)
        for m in ERROR_CODE.findall(resp.get("description", "") + " " + text):
            if m not in NOT_ERROR and not m.startswith("X_") and len(m) <= 48:
                errors.add(m)
    return statuses, sorted(errors)


def catalogue():
    m = load(MANIFEST)
    out = {}
    for c in m["catalogue"]:
        out[(c["domain"], c["method"].upper(), c["path"])] = c["class"]
    return out


def frontend_operation(domain, method, path, classes) -> bool:
    if path.startswith(SKIP_PREFIXES) or "/admin/" in path or path.startswith("/api/v2/admin"):
        return False
    cls = classes.get((domain, method, path))
    if cls is None:
        # Not in the gateway catalogue: first-party only if it is an account/app route.
        return path.startswith(("/api/v2/", "/api/v1/", "/api/auth"))
    return cls in FRONTEND_CLASSES


def esc(s: str) -> str:
    return (s or "").replace("|", "\\|").replace("\n", " ").strip()


def render() -> str:
    classes = catalogue()
    lines = [
        "# CF Meet — frontend endpoint reference (generated)",
        "",
        "Generated by `scripts/contracts/frontend_reference.py` from the served OpenAPI contracts in",
        "`packages/contracts/` and the gateway catalogue. **Do not edit by hand**; CI (`contracts.yml`) fails",
        "when it is stale. Read it together with the handover guide",
        "`docs/frontend/cfmeet-frontend-handover.md` (auth, scopes, realtime, errors, journeys).",
        "",
        "Columns: **Auth** = security scheme(s) the contract declares; **Status** = documented HTTP statuses;",
        "**Error codes** = codes named in the contract's responses for that operation (the shared error",
        "envelope is `{\"error\": {\"code\", \"message\", \"requestId\"}}`). The contract link is the canonical",
        "Swagger/OpenAPI entry (`<file>#<operationId>`).",
        "",
    ]
    totals = []
    body = []
    for title, domain, rel, base, how in SURFACES:
        doc = load(CONTRACTS / rel)
        by_tag = defaultdict(list)
        for path, item in doc["paths"].items():
            for method in METHODS:
                op = item.get(method)
                if not op or not frontend_operation(domain, method.upper(), path, classes):
                    continue
                tag = (op.get("tags") or ["Untagged"])[0]
                by_tag[tag].append((path, method.upper(), op))
        count = sum(len(v) for v in by_tag.values())
        totals.append((title, rel, doc["info"].get("version", "?"), count))
        body += [f"## {title}", "", f"Contract `packages/contracts/{rel}` version **{doc['info'].get('version', '?')}**; "
                 f"base URL env `{base}`. {how}", ""]
        for tag in sorted(by_tag):
            body += [f"### {tag}", "", "| Method | Path | Auth | Summary | Status | Error codes | Contract |",
                     "|---|---|---|---|---|---|---|"]
            for path, method, op in sorted(by_tag[tag], key=lambda x: (x[0], x[1])):
                statuses, errors = codes_of(doc, op)
                summary = esc(op.get("summary") or op.get("description", "").split("\n")[0])
                if op.get("deprecated"):
                    summary = "**DEPRECATED** " + summary
                link = f"`{rel}#{op.get('operationId', '?')}`"
                body.append(f"| {method} | `{path}` | {esc(auth_of(doc, op))} | {summary} | {', '.join(statuses)} | "
                            f"{', '.join(errors) or '—'} | {link} |")
            body.append("")
    lines += ["## Totals", "", "| Surface | Contract | Version | Frontend operations |", "|---|---|---|---:|"]
    for title, rel, ver, n in totals:
        lines.append(f"| {title} | `{rel}` | {ver} | {n} |")
    lines.append(f"| **Total** | | | **{sum(t[3] for t in totals)}** |")
    lines.append("")
    return "\n".join(lines + body).rstrip() + "\n"


def main() -> int:
    text = render()
    if "--check" in sys.argv:
        if not OUT.exists() or OUT.read_text() != text:
            print(f"{OUT.relative_to(ROOT)} is stale; run python3 scripts/contracts/frontend_reference.py")
            return 1
        print(f"frontend endpoint reference current: {OUT.relative_to(ROOT)}")
        return 0
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(text)
    print(f"wrote {OUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
