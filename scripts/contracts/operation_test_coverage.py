#!/usr/bin/env python3
"""Operation test-reference coverage gate (INV-002).

For every OpenAPI contract in ``packages/contracts/manifest.json`` (source contracts
only; ``bundled/*`` derivatives repeat them) this lists the operations that no
automated test references, using the same path-literal heuristic as the API inventory
(``docs/audits/cfmeet-api-inventory.csv`` column ``test_ref_count``):

* the test corpus is ``services/*/tests/**``, ``certification/**`` and
  ``scripts/staging/**`` (inline ``src`` unit tests are excluded on purpose: the
  router source would otherwise "reference" every route it registers);
* every string literal containing ``/`` is a candidate. Format placeholders
  (``{id}``, ``{}``, ``${id}``), ``:param`` segments and concrete values all match a
  path parameter; a query string is ignored; ``http(s)://host`` is dropped;
* a literal that starts with a placeholder (``format!("{base}/devices/{id}")``)
  matches as a suffix (at least one static segment) only in the contract owner's own
  tests (`services/<owner>/tests`), never across services;
* an operation is referenced when one literal matches its path template segment by
  segment (static segments exactly, parameters by anything non-empty).

Registered routes that have no contract of their own are covered by declared
``MIRRORS``: Messaging serves its whole customer surface again under
``/internal/v1/delegated`` (``services/messaging/src/http/mod.rs``). A mirror operation
counts as referenced only by a literal of its full mirror path, or by a placeholder
suffix in a file that also names the mirror prefix.

The gate fails when a contract's untested count rises above the frozen ``BASELINE`` (a
new route needs a test), when a contract with untested operations is missing from the
baseline, or when a mirror rule no longer matches the router source. A count below the
baseline passes with a hint to lower it. Standard library only, no network.

Usage::

    python3 scripts/contracts/operation_test_coverage.py            # gate + summary
    python3 scripts/contracts/operation_test_coverage.py --list     # + untested operations
    python3 scripts/contracts/operation_test_coverage.py --json     # machine-readable report
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
METHODS = ("get", "put", "post", "delete", "patch", "head", "options", "trace")
TEST_GLOBS = (
    "services/*/tests/**/*",
    "certification/**/*",
    "scripts/staging/**/*",
)
TEST_SUFFIXES = {".rs", ".py", ".sh", ".mjs", ".js", ".ts"}
PARAM = "\x00"

# (contract id, mirror prefix, mirrored path prefixes, router file, router evidence).
MIRRORS: tuple[tuple[str, str, tuple[str, ...], str, str], ...] = (
    (
        "cfmeet.messaging.http-api",
        "/internal/v1/delegated",
        ("/v1/messaging/", "/v1/conversations"),
        "services/messaging/src/http/mod.rs",
        '.nest("/delegated", delegated)',
    ),
)


# Contract-wide route-existence tests (the inventory's "[route-existence only]"): the
# test walks every operation of the contract. Operations referenced only by such a sweep
# count as tested and are reported separately as ``route_only``.
SWEEPS: dict[str, tuple[str, str]] = {
    "cfmeet.communications.http-api": (
        "services/communications/tests/unified.rs",
        "fn every_published_communications_operation_is_routed",
    ),
}


def mirror_key(contract_id: str) -> str:
    return f"{contract_id}#delegated-mirror"


# Untested operations per contract, frozen after the Section 16 tests (INV-002). The
# audit counted 200 of 1,066 registered operations; with this script's per-contract view
# the count was 146 of 1,128 before Section 16 (meeting-service http-api 16, the
# Messaging delegated mirror 82) and 48 after it. Lower an entry when tests are added;
# never raise one - add a test for the new operation instead.
BASELINE: dict[str, int] = {
    "cfmeet.api-gateway.http-api": 0,
    "cfmeet.calling.communications-internal": 0,
    "cfmeet.calling.http-api": 1,
    "cfmeet.communications.communications-internal": 4,
    "cfmeet.communications.http-api": 0,
    "cfmeet.developer-platform.http-api": 1,
    "cfmeet.developer-platform.internal-api": 13,
    "cfmeet.meeting-service.communications-internal": 1,
    "cfmeet.meeting-service.http-api": 0,
    "cfmeet.messaging.communications-internal": 0,
    "cfmeet.messaging.http-api": 0,
    "cfmeet.messaging.http-api#delegated-mirror": 0,
    "cfmeet.messaging.internal-api": 0,
    "cfmeet.numbering.internal-api": 20,
    "cfmeet.organizations.internal-api": 0,
    "cfmeet.platform-access.internal-api": 3,
    "cfmeet.platform.internal-http": 0,
    "cfmeet.privacy-relay.http-api": 0,
    "cfmeet.privacy-relay.internal-api": 0,
    "cfmeet.sms-gateway.http-api": 0,
    "cfmeet.sms-gateway.internal-api": 0,
    "cfmeet.telephony.internal-http-api": 5,
}

LITERAL_PATTERNS = (
    re.compile(r'r#+"(.*?)"#+', re.S),
    re.compile(r'"((?:[^"\\\n]|\\.)*)"'),
    re.compile(r"'((?:[^'\\\n]|\\.)*)'"),
    re.compile(r"`((?:[^`\\]|\\.)*)`", re.S),
)
PLACEHOLDER = re.compile(r"\$\{[^}]*\}|\{[^{}]*\}")


def template_segments(path: str) -> list[str | None]:
    """``/a/{id}/b`` -> ``["a", None, "b"]`` (``None`` = parameter)."""
    out: list[str | None] = []
    for seg in path.strip("/").split("/"):
        if seg.startswith("{") and seg.endswith("}") or seg.startswith(":"):
            out.append(None)
        else:
            out.append(seg)
    return out if path.strip("/") else []


def literal_paths(text: str) -> set[tuple[bool, tuple[str, ...]]]:
    """Candidate paths in one file: ``(starts_with_placeholder, segments)``."""
    found: set[tuple[bool, tuple[str, ...]]] = set()
    for pattern in LITERAL_PATTERNS:
        for m in pattern.finditer(text):
            body = m.group(1)
            if "/" not in body or len(body) > 2000:
                continue
            for token in body.split():
                parsed = parse_literal(token)
                if parsed:
                    found.add(parsed)
    return found


def parse_literal(token: str) -> tuple[bool, tuple[str, ...]] | None:
    token = token.strip("\"'`,;()[]")
    if "/" not in token:
        return None
    token = PLACEHOLDER.sub(PARAM, token)
    token = re.split(r"[?#]", token, maxsplit=1)[0]
    if "://" in token:
        rest = token.split("://", 1)[1]
        token = "/" + rest.split("/", 1)[1] if "/" in rest else ""
    if not token:
        return None
    if token == "/":
        return False, ()
    leading = False
    if not token.startswith("/"):
        head, _, tail = token.partition("/")
        if head.strip(PARAM) == "" and head:
            leading = True
            token = "/" + tail
        else:
            return None
    segs = token[1:].split("/")
    norm: list[str] = []
    for i, seg in enumerate(segs):
        if PARAM in seg or seg.startswith(":") or (seg == "" and i == len(segs) - 1 and i > 0):
            norm.append(PARAM)
        elif seg == "":
            return None
        else:
            norm.append(seg)
    if not norm or all(s == PARAM for s in norm):
        return None
    return leading, tuple(norm)


def seg_match(template: list[str | None], literal: tuple[str, ...]) -> bool:
    if len(template) != len(literal):
        return False
    for t, lit in zip(template, literal):
        if t is None:
            if not lit:
                return False
        elif lit != t:
            return False
    return True


def references(
    template: list[str | None],
    full: set[tuple[str, ...]],
    suffixes: set[tuple[str, ...]],
) -> bool:
    """``full``: literals that start at the root; ``suffixes``: placeholder-led literals."""
    if not template:
        return () in full
    for segs in full:
        if seg_match(template, segs):
            return True
    for segs in suffixes:
        if len(segs) > len(template):
            continue
        tail = template[len(template) - len(segs):]
        if any(t is not None for t in tail) and seg_match(tail, segs):
            return True
    return False


def load_contracts(root: Path) -> list[tuple[str, str, list[tuple[str, str]]]]:
    """[(contract id, owner, [(METHOD, path)])] for every source OpenAPI contract."""
    manifest = json.loads((root / "packages/contracts/manifest.json").read_text(encoding="utf-8"))
    out = []
    for c in manifest["contracts"]:
        if c.get("kind") != "openapi" or str(c.get("path", "")).startswith("bundled/"):
            continue
        doc = json.loads((root / "packages/contracts" / c["path"]).read_text(encoding="utf-8"))
        ops = [
            (method.upper(), path)
            for path, item in sorted(doc.get("paths", {}).items())
            for method in METHODS
            if isinstance(item, dict) and method in item
        ]
        out.append((c["id"], c.get("owner", ""), ops))
    return sorted(out)


Corpus = list[tuple[str, set[tuple[bool, tuple[str, ...]]], str]]


def load_corpus(root: Path) -> Corpus:
    files: dict[Path, None] = {}
    for g in TEST_GLOBS:
        for p in root.glob(g):
            if p.is_file() and p.suffix in TEST_SUFFIXES and "target" not in p.parts and "node_modules" not in p.parts:
                files[p] = None
    corpus = []
    for p in sorted(files):
        try:
            text = p.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        corpus.append((p.relative_to(root).as_posix(), literal_paths(text), text))
    return corpus


def split_literals(corpus: Corpus, owner: str, require: str | None = None) -> tuple[set, set]:
    """(root-anchored literals from every test file, placeholder-led literals from the
    owner's own tests only: a ``{base}/v1/webhooks`` in another service's tests says
    nothing about this contract). ``require``: suffix files must also contain it."""
    own = f"services/{owner}/tests/"
    full: set[tuple[str, ...]] = set()
    suffixes: set[tuple[str, ...]] = set()
    for rel, paths, text in corpus:
        suffix_ok = rel.startswith(own) and (require is None or require in text)
        for leading, segs in paths:
            if not leading:
                full.add(segs)
            elif suffix_ok:
                suffixes.add(segs)
    return full, suffixes


def analyse(
    root: Path,
    mirrors: tuple = MIRRORS,
    sweeps: dict[str, tuple[str, str]] = SWEEPS,
) -> tuple[dict[str, dict], list[str]]:
    """({contract key: {"owner", "total", "untested", "route_only"}}, errors)."""
    errors: list[str] = []
    contracts = load_contracts(root)
    corpus = load_corpus(root)
    report: dict[str, dict] = {}
    by_id = {cid: (owner, ops) for cid, owner, ops in contracts}
    texts = {rel: text for rel, _, text in corpus}
    for cid, owner, ops in contracts:
        full, suffixes = split_literals(corpus, owner)
        unreferenced = [(m, p) for m, p in ops if not references(template_segments(p), full, suffixes)]
        swept = False
        if cid in sweeps:
            rel, marker = sweeps[cid]
            if marker in texts.get(rel, ""):
                swept = True
            else:
                errors.append(f"sweep for {cid} no longer found ({rel} must contain {marker!r}); update SWEEPS")
        report[cid] = {
            "owner": owner,
            "total": len(ops),
            "untested": [] if swept else unreferenced,
            "route_only": unreferenced if swept else [],
        }
    for cid, prefix, mirrored, router, evidence in mirrors:
        src = root / router
        if not src.is_file() or evidence not in src.read_text(encoding="utf-8"):
            errors.append(f"mirror rule for {cid} no longer matches {router} (expected {evidence!r}); update MIRRORS")
            continue
        if cid not in by_id:
            errors.append(f"mirror rule names unknown contract {cid}")
            continue
        owner, ops = by_id[cid]
        mirror_ops = [(m, prefix + p) for m, p in ops if p.startswith(mirrored)]
        full, suffixes = split_literals(corpus, owner, require=prefix)
        untested = [(m, p) for m, p in mirror_ops if not references(template_segments(p), full, suffixes)]
        report[mirror_key(cid)] = {"owner": owner, "total": len(mirror_ops), "untested": untested, "route_only": []}
    return report, errors


def gate(report: dict[str, dict], baseline: dict[str, int]) -> tuple[list[str], list[str]]:
    """(failures, hints)."""
    failures, hints = [], []
    for key, entry in sorted(report.items()):
        n = len(entry["untested"])
        if key not in baseline:
            if n:
                failures.append(f"{key}: {n} untested operations and no BASELINE entry (add tests, or freeze the count deliberately)")
            continue
        if n > baseline[key]:
            new = ", ".join(f"{m} {p}" for m, p in entry["untested"])
            failures.append(f"{key}: {n} untested operations > baseline {baseline[key]} - add a test that references the new path. Untested now: {new}")
        elif n < baseline[key]:
            hints.append(f"{key}: {n} untested < baseline {baseline[key]} - lower BASELINE[{key!r}] to {n}")
    for key in sorted(set(baseline) - set(report)):
        failures.append(f"BASELINE entry {key} matches no contract (remove it)")
    return failures, hints


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--root", type=Path, default=ROOT)
    ap.add_argument("--list", action="store_true", help="print every untested operation")
    ap.add_argument("--json", action="store_true", help="print the report as JSON")
    args = ap.parse_args(argv)
    report, errors = analyse(args.root)
    failures, hints = gate(report, BASELINE)
    failures = errors + failures
    if args.json:
        print(json.dumps({
            k: {
                "owner": v["owner"],
                "total": v["total"],
                "untested": [f"{m} {p}" for m, p in v["untested"]],
                "route_only": [f"{m} {p}" for m, p in v["route_only"]],
            }
            for k, v in sorted(report.items())
        }, indent=2))
    else:
        total = sum(v["total"] for v in report.values())
        untested = sum(len(v["untested"]) for v in report.values())
        print(f"{'contract':58} {'ops':>5} {'untested':>9} {'baseline':>9} {'route-only':>11}")
        for key, v in sorted(report.items()):
            print(f"{key:58} {v['total']:>5} {len(v['untested']):>9} {BASELINE.get(key, '-'):>9} {len(v['route_only']):>11}")
            if args.list:
                for m, p in v["untested"]:
                    print(f"    {m:7} {p}")
        print(f"{'total':58} {total:>5} {untested:>9}")
    for h in hints:
        print(f"HINT: {h}")
    for f in failures:
        print(f"FAIL: {f}")
    if failures:
        return 1
    if not args.json:
        print("PASS: no contract has more untested operations than its frozen baseline.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
