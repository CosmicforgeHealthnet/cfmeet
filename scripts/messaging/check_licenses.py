#!/usr/bin/env python3
"""Dependency licence review for the Messaging crypto core and service (Part V §144, WS A).

Reads the licence of every package in Cargo's resolved graph (normal edges — what ships —
every target platform, the requested features) and fails unless each one is permissive,
or MPL-2.0 on an explicitly listed crate whose obligations are pending legal review
(ADR 0045: hpke-rs). Copyleft (GPL / AGPL / LGPL), unknown and missing licences fail.

    check_licenses.py <manifest dir> [cargo tree args...]
"""
import re
import subprocess
import sys

PERMISSIVE = {
    "MIT", "MIT-0", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause",
    "BSD-3-Clause", "ISC", "Zlib", "0BSD", "CC0-1.0", "Unlicense", "BSL-1.0",
    "Unicode-3.0", "Unicode-DFS-2016", "CDLA-Permissive-2.0",
}
# File-level copyleft, unmodified upstream use; legal confirmation pending (ADR 0045).
MPL_REVIEW = {"hpke-rs", "hpke-rs-crypto", "hpke-rs-rust-crypto"}


def allowed(expr: str, crate: str) -> bool:
    expr = expr.replace("/", " OR ").strip()
    ok_ids = PERMISSIVE | ({"MPL-2.0"} if crate in MPL_REVIEW else set())

    def term(t: str) -> bool:
        t = t.strip().strip("()").strip()
        if " AND " in t:
            return all(term(x) for x in t.split(" AND "))
        return t in ok_ids

    return any(term(alt) for alt in re.split(r"\s+OR\s+", expr))


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    manifest_dir, extra = sys.argv[1], sys.argv[2:]
    proc = subprocess.run(
        ["cargo", "tree", "--locked", "-e", "normal", "--target", "all", "--prefix", "none",
         "-f", "{p}|{l}", *extra],
        cwd=manifest_dir, capture_output=True, text=True,
    )
    if proc.returncode != 0:
        print(proc.stderr, file=sys.stderr)
        print(f"::error::cargo tree failed in {manifest_dir}", file=sys.stderr)
        return 2
    seen, bad, mpl = set(), [], []
    for line in proc.stdout.splitlines():
        line = line.removesuffix(" (*)").strip()
        if not line or "|" not in line:
            continue
        pkg, lic = line.rsplit("|", 1)
        if pkg in seen:
            continue
        seen.add(pkg)
        name = pkg.split(" ")[0]
        if "(/" in pkg:
            continue  # this repository's own path crates
        if not lic.strip():
            bad.append(f"{pkg}: no licence declared")
        elif not allowed(lic, name):
            bad.append(f"{pkg}: {lic}")
        elif "MPL-2.0" in lic and not any(allowed(a, "") for a in re.split(r"\s+OR\s+", lic)):
            mpl.append(f"{pkg}: {lic}")
    if len(seen) < 10:
        print(f"::error::implausibly small dependency graph in {manifest_dir}", file=sys.stderr)
        return 2
    for m in mpl:
        print(f"MPL-2.0 pending legal review (ADR 0045): {m}")
    if bad:
        for b in bad:
            print(f"::error::licence not allowed: {b}", file=sys.stderr)
        return 1
    print(f"OK: {len(seen)} packages in {manifest_dir} {' '.join(extra)} have allowed licences")
    return 0


if __name__ == "__main__":
    sys.exit(main())
