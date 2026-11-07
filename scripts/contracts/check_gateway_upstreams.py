#!/usr/bin/env python3
"""Prove every API Gateway route points at a handler that exists upstream (INV-001).

The gateway manifest (``infra/api-gateway/route-manifest.json``) is generated from
the service OpenAPI contracts, so a contract can keep an operation after its Rust
handler is removed or renamed and the gateway would forward to a 404. This gate
reads the axum routers of each upstream service statically and fails when a
manifest ``method upstreamPath`` has no matching route.

How a route is found, per upstream crate:

* every ``.route("<literal>", <method router>)`` gives a path and its methods
  (``get(..)``, ``post(..)``, ``.put(..)`` chains, ``any(..)``, ``on(MethodFilter::X, ..)``);
* every ``.nest("<literal>", ..)`` gives a possible prefix;
* a manifest path matches when it equals ``prefix* + route`` for some sequence of
  nest prefixes of that crate (empty included) and the route allows the method.

Literal ``const NAME: &str = "/path";`` values used as route or nest paths are
resolved too. Catch-all routes (``/*path``) never satisfy a manifest path on
their own: a catch-all that forwards to another service's dispatch table must be
declared in ``RELAYS``, and each path under it is checked against the
``("METHOD", ["segment", var, ..])`` arms of that table.

Run ``python scripts/contracts/check_gateway_upstreams.py``; exits 1 on any orphan.
"""
from __future__ import annotations

import json
import re
import sys
from itertools import product
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "infra/api-gateway/route-manifest.json"

# Gateway upstream id -> service source roots whose routers it serves.
UPSTREAMS = {
    "cfmeet.meeting-service": ["services/meeting-service/src"],
    "cfmeet.calling": ["services/calling/src"],
    "cfmeet.messaging": ["services/messaging/src"],
    "cfmeet.developer-platform": ["services/developer-platform/src"],
    "cfmeet.numbering": ["services/numbering/src"],
    "cfmeet.sms-gateway": ["services/sms-gateway/src"],
    "cfmeet.api-gateway": ["services/api-gateway/src"],
}

# Catch-all relays: upstream -> [(path prefix served by a `/*path` route,
# source file holding the `match (method, parts.as_slice())` dispatch table)].
RELAYS: dict[str, list[tuple[str, str]]] = {
    # meeting-service `services/communications.rs::public_routes` forwards
    # `/api/v2/communications/*path` to Communications `dispatch_inner`.
    "cfmeet.meeting-service": [("/api/v2/communications", "services/communications/src/api.rs")],
}

METHODS = ("get", "post", "put", "patch", "delete", "head", "options", "trace")
STR = r'"((?:[^"\\]|\\.)*)"'


CHAR_LITERAL = re.compile(r"'(?:\\u\{[0-9a-fA-F]+\}|\\x[0-9a-fA-F]{2}|\\.|[^\\'])'")


def strip_comments(src: str) -> str:
    """Drop Rust comments (nested block comments included) but keep string literals."""
    out, i, n = [], 0, len(src)
    while i < n:
        c, nxt = src[i], src[i + 1 : i + 2]
        if c == "/" and nxt == "/":
            while i < n and src[i] != "\n":
                i += 1
        elif c == "/" and nxt == "*":
            depth, i = 1, i + 2
            while i < n and depth:
                if src.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif src.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
        elif c == "r" and re.match(r'r#*"', src[i:]) and re.search(r"(?:^|[^A-Za-z0-9_])b?$", src[max(0, i - 2) : i]):
            hashes = re.match(r'r(#*)"', src[i:]).group(1)
            end = src.find('"' + hashes, i + len(hashes) + 2)
            end = n if end < 0 else end + 1 + len(hashes)
            out.append(src[i:end])
            i = end
        elif c == '"':
            j = i + 1
            while j < n and src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            out.append(src[i : j + 1])
            i = j + 1
        elif c == "'" and (char := CHAR_LITERAL.match(src, i)):
            # a char literal such as '"' must not open a string; lifetimes ('a) fall through
            out.append(char.group(0))
            i = char.end()
        else:
            out.append(c)
            i += 1
    return "".join(out)


def balanced(src: str, start: int) -> str:
    """Text between the parenthesis at ``start`` and its match (exclusive)."""
    depth, i, in_str = 0, start, False
    while i < len(src):
        c = src[i]
        if in_str:
            if c == "\\":
                i += 1
            elif c == '"':
                in_str = False
        elif c == '"':
            in_str = True
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return src[start + 1 : i]
        i += 1
    return src[start + 1 :]


def split_first_arg(args: str) -> tuple[str, str]:
    depth, in_str = 0, False
    for i, c in enumerate(args):
        if in_str:
            if c == '"' and args[i - 1] != "\\":
                in_str = False
        elif c == '"':
            in_str = True
        elif c in "([{<":
            depth += 1
        elif c in ")]}>":
            depth -= 1
        elif c == "," and depth == 0:
            return args[:i].strip(), args[i + 1 :]
    return args.strip(), ""


def methods_of(expr: str) -> set[str]:
    found = set(re.findall(r"(?<![A-Za-z0-9_])(?:routing::|axum::routing::)?(" + "|".join(METHODS) + r")(?:_service)?\s*\(", expr))
    found |= {m.lower() for m in re.findall(r"MethodFilter::([A-Z]+)", expr)}
    if re.search(r"(?<![A-Za-z0-9_])any(?:_service)?\s*\(", expr):
        found |= set(METHODS)
    return found


def normalize(path: str) -> str:
    path = re.sub(r"\{[^}/]+\}", "{}", path)
    path = re.sub(r"(?<=/):[^/]+", "{}", path)
    path = re.sub(r"(?<=/)\*[^/]+", "{*}", path)
    return path.rstrip("/") or "/"


class Crate:
    def __init__(self, roots: list[str]):
        self.routes: dict[str, set[str]] = {}
        self.prefixes: set[str] = set()
        sources = [strip_comments(p.read_text(encoding="utf-8")) for r in roots for p in sorted((ROOT / r).rglob("*.rs"))]
        self.text = "\n".join(sources)
        consts = dict(re.findall(r"const\s+([A-Z0-9_]+)\s*:\s*&(?:'static\s+)?str\s*=\s*" + STR, self.text))
        for kind in ("route", "nest", "route_service", "nest_service"):
            for m in re.finditer(r"\.\s*" + kind + r"\s*\(", self.text):
                first, rest = split_first_arg(balanced(self.text, m.end() - 1))
                lit = re.fullmatch(STR, first)
                path = lit.group(1) if lit else consts.get(first.split("::")[-1])
                if path is None:
                    continue
                if kind.startswith("route"):
                    found = methods_of(rest) if kind == "route" else set(METHODS)
                    self.routes.setdefault(normalize(path), set()).update(found)
                else:
                    self.prefixes.add(normalize(path))
        self.prefixes.discard("/")

    def resolves(self, method: str, path: str) -> bool:
        method = method.lower()
        for tail in self._tails(normalize(path), 0):
            allowed = self.routes.get(tail, set())
            if method in allowed or (method == "head" and "get" in allowed):
                return True
        return False

    def _tails(self, target: str, depth: int) -> set[str]:
        """The path itself plus what remains after stripping nest prefixes."""
        out = {target}
        if depth < 3:  # deeper than any service nests
            for prefix in self.prefixes:
                rest = self._strip(target, prefix)
                if rest is not None:
                    out |= self._tails(rest, depth + 1)
        return out

    @staticmethod
    def _strip(target: str, prefix: str) -> str | None:
        t, p = target.split("/"), prefix.split("/")
        if len(t) < len(p) or any(a != b and b != "{}" for a, b in zip(t, p)):
            return None
        return "/" + "/".join(t[len(p):]) if len(t) > len(p) else "/"


def dispatch_arms(source: str) -> list[tuple[str, list[str | None]]]:
    """``("GET", ["me", id])`` arms -> (method, segments); None = variable, "**" = rest."""
    text = strip_comments((ROOT / source).read_text(encoding="utf-8"))
    arms = []
    for method, body in re.findall(r'\(\s*"([A-Z]+)"\s*,\s*\[([^\]]*)\]\s*\)', text):
        segs: list[str | None] = []
        for part in [x.strip() for x in body.split(",") if x.strip()]:
            lit = re.fullmatch(STR, part)
            segs.append(lit.group(1) if lit else ("**" if part.endswith("..") else None))
        arms.append((method.lower(), segs))
    return arms


def relay_resolves(up: str, method: str, path: str, cache: dict) -> bool:
    for prefix, source in RELAYS.get(up, []):
        if not path.startswith(prefix + "/"):
            continue
        parts = path[len(prefix) + 1 :].split("/")
        if source not in cache:
            cache[source] = dispatch_arms(source)
        for m, segs in cache[source]:
            if m != method.lower():
                continue
            if segs and segs[-1] == "**":
                ok = len(parts) >= len(segs) - 1 and all(s is None or s == p for s, p in zip(segs[:-1], parts))
            else:
                ok = len(parts) == len(segs) and all(s is None or s == p for s, p in zip(segs, parts))
            if ok:
                return True
    return False


def self_test(manifest: dict, crates: dict[str, Crate]) -> list[str]:
    """The gate must reject paths that no router serves; prove it on real crates."""
    problems = []
    seen = set()
    for r in manifest["routes"]:
        up = r["upstream"]
        if up in seen or up not in crates:
            continue
        seen.add(up)
        path = r.get("upstreamPath") or r["pattern"]
        crate = crates[up]
        if not crate.resolves(r["method"], path):
            continue
        for bogus in (path + "/inv001-missing", "/inv001-missing" + path, path.rsplit("/", 1)[0] + "/inv001-missing"):
            if crate.resolves(r["method"], bogus):
                problems.append(f"self-test: {up} wrongly resolves {r['method']} {bogus}")
    for up, relays in RELAYS.items():
        for prefix, source in relays:
            arms: dict = {}
            for bogus in (prefix + "/inv001-missing", prefix + "/me/inv001-missing"):
                if relay_resolves(up, "GET", bogus, arms) or crates.get(up, Crate([])).resolves("GET", bogus):
                    problems.append(f"self-test: {up} wrongly resolves GET {bogus}")
            if not arms.get(source):
                problems.append(f"self-test: no dispatch arms parsed from {source}")
    return problems


def main() -> int:
    manifest = json.loads(MANIFEST.read_text(encoding="utf-8"))
    crates: dict[str, Crate] = {}
    orphans, unknown = [], set()
    arms: dict = {}
    for r in manifest["routes"]:
        up = r["upstream"]
        if up not in UPSTREAMS:
            unknown.add(up)
            continue
        if up not in crates:
            crates[up] = Crate(UPSTREAMS[up])
        crate = crates[up]
        path = r.get("upstreamPath") or r["pattern"]
        if crate.resolves(r["method"], path):
            continue
        if relay_resolves(up, r["method"], path, arms):
            continue
        orphans.append(f"{r['id']}: {r['method']} {path} -> {up}")
    weak = self_test(manifest, crates)
    for up in sorted(unknown):
        print(f"FAIL: manifest upstream {up!r} has no source mapping in check_gateway_upstreams.py")
    for o in orphans:
        print(f"FAIL: no upstream handler for {o}")
    for w in weak:
        print(f"FAIL: {w}")
    if orphans or unknown or weak:
        return 1
    print(f"PASS: all {len(manifest['routes'])} gateway routes resolve to an upstream axum route.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
