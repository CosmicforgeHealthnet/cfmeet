#!/usr/bin/env python3
"""Ensure CF Meet Swagger contracts never regress into the unhelpful default tag.

Validate the canonical contracts and their deterministic bundled copies.
This is read-only: it cannot modify routes, authentication or OpenAPI files.
The generated Meeting Service document is intentionally excluded because its
operation tags come from utoipa and are already maintained at handler level.
"""
from __future__ import annotations

import json
from pathlib import Path

from bundle_openapi import CONTRACTS, OUT, SOURCES

HTTP_VERBS = {"get", "post", "put", "patch", "delete", "head", "options", "trace"}


def inspect(source: Path) -> tuple[dict, dict[tuple[str, str], dict]]:
    doc = json.loads(source.read_text(encoding="utf-8"))
    paths = doc.get("paths")
    if source.parts[-3:] == ("platform", "internal-http", "v1.openapi.json"):
        # Schema-only conventions document: legitimately has no HTTP operations.
        if paths is not None and paths != {}:
            raise AssertionError(f"{source}: expected no operations")
        return doc, {}

    if not isinstance(paths, dict) or not paths:
        raise AssertionError(f"{source}: expected nonempty OpenAPI paths")

    definitions = doc.get("tags")
    if not isinstance(definitions, list) or not definitions:
        raise AssertionError(f"{source}: missing tag descriptions")
    defined = {}
    for item in definitions:
        if not isinstance(item, dict) or not item.get("name") or not item.get("description"):
            raise AssertionError(f"{source}: every tag needs a name and description")
        name = item["name"]
        if name in defined:
            raise AssertionError(f"{source}: duplicate tag {name}")
        if name.strip().lower() == "default":
            raise AssertionError(f"{source}: default is not a meaningful tag")
        defined[name] = item["description"]

    records = {}
    for path, item in paths.items():
        for verb, operation in item.items():
            if verb not in HTTP_VERBS:
                continue
            label = f"{verb.upper()} {path}"
            if not isinstance(operation, dict):
                raise AssertionError(f"{source}: malformed operation {label}")
            tags = operation.get("tags")
            if not isinstance(tags, list) or len(tags) != 1 or tags[0] not in defined:
                raise AssertionError(f"{source}: missing/undefined operation tag {label}")
            for field in ("operationId", "summary", "description"):
                value = operation.get(field)
                if not isinstance(value, str) or not value.strip():
                    raise AssertionError(f"{source}: missing {field} for {label}")
            if len(operation["summary"]) > 120:
                raise AssertionError(f"{source}: overly long summary for {label}")
            records[(path, verb)] = {
                key: operation.get(key)
                for key in ("operationId", "tags", "summary", "description", "security")
            }
    if not records:
        raise AssertionError(f"{source}: no documented HTTP operations")
    return doc, records


def main() -> None:
    total = 0
    for relative in SOURCES:
        source = CONTRACTS / relative
        bundled = OUT / relative
        source_doc, source_ops = inspect(source)
        bundled_doc, bundled_ops = inspect(bundled)
        if source_doc.get("tags") != bundled_doc.get("tags"):
            raise AssertionError(f"{relative}: tag definitions differ between source/bundle")
        if source_ops != bundled_ops:
            raise AssertionError(f"{relative}: operation metadata differs between source/bundle")
        print(f"Swagger labels PASS: {relative}: {len(source_ops)} operations")
        total += len(source_ops)

    if total < 400:
        raise AssertionError(f"unexpectedly small operation inventory: {total}")
    print(f"All {total} peer Swagger operations have explicit tags, summaries and descriptions.")


if __name__ == "__main__":
    main()
