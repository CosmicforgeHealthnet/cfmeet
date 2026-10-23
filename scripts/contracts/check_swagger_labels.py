#!/usr/bin/env python3
"""Validate that source and Swagger-bundled OpenAPI operations stay fully labeled.

This is a documentation-only quality gate. It never changes API routing,
authentication, schemas, response bodies, or any production runtime setting.
"""
from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CONTRACTS = ROOT / "packages" / "contracts"
SOURCES = (
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
)
VERBS = {"get", "post", "put", "patch", "delete", "head", "options", "trace"}


def operations(doc: dict) -> dict[tuple[str, str], dict]:
    return {
        (path, method): data
        for path, item in doc.get("paths", {}).items()
        for method, data in item.items()
        if method in VERBS
    }


def main() -> None:
    total = 0
    failures: list[str] = []
    for relative in SOURCES:
        source = json.loads((CONTRACTS / relative).read_text(encoding="utf-8"))
        bundled = json.loads((CONTRACTS / "bundled" / relative).read_text(encoding="utf-8"))
        before, after = operations(source), operations(bundled)
        if before.keys() != after.keys():
            failures.append(f"{relative}: source/bundled operation sets differ")
            continue
        # Components-only platform contract intentionally has no paths.
        if not before and relative == "platform/internal-http/v1.openapi.json":
            print(f"{relative}: components-only (0 operations)")
            continue
        if not before:
            failures.append(f"{relative}: unexpectedly no operations")
            continue
        root_tags = source.get("tags", [])
        bundle_tags = bundled.get("tags", [])
        if not root_tags or root_tags != bundle_tags:
            failures.append(f"{relative}: missing/mismatched top-level tag definitions")
        known_names = {t.get("name") for t in root_tags}
        for item in root_tags:
            if not item.get("name") or not item.get("description"):
                failures.append(f"{relative}: tag definition without name/description")
        for (path, method), operation in before.items():
            location = f"{relative}:{method.upper()} {path}"
            labels = operation.get("tags", [])
            if not isinstance(labels, list) or len(labels) != 1 or labels[0] in ("", "default"):
                failures.append(f"{location}: must have exactly one meaningful tag")
            elif labels[0] not in known_names:
                failures.append(f"{location}: tag {labels[0]} undefined at top level")
            if not operation.get("operationId"):
                failures.append(f"{location}: missing operationId")
            if not operation.get("summary") or not operation.get("summary").strip():
                failures.append(f"{location}: missing operation summary")
            if not operation.get("description") or not operation.get("description").strip():
                failures.append(f"{location}: missing operation description")
            rendered = after[(path, method)]
            for field in ("tags", "summary", "description", "operationId", "security"):
                if operation.get(field) != rendered.get(field):
                    failures.append(f"{location}: source/bundle mismatch in {field}")
        total += len(before)
        print(f"{relative}: {len(before)} labeled operations / {len(known_names)} sections")
    if failures:
        for failure in failures:
            print(f"ERROR: {failure}")
        raise SystemExit(f"Swagger metadata validation failed: {len(failures)} issue(s)")
    print(f"PASS: {total} service API operations have complete, consistent Swagger metadata.")


if __name__ == "__main__":
    main()
