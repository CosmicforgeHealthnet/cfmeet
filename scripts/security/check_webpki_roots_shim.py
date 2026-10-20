#!/usr/bin/env python3
"""Guard for the RUSTSEC-2026-0381 exception (webpki-roots, distrusted emSign intermediates).

    check_webpki_roots_shim.py <Cargo.lock>...

The advisory is fixed in webpki-roots 1.0.10. sqlx 0.8 (via sea-orm 1.1) still depends on
webpki-roots 0.26, whose last release (0.26.11) is a "semver trick" shim: it contains no
certificate data and only re-exports `TLS_SERVER_ROOTS` from its dependency webpki-roots 1.
With that dependency at >= 1.0.10 the root store compiled into the binary is the fixed one,
but cargo-audit still matches the shim's own version number.

The exception holds only while, in every lockfile given:
  * every webpki-roots 1.x is >= 1.0.10, and
  * every webpki-roots 0.26.x is exactly the reviewed shim (0.26.11, crates.io checksum
    below) and depends on a webpki-roots 1.x >= 1.0.10.
Anything else (another 0.x version, a different checksum, a vulnerable parent) fails, and the
advisory must then be fixed rather than ignored. Exit 0 when the exception holds or the
crate is absent.
"""
import re
import sys

SHIM_VERSION = "0.26.11"
SHIM_CHECKSUM = "521bc38abb08001b01866da9f51eb7c5d647a19260e00054a8c7fd5f9e57f7a9"
FIXED = (1, 0, 10)


def version(v):
    return tuple(int(x) for x in re.match(r"(\d+)\.(\d+)\.(\d+)", v).groups())


def packages(text):
    for block in text.split("[[package]]")[1:]:
        name = re.search(r'^name = "([^"]+)"', block, re.M)
        ver = re.search(r'^version = "([^"]+)"', block, re.M)
        checksum = re.search(r'^checksum = "([^"]+)"', block, re.M)
        deps = re.search(r"^dependencies = \[(.*?)\]", block, re.M | re.S)
        yield {
            "name": name.group(1) if name else "",
            "version": ver.group(1) if ver else "",
            "checksum": checksum.group(1) if checksum else None,
            "deps": re.findall(r'"([^"]+)"', deps.group(1)) if deps else [],
        }


def check(text):
    problems = []
    pkgs = [p for p in packages(text) if p["name"] == "webpki-roots"]
    for p in pkgs:
        v = version(p["version"])
        if v[0] >= 1:
            if v < FIXED:
                problems.append(f"webpki-roots {p['version']} is vulnerable (needs >= 1.0.10)")
            continue
        if p["version"] != SHIM_VERSION or p["checksum"] != SHIM_CHECKSUM:
            problems.append(f"webpki-roots {p['version']} is not the reviewed re-export shim {SHIM_VERSION}")
            continue
        parents = [d.split(" ", 1)[1] for d in p["deps"] if d.startswith("webpki-roots ")]
        if not parents or any(version(x) < FIXED for x in parents):
            problems.append(f"the webpki-roots {SHIM_VERSION} shim does not resolve to webpki-roots >= 1.0.10 ({parents})")
    return problems


def main(paths):
    if not paths:
        print(__doc__, file=sys.stderr)
        return 2
    failed = False
    for path in paths:
        for problem in check(open(path, encoding="utf-8").read()):
            print(f"{path}: {problem}", file=sys.stderr)
            failed = True
    if not failed:
        print("webpki-roots: every 0.26 copy is the reviewed shim over a patched webpki-roots 1")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
