#!/usr/bin/env python3
"""Self-contained (bundled) OpenAPI documents for Swagger UI and other offline consumers.

The authoritative contracts in packages/contracts reference shared JSON Schemas by their
canonical `$id` URLs (https://schemas.cfmeet.com/...). That host is a naming authority,
not a deployed service, so a browser rendering a contract directly fails with
"Could not resolve reference: Failed to fetch".

This tool resolves every such reference against the checked-in schema files and copies
each referenced schema into the document's own `components/schemas`, under a
deterministic key derived from its URL (`platform.common.v1.Error`). References inside the
copied schemas are rewritten the same way, recursively. The source contracts are not
modified. The output is deterministic, so CI can check that it is up to date.

Usage:
  bundle_openapi.py            regenerate packages/contracts/bundled/**
  bundle_openapi.py --check    exit 1 if a bundle is missing, stale or not self-contained
"""
import copy
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CONTRACTS = ROOT / "packages" / "contracts"
OUT = CONTRACTS / "bundled"
BASE = "https://schemas.cfmeet.com/"
# Every OpenAPI document that is (or may be) rendered by a Swagger UI.
SOURCES = [
    "calling/http/v1.openapi.json",
    "messaging/http/v1.openapi.json",
    "messaging/internal/v1.openapi.json",
    "numbering/internal/v1.openapi.json",
    "platform-access/internal/v1.openapi.json",
    "platform/internal-http/v1.openapi.json",
    "privacy-relay/http/v1.openapi.json",
    "privacy-relay/internal/v1.openapi.json",
    "sms-gateway/http/v1.openapi.json",
    "sms-gateway/internal/v1.openapi.json",
    "telephony/internal/v1.openapi.json",
]


class BundleError(Exception):
    pass


def unescape(token: str) -> str:
    return token.replace("~1", "/").replace("~0", "~")


def resolve_pointer(doc, pointer: str, where: str):
    if pointer in ("", "/"):
        return doc
    if not pointer.startswith("/"):
        raise BundleError(f"{where}: unsupported JSON pointer {pointer!r}")
    node = doc
    for raw in pointer[1:].split("/"):
        token = unescape(raw)
        if isinstance(node, dict) and token in node:
            node = node[token]
        elif isinstance(node, list) and token.isdigit() and int(token) < len(node):
            node = node[int(token)]
        else:
            raise BundleError(f"{where}: {pointer!r} does not resolve")
    return node


def component_key(path: str, pointer: str) -> str:
    stem = path[: -len(".schema.json")] if path.endswith(".schema.json") else path
    parts = [p for p in stem.split("/") if p]
    if pointer:
        segs = [unescape(s) for s in pointer.strip("/").split("/")]
        # "$defs" / "definitions" / "properties" containers carry no meaning in a name.
        parts += [s for s in segs if s not in ("$defs", "definitions")]
    key = ".".join(parts)
    return "".join(c if c.isalnum() or c in "._-" else "_" for c in key)


class Bundler:
    def __init__(self):
        self.files = {}

    def schema_file(self, path: str):
        if path not in self.files:
            f = CONTRACTS / path
            if not f.is_file():
                raise BundleError(f"no checked-in schema for {BASE}{path}")
            self.files[path] = json.loads(f.read_text())
        return self.files[path]

    def bundle(self, source: str):
        return self.bundle_doc(json.loads((CONTRACTS / source).read_text()), source)

    def bundle_doc(self, doc, source: str):
        """Bundles an in-memory OpenAPI document (`source` names it in errors)."""
        self.components = doc.setdefault("components", {}).setdefault("schemas", {})
        self.added = {}
        self.pending = []
        self.rewrite(doc, None, source)
        while self.pending:
            key, path, pointer = self.pending.pop()
            target = copy.deepcopy(
                resolve_pointer(self.schema_file(path), pointer, f"{BASE}{path}")
            )
            if isinstance(target, dict):
                for meta in ("$id", "$schema"):
                    target.pop(meta, None)
                if pointer == "":
                    # A whole-document reference: its $defs are hoisted individually.
                    target.pop("$defs", None)
                    target.pop("definitions", None)
            self.rewrite(target, path, source)
            self.components[key] = target
        doc["components"]["schemas"] = dict(sorted(doc["components"]["schemas"].items()))
        self.verify(doc, source)
        return doc

    def external(self, path: str, pointer: str) -> str:
        key = component_key(path, pointer)
        origin = (path, pointer)
        if key in self.added and self.added[key] != origin:
            raise BundleError(f"component key collision {key}: {self.added[key]} vs {origin}")
        if key not in self.added:
            if key in self.components:
                raise BundleError(f"{key} already exists in the source components")
            self.added[key] = origin
            self.pending.append((key, path, pointer))
        return f"#/components/schemas/{key}"

    def rewrite(self, node, base_path, source):
        if isinstance(node, list):
            for item in node:
                self.rewrite(item, base_path, source)
            return
        if not isinstance(node, dict):
            return
        ref = node.get("$ref")
        if isinstance(ref, str):
            if ref.startswith(BASE):
                path, _, pointer = ref[len(BASE):].partition("#")
                node["$ref"] = self.external(path, pointer)
            elif ref.startswith("#"):
                if base_path is not None:
                    # Local to the schema file it was copied from.
                    node["$ref"] = self.external(base_path, ref[1:])
            else:
                raise BundleError(f"{source}: unsupported $ref {ref!r}")
        for key, value in node.items():
            if key != "$ref":
                self.rewrite(value, base_path, source)

    @staticmethod
    def verify(doc, source):
        def walk(node):
            if isinstance(node, list):
                for item in node:
                    walk(item)
            elif isinstance(node, dict):
                ref = node.get("$ref")
                if isinstance(ref, str):
                    if not ref.startswith("#"):
                        raise BundleError(f"{source}: external reference left: {ref}")
                    resolve_pointer(doc, ref[1:], f"{source} {ref}")
                for k, v in node.items():
                    if k != "$ref":
                        walk(v)

        walk(doc)


def render(doc) -> str:
    return json.dumps(doc, indent=2, ensure_ascii=False) + "\n"


def main(argv):
    check = "--check" in argv
    bundler = Bundler()
    stale = []
    for source in SOURCES:
        try:
            text = render(bundler.bundle(source))
        except BundleError as e:
            print(f"ERROR: {e}", file=sys.stderr)
            return 1
        out = OUT / source
        if check:
            if not out.is_file() or out.read_text() != text:
                stale.append(str(out.relative_to(ROOT)))
        else:
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_text(text)
            print(f"bundled {source}")
    if stale:
        print("stale or missing bundles (run scripts/contracts/bundle_openapi.py):", file=sys.stderr)
        for s in stale:
            print(f"  {s}", file=sys.stderr)
        return 1
    if check:
        print(f"bundles current and self-contained: {len(SOURCES)} documents")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
