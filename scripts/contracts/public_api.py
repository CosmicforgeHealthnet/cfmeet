#!/usr/bin/env python3
"""Part VI §51 — the public API catalogue: gateway route manifest and public OpenAPI.

Inputs (authoritative):
  * the source OpenAPI contracts of every customer-facing service (``SOURCES``);
  * ``infra/api-gateway/public-routes.json`` — the EXPLICIT allow-list: one entry per
    operation that is PUBLIC_CUSTOMER (routable) or deliberately EXCLUDED (with a reason).

Every operation of every source document gets exactly one class:

  INTERNAL_SERVICE      /internal/** service-authenticated operations
  OPERATOR_ADMIN        administration (/admin/ segments, operator tags)
  PROVIDER_CALLBACK     carrier / media / SMS provider callbacks
  PUBLIC_INFRASTRUCTURE probes and documentation
  PUBLIC_CUSTOMER       explicitly allow-listed customer operations (routable)
  EXCLUDED              explicitly not routed by the public gateway (reason recorded)

Default deny: an operation that is neither auto-classified non-public nor listed in the
allow-list FAILS the build, so a new contract operation can never become routable (or be
silently ignored) without an explicit decision. An allow-list entry can never make an
internal / admin / provider operation public (the build fails).

Outputs (generated, checked by CI with ``--check``):
  * ``infra/api-gateway/route-manifest.json`` — the full classified catalogue plus the
    routing data of every PUBLIC_CUSTOMER operation (consumed by services/api-gateway);
  * ``packages/contracts/bundled/public/<domain>/v1.openapi.json`` — customer-facing,
    self-contained per-domain OpenAPI (public operations only, internal security schemes
    and unreferenced internal schemas removed, every reference resolved offline);
  * ``packages/contracts/bundled/public/aggregate/v1.openapi.json`` — the aggregate
    customer API reference (service-namespaced components, stable operation ids).

Usage:
  public_api.py            regenerate
  public_api.py --check    exit 1 on drift, leakage or an unclassified operation
"""
from __future__ import annotations

import copy
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bundle_openapi  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
CONTRACTS = ROOT / "packages" / "contracts"
ALLOW_LIST = ROOT / "infra" / "api-gateway" / "public-routes.json"
MANIFEST = ROOT / "infra" / "api-gateway" / "route-manifest.json"
PUBLIC_OUT = CONTRACTS / "bundled" / "public"
GATEWAY_URL = "https://api.cfmeet.com"
METHODS = ("get", "post", "put", "patch", "delete")

# (domain, owning service identity, contract path, contract id)
SOURCES = [
    ("meetings", "cfmeet.meeting-service", "openapi/meeting-service.json", "cfmeet.meeting-service.http-api"),
    ("calling", "cfmeet.calling", "calling/http/v1.openapi.json", "cfmeet.calling.http-api"),
    ("privacy-relay", "cfmeet.calling", "privacy-relay/http/v1.openapi.json", "cfmeet.privacy-relay.http-api"),
    ("messaging", "cfmeet.messaging", "messaging/http/v1.openapi.json", "cfmeet.messaging.http-api"),
    ("numbering", "cfmeet.numbering", "numbering/internal/v1.openapi.json", "cfmeet.numbering.internal-api"),
    ("sms", "cfmeet.sms-gateway", "sms-gateway/http/v1.openapi.json", "cfmeet.sms-gateway.http-api"),
    ("developer-platform", "cfmeet.developer-platform", "platform-developer/http/v1.openapi.json", "cfmeet.developer-platform.http-api"),
    ("gateway", "cfmeet.api-gateway", "platform-developer/gateway/http/v1.openapi.json", "cfmeet.api-gateway.http-api"),
]

# Services that may ever receive public customer traffic. Platform Access, Telephony,
# the operator console and Privacy Relay's own process (Contact Vault) never do.
CUSTOMER_UPSTREAMS = {
    "cfmeet.meeting-service", "cfmeet.calling", "cfmeet.messaging", "cfmeet.numbering",
    "cfmeet.sms-gateway", "cfmeet.developer-platform",
}

# Contract security scheme -> gateway authentication mode.
AUTH_MODES = {
    "platformCredential": "PLATFORM_CREDENTIAL",
    "customerCredential": "PLATFORM_CREDENTIAL",
    "clientToken": "CLIENT_TOKEN",
    "realtimeTicket": "REALTIME_TICKET",
    "signalingCredential": "SIGNALING_CREDENTIAL",
    "bearer_auth": "MEETING_SESSION",
    "participant_token": "PARTICIPANT_TOKEN",
    "userSession": "DEVELOPER_SESSION",
}
# Security schemes that never appear in customer documents.
INTERNAL_SCHEMES = {"cfServiceAuth", "serviceToken", "mediaProviderSignature", "providerSignature",
                    "contactDelegation", "mediaCredential", "api_key_auth"}
# Per-route request headers the gateway forwards (besides content negotiation).
EXTRA_FORWARDED = {
    "messaging": ["cf-acting-principal"],
    "meetings": ["x-client-kind"],
}


class CatalogueError(Exception):
    pass


def load(rel: str):
    return json.loads((CONTRACTS / rel).read_text())


def auto_class(domain: str, path: str, op: dict):
    p = path.lower()
    if "/provider-callbacks/" in p or p.startswith("/internal/provider") or "/provider/" in p:
        return "PROVIDER_CALLBACK"
    if p.startswith("/internal/") or p == "/internal":
        return "INTERNAL_SERVICE"
    segs = [s for s in p.split("/") if s]
    if "admin" in segs or "operator" in segs or any(t.lower() == "admin" or t.lower().startswith("admin ") for t in op.get("tags", [])):
        return "OPERATOR_ADMIN"
    if p in ("/health", "/ready", "/openapi.json", "/docs", "/discovery") or p.startswith("/openapi/") or p.startswith("/api-docs"):
        return "PUBLIC_INFRASTRUCTURE"
    return None


def operations():
    """(domain, service, contract path, contract id, method, path, op, doc) of every source op."""
    for domain, service, rel, cid in SOURCES:
        doc = load(rel)
        for path, item in doc["paths"].items():
            for method, op in item.items():
                if method in METHODS:
                    yield domain, service, rel, cid, method.upper(), path, op, doc


def security_names(op, doc):
    sec = op.get("security")
    if sec is None:
        sec = doc.get("security", [])
    names = []
    for alt in sec:
        names.extend(alt.keys())
    return names


def auth_modes(domain, op, doc):
    names = security_names(op, doc)
    if not names:
        return ["ANONYMOUS"]
    modes = []
    for n in names:
        if n in INTERNAL_SCHEMES:
            raise CatalogueError(f"{domain} {op.get('operationId')}: internal security scheme {n} on a public route")
        if n not in AUTH_MODES:
            raise CatalogueError(f"{domain} {op.get('operationId')}: unknown security scheme {n}")
        if AUTH_MODES[n] not in modes:
            modes.append(AUTH_MODES[n])
    return modes


def pattern_of(path: str) -> str:
    return re.sub(r"\{([^}]+)\}", lambda m: "{" + m.group(1) + "}", path)


def build():
    allow = json.loads(ALLOW_LIST.read_text())
    entries = {}
    for e in allow["operations"]:
        key = (e["domain"], e["method"], e["path"])
        if key in entries:
            raise CatalogueError(f"allow-list: duplicate entry {key}")
        if e["class"] not in ("PUBLIC_CUSTOMER", "EXCLUDED"):
            raise CatalogueError(f"allow-list: {key} class must be PUBLIC_CUSTOMER or EXCLUDED")
        if e["class"] == "EXCLUDED" and not re.fullmatch(r"[A-Z][A-Z0-9_]{2,63}", e.get("reason", "")):
            raise CatalogueError(f"allow-list: {key} EXCLUDED needs a reason code")
        entries[key] = e
    seen = set()
    catalogue = []
    routes = []
    keys = set()
    for domain, service, rel, cid, method, path, op, doc in operations():
        key = (domain, method, path)
        auto = auto_class(domain, path, op)
        entry = entries.get(key)
        seen.add(key)
        if entry and auto:
            raise CatalogueError(f"allow-list: {key} is {auto}; it can never be listed (public or excluded)")
        cls = auto or (entry and entry["class"])
        if not cls:
            raise CatalogueError(f"default deny: {domain} {method} {path} ({op.get('operationId')}) is not classified; add it to {ALLOW_LIST.relative_to(ROOT)}")
        row = {"domain": domain, "contract": cid, "operationId": op["operationId"], "method": method,
               "path": path, "class": cls}
        if cls == "EXCLUDED":
            row["reason"] = entry["reason"]
        catalogue.append(row)
        if cls != "PUBLIC_CUSTOMER":
            continue
        upstream = service
        if domain == "privacy-relay" and op.get("x-cfmeet-served-by"):
            upstream = op["x-cfmeet-served-by"]
        upstream = entry.get("upstream", upstream)
        if upstream not in CUSTOMER_UPSTREAMS:
            raise CatalogueError(f"{key}: upstream {upstream} is not a customer-facing service")
        rkey = (method, pattern_of(path))
        if rkey in keys:
            raise CatalogueError(f"{key}: route collision {rkey}")
        keys.add(rkey)
        route = {
            "id": f"{domain}.{op['operationId']}",
            "domain": domain,
            "method": method,
            "pattern": pattern_of(path),
            "protocol": entry.get("protocol", "REST"),
            "upstream": upstream,
            "upstreamPath": entry.get("upstreamPath", pattern_of(path)),
            "auth": entry.get("auth") or auth_modes(domain, op, doc),
            "forwardHeaders": EXTRA_FORWARDED.get(domain, []),
        }
        scope = op.get("x-cfmeet-required-scope")
        if scope:
            route["requiredScope"] = scope
        if "timeoutMs" in entry:
            route["timeoutMs"] = entry["timeoutMs"]
        if route["protocol"] == "WEBSOCKET" and method != "GET":
            raise CatalogueError(f"{key}: WebSocket routes are GET upgrades")
        routes.append(route)
    stale = set(entries) - seen
    if stale:
        raise CatalogueError(f"allow-list entries without a contract operation: {sorted(stale)[:5]}")
    # Leakage guard (belt and braces): nothing routable is internal / admin / provider.
    for r in routes:
        if r["pattern"].startswith("/internal") or "/admin" in r["pattern"] or "provider-callbacks" in r["pattern"]:
            raise CatalogueError(f"leak: {r['id']} {r['pattern']}")
    routes.sort(key=lambda r: (r["pattern"], r["method"]))
    catalogue.sort(key=lambda r: (r["domain"], r["path"], r["method"]))
    counts = {}
    for row in catalogue:
        counts[row["class"]] = counts.get(row["class"], 0) + 1
    manifest = {
        "manifestVersion": 1,
        "description": "Generated by scripts/contracts/public_api.py from the source contracts and infra/api-gateway/public-routes.json. Do not edit.",
        "classes": dict(sorted(counts.items())),
        "routes": routes,
        "catalogue": catalogue,
    }
    return manifest


# ---------------------------------------------------------------------------
# Public OpenAPI
# ---------------------------------------------------------------------------

def to_31(node):
    """OpenAPI 3.0 -> 3.1 schema dialect: `nullable: true` becomes a null type."""
    if isinstance(node, list):
        return [to_31(x) for x in node]
    if not isinstance(node, dict):
        return node
    out = {k: to_31(v) for k, v in node.items()}
    if out.pop("nullable", False) is True:
        t = out.get("type")
        if isinstance(t, str):
            out["type"] = [t, "null"]
        elif isinstance(t, list) and "null" not in t:
            out["type"] = t + ["null"]
        elif "$ref" in out or "allOf" in out or "oneOf" in out or "anyOf" in out:
            out = {"oneOf": [{k: v for k, v in out.items()}, {"type": "null"}]}
    return out


def refs(node, acc):
    if isinstance(node, list):
        for x in node:
            refs(x, acc)
    elif isinstance(node, dict):
        r = node.get("$ref")
        if isinstance(r, str) and r.startswith("#/components/"):
            acc.add(r)
        for k, v in node.items():
            if k != "$ref":
                refs(v, acc)


def prune_components(doc):
    """Keeps only components reachable from the paths (internal schemas are dropped)."""
    comps = doc.get("components", {})
    reachable = set()
    frontier = set()
    refs(doc.get("paths", {}), frontier)
    while frontier:
        r = frontier.pop()
        if r in reachable:
            continue
        reachable.add(r)
        _, _, kind, name = r.split("/", 3)
        target = comps.get(kind, {}).get(bundle_openapi.unescape(name))
        if target is None:
            raise CatalogueError(f"unresolved {r}")
        refs(target, frontier)
    for kind in list(comps):
        if kind == "securitySchemes":
            continue
        keep = {n: v for n, v in comps[kind].items() if f"#/components/{kind}/{n}" in reachable}
        if keep:
            comps[kind] = dict(sorted(keep.items()))
        else:
            del comps[kind]


def public_doc(domain, manifest):
    rel = next(r for d, _, r, _ in SOURCES if d == domain)
    src = load(rel)
    bundled = bundle_openapi.Bundler().bundle_doc(copy.deepcopy(src), rel)
    routes = {(r["method"].lower(), r["pattern"]): r for r in manifest["routes"] if r["domain"] == domain}
    paths = {}
    used_schemes = set()
    for path, item in bundled["paths"].items():
        kept = {}
        for method, op in item.items():
            if method not in METHODS:
                if method == "parameters":
                    kept["parameters"] = op
                continue
            route = routes.get((method, pattern_of(path)))
            if not route:
                continue
            op = copy.deepcopy(op)
            op["x-cfmeet-service"] = domain
            op["x-cfmeet-route-id"] = route["id"]
            op["x-cfmeet-auth"] = route["auth"]
            if route["protocol"] == "WEBSOCKET":
                op["x-cfmeet-protocol"] = "websocket"
            op.pop("x-cfmeet-served-by", None)
            for alt in op.get("security", []) or []:
                used_schemes.update(alt.keys())
            kept[method] = op
        if any(m in kept for m in METHODS):
            paths[path] = kept
    if "security" in bundled and not any("security" in o for p in paths.values() for o in p.values() if isinstance(o, dict)):
        for alt in bundled["security"]:
            used_schemes.update(alt.keys())
    doc = {
        "openapi": "3.1.0",
        "info": dict(bundled["info"]),
        "servers": [{"url": GATEWAY_URL, "description": "CF Meet public API Gateway"}],
        "paths": paths,
        "components": copy.deepcopy(bundled.get("components", {})),
    }
    doc["info"]["title"] = f"CF Meet public API — {domain}"
    doc["info"]["x-cfmeet-source-contract"] = next(c for d, _, _, c in SOURCES if d == domain)
    doc["info"]["x-cfmeet-source-version"] = bundled["info"]["version"]
    schemes = doc["components"].get("securitySchemes", {})
    doc["components"]["securitySchemes"] = {k: v for k, v in sorted(schemes.items()) if k in used_schemes and k not in INTERNAL_SCHEMES}
    if not doc["components"]["securitySchemes"]:
        del doc["components"]["securitySchemes"]
    if bundled["openapi"].startswith("3.0"):
        doc["paths"] = to_31(doc["paths"])
        doc["components"] = to_31(doc["components"])
    prune_components(doc)
    return doc


def aggregate(docs):
    agg = {
        "openapi": "3.1.0",
        "info": {"title": "CF Meet public API", "version": "1.0.0",
                 "description": "Aggregate customer-facing API reference of the CF Meet public API Gateway, generated from the authoritative service contracts and the explicit public route allow-list. Each service keeps its own API version (x-cfmeet-source-version); there is no monolithic version. Internal, operator and provider-callback operations are never included."},
        "servers": [{"url": GATEWAY_URL, "description": "CF Meet public API Gateway"}],
        "tags": [],
        "paths": {},
        "components": {},
    }
    op_ids = set()
    for domain, doc in docs.items():
        prefix = domain.replace("-", "_")
        def rename(node):
            if isinstance(node, list):
                return [rename(x) for x in node]
            if not isinstance(node, dict):
                return node
            out = {}
            for k, v in node.items():
                if k == "$ref" and isinstance(v, str) and v.startswith("#/components/"):
                    _, _, kind, name = v.split("/", 3)
                    out[k] = f"#/components/{kind}/{prefix}.{name}"
                else:
                    out[k] = rename(v)
            return out
        agg["tags"].append({"name": domain, "description": f"{doc['info']['x-cfmeet-source-contract']} {doc['info']['x-cfmeet-source-version']}"})
        for path, item in doc["paths"].items():
            if path in agg["paths"]:
                raise CatalogueError(f"aggregate path collision {path}")
            item = rename(item)
            for m, op in item.items():
                if m not in METHODS:
                    continue
                op["tags"] = [domain]
                if op["operationId"] in op_ids:
                    op["operationId"] = f"{prefix}_{op['operationId']}"
                op_ids.add(op["operationId"])
                if "security" in op:
                    op["security"] = [{f"{prefix}.{k}": v for k, v in alt.items()} for alt in op["security"]]
            agg["paths"][path] = item
        for kind, comps in doc.get("components", {}).items():
            target = agg["components"].setdefault(kind, {})
            for name, value in comps.items():
                target[f"{prefix}.{name}"] = rename(value)
    agg["paths"] = dict(sorted(agg["paths"].items()))
    for kind in agg["components"]:
        agg["components"][kind] = dict(sorted(agg["components"][kind].items()))
    return agg


def render(doc) -> str:
    return json.dumps(doc, indent=2, ensure_ascii=False) + "\n"


PUBLIC_DOMAINS = ["meetings", "calling", "privacy-relay", "messaging", "numbering", "sms", "developer-platform", "gateway"]


def outputs():
    manifest = build()
    out = {MANIFEST: render(manifest)}
    docs = {}
    for domain in PUBLIC_DOMAINS:
        docs[domain] = public_doc(domain, manifest)
        out[PUBLIC_OUT / domain / "v1.openapi.json"] = render(docs[domain])
    agg = aggregate(docs)
    out[PUBLIC_OUT / "aggregate" / "v1.openapi.json"] = render(agg)
    # Leakage guard on the documents themselves.
    for path, text in out.items():
        if path == MANIFEST:
            continue
        doc = json.loads(text)
        for p in doc["paths"]:
            if p.startswith("/internal") or "/admin" in p or "provider-callbacks" in p:
                raise CatalogueError(f"{path}: leaked {p}")
        schemes = set(doc.get("components", {}).get("securitySchemes", {}))
        if any(s.split(".")[-1] in INTERNAL_SCHEMES for s in schemes):
            raise CatalogueError(f"{path}: internal security scheme leaked")
        bundle_openapi.Bundler.verify(doc, str(path))
    return out


def main(argv):
    check = "--check" in argv
    try:
        out = outputs()
    except (CatalogueError, bundle_openapi.BundleError) as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 1
    stale = []
    for path, text in out.items():
        if check:
            if not path.is_file() or path.read_text() != text:
                stale.append(str(path.relative_to(ROOT)))
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
    if stale:
        print("stale public API catalogue outputs (run scripts/contracts/public_api.py):", file=sys.stderr)
        for s in stale:
            print(f"  {s}", file=sys.stderr)
        return 1
    m = json.loads(out[MANIFEST])
    print(f"public API catalogue {'current' if check else 'written'}: {len(m['routes'])} public routes; classes {m['classes']}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
