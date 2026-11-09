#!/usr/bin/env python3
r"""Web client contract-parity report (Blueprint v1.0 Section 16, FE-015). REPORT-ONLY.

`apps/` belongs to the frontend team, so this never fails CI (exit 0 on every finding;
exit 2 only for a bad command line). It answers, for `apps/web/src`:

  1. Does the web app import the generated SDK or its generated types
     (`@cfmeet/platform-sdk`, `sdks/typescript/...`, `.../generated/models`)?
  2. Which handwritten API type modules exist (`src/types/**`, `*.types.ts`)?
  3. Which backend operations does the web app call, matched against the gateway route
     catalogue (`infra/api-gateway/route-manifest.json`)? For each call:
       * PUBLIC_CUSTOMER  → a generated SDK operation / model namespace exists: the call
                            could use generated types instead of handwritten ones;
       * EXCLUDED (reason)→ first-party direct ingress by design (FE-018 decision record);
       * other class      → internal / operator surface called from the browser (review);
       * UNMATCHED        → no contract operation has this path (possible drift).

Call detection is a heuristic over string literals passed to axios-style clients
(`api.get("/meetings")`, `apiV2.post(\`/meeting-spaces/${id}\`)`) and route rules; the
client's base path comes from its identifier and directory. Treat the numbers as a
trend, not an exact census.

Usage:  web_contract_parity_report.py [--json] [--web-root DIR]
Writes Markdown to stdout and, when set, appends it to $GITHUB_STEP_SUMMARY.
Standard library only.
"""
from __future__ import annotations

import json
import os
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ROUTE_MANIFEST = ROOT / "infra/api-gateway/route-manifest.json"
TS_MODELS = ROOT / "sdks/typescript/src/generated/models.ts"

SDK_IMPORT = re.compile(
    r"""from\s+['"](@cfmeet/platform-sdk[^'"]*|[^'"]*sdks/typescript[^'"]*|[^'"]*generated/(models|operations)[^'"]*)['"]"""
)
CALL = re.compile(
    r"""\b(?P<client>[A-Za-z_$][\w$]*)\s*\.\s*(?P<method>get|post|put|patch|delete)\s*(?:<[^()]*?>)?\s*\(\s*(?P<q>[`'"])(?P<path>/[^`'"\s]*)(?P=q)""",
    re.S,
)
TEMPLATE = re.compile(r"\$\{[^}]*\}|\{[^}/]*\}")
# client identifier → base path (by directory when the same name means different bases)
V1_CLIENTS = {"api", "publicApi"}
V2_CLIENTS = {"apiV2", "publicApiV2", "http"}
SOURCE_SUFFIXES = (".ts", ".tsx")
# Calling admin resource definitions (`listPath: "/v1/..."`), proxied by the web server to
# Calling: list/detail are GET, create is POST.
RESOURCE_PATH = re.compile(r"""\b(?P<kind>listPath|detailPath|createPath)\s*:\s*(?P<q>[`'"])(?P<path>/v1/[^`'"\s]*)(?P=q)""")
RESOURCE_METHOD = {"listPath": "GET", "detailPath": "GET", "createPath": "POST"}
# Routes answered by the web app's own server (not backend operations).
LOCAL_ROUTES = {"/identity"}


def base_for(client: str, rel: str) -> str | None:
    if "services/calling/" in rel or "lib/calling/" in rel or "components/calling/" in rel:
        return "" if client in {"api", "client", "callingApi"} else None
    if client in V2_CLIENTS:
        return "/api/v2"
    if client in V1_CLIENTS:
        return "/api/v1"
    return None


def normalise(path: str) -> str:
    path = TEMPLATE.sub("{}", path.split("?")[0]).rstrip("/")
    return path or "/"


def catalogue_index(manifest: dict) -> list[tuple[re.Pattern, str, dict]]:
    index = []
    for op in manifest.get("catalogue", []):
        rx = "^" + re.sub(r"\\\{[^}]*\\\}", r"[^/]+", re.escape(op["path"])) + "$"
        index.append((re.compile(rx), op["method"].upper(), op))
    # Literal segments win over parameters: try the most literal templates first.
    index.sort(key=lambda t: (t[2]["path"].count("{"), -len(t[2]["path"])))
    return index


def match(index, method: str, path: str) -> dict | None:
    probe = path.replace("{}", "x")
    for rx, m, op in index:
        if m == method and rx.match(probe):
            return op
    return None


def sdk_domains() -> set[str]:
    if not TS_MODELS.is_file():
        return set()
    names = re.findall(r"^export namespace (\w+)", TS_MODELS.read_text(encoding="utf-8"), re.M)
    norm = {"Sms": "sms", "PrivacyRelay": "privacy-relay", "DeveloperPlatform": "developer-platform",
            "Gateway": "gateway", "Meetings": "meetings"}
    return {norm.get(n, n.lower()) for n in names}


def scan(web_root: Path) -> dict:
    manifest = json.loads(ROUTE_MANIFEST.read_text(encoding="utf-8"))
    index = catalogue_index(manifest)
    domains = sdk_domains()
    src = web_root / "src"
    files = sorted(p for p in src.rglob("*") if p.suffix in SOURCE_SUFFIXES and "node_modules" not in p.parts) if src.is_dir() else []
    sdk_imports: list[str] = []
    type_modules: list[str] = []
    calls: dict[tuple[str, str], dict] = {}
    for f in files:
        rel = f.relative_to(web_root).as_posix()
        text = f.read_text(encoding="utf-8", errors="replace")
        if SDK_IMPORT.search(text):
            sdk_imports.append(rel)
        if rel.startswith("src/types/") or rel.endswith(".types.ts"):
            type_modules.append(rel)
        found = []
        for m in CALL.finditer(text):
            base = base_for(m.group("client"), rel)
            if base is not None:
                found.append((m.group("method").upper(), normalise(base + m.group("path"))))
        for m in RESOURCE_PATH.finditer(text):
            found.append((RESOURCE_METHOD[m.group("kind")], normalise(m.group("path"))))
        for method, path in found:
            if path in LOCAL_ROUTES:
                continue
            key = (method, path)
            if key in calls:
                calls[key]["sites"].append(rel)
                continue
            op = match(index, method, path)
            if op is None:
                bucket, detail = "UNMATCHED", ""
            elif op.get("class") == "EXCLUDED":
                bucket, detail = "EXCLUDED", op.get("reason", "")
            elif op.get("class") == "PUBLIC_CUSTOMER":
                bucket = "PUBLIC_CUSTOMER"
                detail = "sdk-types" if op.get("domain") in domains else "no-sdk-namespace"
            else:
                bucket, detail = op.get("class", "UNKNOWN"), ""
            calls[key] = {
                "method": method,
                "path": path,
                "bucket": bucket,
                "detail": detail,
                "operationId": (op or {}).get("operationId", ""),
                "domain": (op or {}).get("domain", ""),
                "sites": [rel],
            }
    return {
        "webRoot": web_root.relative_to(ROOT).as_posix() if web_root.is_relative_to(ROOT) else str(web_root),
        "sourceFiles": len(files),
        "sdkImports": sdk_imports,
        "handwrittenTypeModules": type_modules,
        "sdkDomains": sorted(domains),
        "calls": sorted(calls.values(), key=lambda c: (c["bucket"], c["path"], c["method"])),
    }


def markdown(r: dict) -> str:
    buckets = Counter(c["bucket"] for c in r["calls"])
    excluded = Counter(c["detail"] for c in r["calls"] if c["bucket"] == "EXCLUDED")
    public = Counter(c["detail"] for c in r["calls"] if c["bucket"] == "PUBLIC_CUSTOMER")
    out = [
        "## Web contract parity (FE-015, report-only)",
        "",
        f"Scanned `{r['webRoot']}/src`: {r['sourceFiles']} TypeScript files. This report never fails CI; `apps/` is owned by the frontend team.",
        "",
        "| Measure | Value |",
        "|---|---:|",
        f"| Files importing the generated SDK / generated types | {len(r['sdkImports'])} |",
        f"| Handwritten API type modules | {len(r['handwrittenTypeModules'])} |",
        f"| Distinct backend calls detected | {len(r['calls'])} |",
        f"| - PUBLIC_CUSTOMER with generated SDK types available | {public.get('sdk-types', 0)} |",
        f"| - PUBLIC_CUSTOMER without an SDK model namespace | {public.get('no-sdk-namespace', 0)} |",
        f"| - EXCLUDED (first-party direct ingress, FE-018) | {buckets.get('EXCLUDED', 0)} |",
        f"| - other classes (internal / operator from the browser) | {sum(v for k, v in buckets.items() if k not in ('PUBLIC_CUSTOMER', 'EXCLUDED', 'UNMATCHED'))} |",
        f"| - UNMATCHED (no contract operation: possible drift) | {buckets.get('UNMATCHED', 0)} |",
        "",
    ]
    verdict = (
        "The web app imports the generated SDK/types."
        if r["sdkImports"]
        else "The web app does **not** import the generated SDK or generated types; request/response types are handwritten (FRONTEND_HANDOFF)."
    )
    out += [verdict, ""]
    if excluded:
        out += ["EXCLUDED calls by reason: " + ", ".join(f"`{k}` {v}" for k, v in sorted(excluded.items())), ""]
    if excluded.get("CONTACT_VAULT_PRIVATE"):
        out += [
            "Contact Vault operations are called by the web app. They are served by Privacy Relay with a "
            "Contact Vault access token, never by Calling (CAL-013, docs/api/privacy-relay-contacts-upstream-handoff.md).",
            "",
        ]
    unmatched = [c for c in r["calls"] if c["bucket"] == "UNMATCHED"]
    if unmatched:
        out += ["<details><summary>Unmatched calls</summary>", "", "| Method | Path | First call site |", "|---|---|---|"]
        out += [f"| {c['method']} | `{c['path']}` | `{c['sites'][0]}` |" for c in unmatched]
        out += ["", "</details>", ""]
    other = [c for c in r["calls"] if c["bucket"] not in ("PUBLIC_CUSTOMER", "EXCLUDED", "UNMATCHED")]
    if other:
        out += ["<details><summary>Internal / operator operations called from the web app</summary>", "", "| Class | Method | Path | operationId |", "|---|---|---|---|"]
        out += [f"| {c['bucket']} | {c['method']} | `{c['path']}` | `{c['operationId']}` |" for c in other]
        out += ["", "</details>", ""]
    return "\n".join(out)


def main(argv: list[str]) -> int:
    args = argv[1:]
    as_json = False
    web_root = ROOT / "apps/web"
    while args:
        a = args.pop(0)
        if a == "--json":
            as_json = True
        elif a == "--web-root" and args:
            web_root = Path(args.pop(0)).resolve()
        else:
            print("usage: web_contract_parity_report.py [--json] [--web-root DIR]", file=sys.stderr)
            return 2
    try:
        report = scan(web_root)
        text = json.dumps(report, indent=2) if as_json else markdown(report)
    except Exception as e:  # report-only: never break CI
        text = f"## Web contract parity (FE-015, report-only)\n\nReport could not be produced: {type(e).__name__}: {e}\n"
    print(text)
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary and not as_json:
        with open(summary, "a", encoding="utf-8") as fh:
            fh.write(text + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
