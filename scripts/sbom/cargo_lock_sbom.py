#!/usr/bin/env python3
"""CycloneDX 1.5 SBOM (JSON) for the Rust services, built from their Cargo.lock files.

Part V K §158: a software bill of materials for every service lockfile. No network and
no third-party tool: each locked package becomes one component with its exact version,
its source (crates.io registry, git or path) and, for registry crates, the SHA-256
checksum Cargo pinned. Workspace-local path crates are listed as `application`.

Usage: cargo_lock_sbom.py OUTPUT.json LOCKFILE [LOCKFILE ...]
Exit 1 on a lockfile that cannot be parsed or a registry crate without a checksum.
"""
import json
import sys
import tomllib
import uuid
from datetime import datetime, timezone
from pathlib import Path


def components(lock: Path):
    data = tomllib.loads(lock.read_text())
    for pkg in data.get("package", []):
        name, version = pkg["name"], pkg["version"]
        source = pkg.get("source", "")
        comp = {
            "type": "library",
            "name": name,
            "version": version,
            "purl": f"pkg:cargo/{name}@{version}",
            "bom-ref": f"pkg:cargo/{name}@{version}",
            "properties": [{"name": "cfmeet:lockfile", "value": str(lock)}],
        }
        if source.startswith("registry+"):
            if "checksum" not in pkg:
                raise SystemExit(f"{lock}: registry crate {name} {version} has no checksum")
            comp["hashes"] = [{"alg": "SHA-256", "content": pkg["checksum"]}]
        elif source.startswith("git+"):
            comp["externalReferences"] = [{"type": "vcs", "url": source[4:]}]
        else:
            comp["type"] = "application"
        yield comp


def main(argv):
    if len(argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    out, locks = Path(argv[1]), [Path(p) for p in argv[2:]]
    seen = {}
    for lock in locks:
        for c in components(lock):
            key = c["bom-ref"]
            if key in seen:
                seen[key]["properties"].extend(c["properties"])
            else:
                seen[key] = c
    bom = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": f"urn:uuid:{uuid.uuid4()}",
        "version": 1,
        "metadata": {
            "timestamp": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "component": {"type": "application", "name": "cfmeet-backend", "bom-ref": "cfmeet-backend"},
        },
        "components": sorted(seen.values(), key=lambda c: c["bom-ref"]),
    }
    out.write_text(json.dumps(bom, indent=1) + "\n")
    print(f"SBOM: {len(bom['components'])} components from {len(locks)} lockfile(s) -> {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
