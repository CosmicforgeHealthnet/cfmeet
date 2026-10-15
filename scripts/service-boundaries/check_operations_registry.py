#!/usr/bin/env python3
"""Operations registry check (Part I section 82).

Every required durable responsibility (call timeout, relay expiry, Telephony command
worker, reconciler, usage finalizer, artifact worker, webhook delivery) must be listed in
operations_registry.json with exactly ONE owning service, an entry point that exists as a
function in that service's source file, and tests that exist in that service's tests. A
renamed / deleted worker or test, an unknown owner, or a missing required responsibility
fails. Standard library only. Exit status: 0 clean, 1 violations.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def check(root: Path, registry_path: Path) -> list[str]:
    registry = json.loads(registry_path.read_text(encoding="utf-8"))
    errors: list[str] = []
    services = {p.name for p in (root / "services").iterdir() if p.is_dir()}
    seen: dict[str, str] = {}
    for item in registry["responsibilities"]:
        rid, owner = item["id"], item["owner"]
        if rid in seen:
            errors.append(f"{rid}: listed twice (owners {seen[rid]} and {owner}); one owner per responsibility")
            continue
        seen[rid] = owner
        if owner not in services:
            errors.append(f"{rid}: owner services/{owner} does not exist")
            continue
        base = root / "services" / owner
        source = base / item["source"]
        if not source.is_file():
            errors.append(f"{rid}: {source.relative_to(root)} not found")
        elif not re.search(r"\bfn\s+" + re.escape(item["entry"]) + r"\s*[(<]", source.read_text(encoding="utf-8")):
            errors.append(f"{rid}: entry point `{item['entry']}` not defined in {source.relative_to(root)}")
        if not item.get("tests"):
            errors.append(f"{rid}: no proving test listed")
        for test in item.get("tests", []):
            file, _, name = test.partition("::")
            path = base / file
            if not path.is_file() or not re.search(r"\bfn\s+" + re.escape(name) + r"\s*\(", path.read_text(encoding="utf-8")):
                errors.append(f"{rid}: test {owner}/{test} not found")
    for required in registry["required"]:
        if required not in seen:
            errors.append(f"required responsibility `{required}` has no owner in the registry")
    return errors


def main() -> int:
    root = HERE.parents[1]
    errors = check(root, HERE / "operations_registry.json")
    for e in errors:
        print(f"operations registry: {e}")
    if errors:
        print(f"operations registry: FAILED — {len(errors)} problem(s)", file=sys.stderr)
        return 1
    registry = json.loads((HERE / "operations_registry.json").read_text(encoding="utf-8"))
    owners: dict[str, int] = {}
    for item in registry["responsibilities"]:
        owners[item["owner"]] = owners.get(item["owner"], 0) + 1
    print(f"operations registry: OK — {len(registry['responsibilities'])} responsibilities "
          f"({', '.join(f'{k}: {v}' for k, v in sorted(owners.items()))}); all required ones owned")
    return 0


if __name__ == "__main__":
    sys.exit(main())
