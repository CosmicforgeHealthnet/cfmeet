#!/usr/bin/env python3
"""First-party direct-ingress operation list (Blueprint v1.0 Section 16, FE-018).

The CF Meet apps (web, mobile) call the services on their own ingress
(`api.cfmeet.com`); the API Gateway is the ingress for customer applications that
authenticate with a platform credential. Operations that exist in a service contract but
are deliberately NOT routed by the gateway are classified `EXCLUDED` (with a reason code)
in `infra/api-gateway/public-routes.json`. This script renders that list, grouped by
reason, into the generated block of
`docs/architecture/first-party-direct-ingress-decision.md`, so the decision record can
never drift from the allow-list.

Inputs (repository files only; the result never depends on apps/):
  * infra/api-gateway/public-routes.json   reason texts + EXCLUDED operations
  * infra/api-gateway/route-manifest.json  catalogue (contract, operationId) — must agree

Rules (exit 1 on violation):
  * every EXCLUDED operation has a reason code that `public-routes.json` defines;
  * the EXCLUDED sets of public-routes.json and the route-manifest catalogue are equal
    (method + path); an EXCLUDED operation never appears in the routable `routes`;
  * with --check: the generated block of the decision record is current.

Usage:  first_party_ingress.py            rewrite the generated block
        first_party_ingress.py --check    exit 1 if the block is missing or stale
        first_party_ingress.py --summary  print counts by reason (no write)
Standard library only.
"""
from __future__ import annotations

import json
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PUBLIC_ROUTES = ROOT / "infra/api-gateway/public-routes.json"
ROUTE_MANIFEST = ROOT / "infra/api-gateway/route-manifest.json"
DOC = ROOT / "docs/architecture/first-party-direct-ingress-decision.md"
BEGIN = "<!-- BEGIN GENERATED: scripts/contracts/first_party_ingress.py -->"
END = "<!-- END GENERATED: scripts/contracts/first_party_ingress.py -->"


def load(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def collect(public_routes: dict, manifest: dict) -> tuple[dict[str, list[dict]], list[str]]:
    """EXCLUDED operations grouped by reason, plus rule violations."""
    errors: list[str] = []
    reasons = public_routes.get("reasons", {})
    catalogue = {
        (o["method"].upper(), o["path"]): o
        for o in manifest.get("catalogue", [])
        if o.get("class") == "EXCLUDED"
    }
    routed = {(r["method"].upper(), r["pattern"]) for r in manifest.get("routes", [])}
    groups: dict[str, list[dict]] = defaultdict(list)
    seen: set[tuple[str, str]] = set()
    for op in public_routes.get("operations", []):
        if op.get("class") != "EXCLUDED":
            continue
        key = (op["method"].upper(), op["path"])
        reason = op.get("reason")
        if not reason or reason not in reasons:
            errors.append(f"{key[0]} {key[1]}: EXCLUDED without a defined reason ({reason!r})")
            continue
        if key in routed:
            errors.append(f"{key[0]} {key[1]}: EXCLUDED but routed by the gateway")
        entry = catalogue.get(key)
        if entry is None:
            errors.append(f"{key[0]} {key[1]}: EXCLUDED in public-routes.json but not in the route-manifest catalogue")
        elif entry.get("reason") != reason:
            errors.append(f"{key[0]} {key[1]}: reason {reason} in public-routes.json, {entry.get('reason')} in the catalogue")
        seen.add(key)
        groups[reason].append(
            {
                "domain": op.get("domain", ""),
                "method": key[0],
                "path": key[1],
                "operationId": (entry or {}).get("operationId", ""),
                "contract": (entry or {}).get("contract", ""),
            }
        )
    for key in sorted(set(catalogue) - seen):
        errors.append(f"{key[0]} {key[1]}: EXCLUDED in the route-manifest catalogue but not in public-routes.json")
    for ops in groups.values():
        ops.sort(key=lambda o: (o["path"], o["method"]))
    return dict(sorted(groups.items())), errors


def render(public_routes: dict, groups: dict[str, list[dict]]) -> str:
    reasons = public_routes.get("reasons", {})
    total = sum(len(v) for v in groups.values())
    lines = [
        BEGIN,
        "",
        f"Generated from `infra/api-gateway/public-routes.json` (EXCLUDED operations, {total} in total). "
        "Regenerate with `python3 scripts/contracts/first_party_ingress.py`; CI runs it with `--check`.",
        "",
        "| Reason code | Operations | Domains | Meaning (from `public-routes.json`) |",
        "|---|---:|---|---|",
    ]
    for reason, ops in groups.items():
        domains = ", ".join(sorted({o["domain"] for o in ops}))
        meaning = reasons.get(reason, "").replace("|", "\\|")
        lines.append(f"| `{reason}` | {len(ops)} | {domains} | {meaning} |")
    lines.append(f"| **Total** | **{total}** | | |")
    for reason, ops in groups.items():
        lines += ["", f"#### `{reason}` ({len(ops)})", "", "| Method | Path | operationId | Contract |", "|---|---|---|---|"]
        for o in ops:
            lines.append(f"| {o['method']} | `{o['path']}` | `{o['operationId']}` | `{o['contract']}` |")
    lines += ["", END]
    return "\n".join(lines)


def splice(doc: str, block: str) -> str | None:
    a, b = doc.find(BEGIN), doc.find(END)
    if a < 0 or b < 0 or b < a:
        return None
    return doc[:a] + block + doc[b + len(END):]


def main(argv: list[str]) -> int:
    args = argv[1:]
    if args not in ([], ["--check"], ["--summary"]):
        print(__doc__.split("Usage:")[1].split("Standard")[0].strip(), file=sys.stderr)
        return 2
    public_routes, manifest = load(PUBLIC_ROUTES), load(ROUTE_MANIFEST)
    groups, errors = collect(public_routes, manifest)
    for e in errors:
        print(f"first_party_ingress: {e}", file=sys.stderr)
    if errors:
        return 1
    if args == ["--summary"]:
        for reason, ops in groups.items():
            print(f"{reason}\t{len(ops)}")
        print(f"TOTAL\t{sum(len(v) for v in groups.values())}")
        return 0
    if not DOC.is_file():
        print(f"first_party_ingress: {DOC.relative_to(ROOT)} is missing", file=sys.stderr)
        return 1
    current = DOC.read_text(encoding="utf-8")
    updated = splice(current, render(public_routes, groups))
    if updated is None:
        print(f"first_party_ingress: generated-block markers missing in {DOC.relative_to(ROOT)}", file=sys.stderr)
        return 1
    if args == ["--check"]:
        if updated != current:
            print(
                f"first_party_ingress: {DOC.relative_to(ROOT)} is stale; run python3 scripts/contracts/first_party_ingress.py",
                file=sys.stderr,
            )
            return 1
        print(f"first_party_ingress: {sum(len(v) for v in groups.values())} EXCLUDED operations, decision record current")
        return 0
    if updated != current:
        DOC.write_text(updated, encoding="utf-8", newline="\n")
        print(f"first_party_ingress: rewrote {DOC.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
