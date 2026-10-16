#!/usr/bin/env python3
"""CF Meet repository / built-image secret scan (Part H §74 secret protection).

Finds real secret material that must never be committed or shipped:

  PRIVATE_KEY_BLOCK       a PEM private-key block with key material (a bare header
                          string inside an assertion is not a key)
  KEYRING_DOCUMENT        a Contact Vault keyring document ("keyring_version" plus
                          "purpose" and base64 key material)
  API_CREDENTIAL          a well-formed CF Meet API credential
                          (cfmk_<tag>_<24 hex>_<64 hex>)
  WEBHOOK_SECRET          a customer webhook signing secret (whsec_ followed by key
                          material)
  CLOUD_ACCESS_KEY        an AWS-style access key id
  CREDENTIAL_ASSIGNMENT   password / secret / token / api key assigned a literal of 16+
                          credential-shaped characters in configuration or source

Scope: by default every file tracked by git in the repository (``git ls-files``);
``--tree DIR`` scans a directory tree instead (an exported container root filesystem,
a build context) with pseudo-filesystems excluded. Binary files are skipped for the
textual rules (the PEM, keyring and credential rules still run on them, byte-wise).
In a tree that is a Debian root filesystem, a file is skipped only when it is unmodified
distribution content: it is listed in a package's ``var/lib/dpkg/info/*.md5sums`` and its
MD5 matches that manifest (e.g. GnuTLS ships well-known FIPS self-test keys). Anything
added to the image, or a package file that was altered, is scanned.

Known non-secrets are allow-listed explicitly, never by excluding whole directories:
``*.example*`` templates, loopback-only certification placeholders under
``infra/telephony/certification`` and ``infra/livekit/*cert*`` / ``*dev*``, test fixtures
whose names say so, and documentation placeholders (``<...>``, ``…``, ``changeme``).
Add an allow-list entry only with a comment explaining why the value is not a secret.

Standard library only. Exit status: 0 clean, 1 findings, 2 usage error.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

PRUNE_DIRS = {".git", "target", "node_modules", ".next", "__pycache__", "proc", "sys", "dev", "run"}
MAX_FILE_BYTES = 8 * 1024 * 1024

# PEM private key: header followed by base64 key material (raw newline or an escaped
# "\n" inside a string literal). 16+ base64 characters rule out assertions on the header.
PEM_RE = re.compile(rb"-----BEGIN [A-Z ]*PRIVATE KEY-----(?:\\n|\s)+[A-Za-z0-9+/]{16,}")
# A keyring document carries its version, purpose and at least one base64 key (32 bytes
# → 44 base64 characters).
KEYRING_RE = re.compile(
    rb"\"keyring_version\"\s*:\s*1[\s\S]{0,400}?\"key\"\s*:\s*\"[A-Za-z0-9+/]{43}=\""
)
API_CREDENTIAL_RE = re.compile(rb"\bcfmk_(?:sbx|test|live)_[0-9a-f]{24}_[0-9a-f]{64}\b")
WEBHOOK_SECRET_RE = re.compile(rb"\bwhsec_[A-Za-z0-9_-]{24,}\b")
CLOUD_KEY_RE = re.compile(rb"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b")
# "password = <literal>" shapes in configuration and source (YAML, TOML, conf, env, Rust).
ASSIGNMENT_RE = re.compile(
    rb"(?i)(password|passwd|secret|token|api[_-]?key|private[_-]?key|signing[_-]?key)"
    rb"[A-Za-z0-9_]*\s*[=:]\s*[\"']?([A-Za-z0-9/+_.=-]{16,})[\"']?"
)
# Assignment look-alikes that are never a secret value.
ASSIGNMENT_SAFE_RE = re.compile(
    rb"(?i)([A-Z_]+_FILE\s*[=:]|secret_ref|credential_ref|SECRET_REF|env\(|from_env|lookup\(|Zeroizing|"
    rb"std::env|\$\{|\$[A-Z_]+|<[a-z][a-z0-9-]*>|\.\.\.|\xe2\x80\xa6|changeme|example|placeholder|"
    rb"REDACTED|xxxx|/run/secrets|secret_key_version|key_version|SigningKey|verifier)"
)
# Words that make the captured literal a name / type / path, not a value.
LITERAL_NOT_VALUE_RE = re.compile(
    rb"(?i)^(?:[A-Z][A-Za-z0-9]*(?:::[A-Za-z0-9_]+)+|[a-z_]+(?:_[a-z_]+)+|[A-Z_]{16,}|"
    rb"[A-Za-z0-9_-]+\.(?:rs|md|conf|toml|yml|yaml|json|txt|pem|key)|/[A-Za-z0-9/._-]+)$"
)

SYNTHETIC_WORDS_RE = re.compile(rb"(?i)example|test-only|fixture|your-|placeholder|dummy|changeme|not-a-secret|certification|cert-only|cfmeetcert")


def _looks_synthetic(literal: bytes) -> bool:
    """Published or obviously fabricated fixture material, never a real secret: a literal
    with almost no distinct characters (``AAAA…``, ``abab…``), an ascending alphabet /
    digit run (``ABCDEFGHIJ…``), or a word that says it is an example."""
    if len(set(literal)) < 8:
        return True
    if SYNTHETIC_WORDS_RE.search(literal):
        return True
    run = 1
    for a, b in zip(literal, literal[1:]):
        run = run + 1 if b == a + 1 else 1
        if run >= 12:
            return True
    return False


RULES_BINARY_SAFE = ("PRIVATE_KEY_BLOCK", "KEYRING_DOCUMENT", "API_CREDENTIAL", "WEBHOOK_SECRET", "CLOUD_ACCESS_KEY")


@dataclass(frozen=True)
class Finding:
    rule: str
    path: str
    line: int

    def __str__(self) -> str:  # pragma: no cover - formatting only
        return f"{self.rule}: {self.path}:{self.line}"


def _line_of(data: bytes, offset: int) -> int:
    return data.count(b"\n", 0, offset) + 1


def _is_template(rel: str) -> bool:
    name = Path(rel).name
    return ".example" in name or name.endswith(".template") or name.endswith(".sample")


def _is_certification_placeholder(rel: str) -> bool:
    # Loopback-only simulation stacks shipped for local certification; documented as
    # non-secret in infra/telephony/README.md and infra/livekit/README.md.
    return (
        rel.startswith("infra/telephony/certification/")
        or (rel.startswith("infra/livekit/") and ("cert" in rel or "dev" in rel))
    )


def _is_test_source(rel: str) -> bool:
    parts = Path(rel).parts
    return "tests" in parts or Path(rel).name.endswith("_test.py") or Path(rel).name.startswith("test_")


def scan_bytes(rel: str, data: bytes) -> list[Finding]:
    findings: list[Finding] = []
    for match in PEM_RE.finditer(data):
        findings.append(Finding("PRIVATE_KEY_BLOCK", rel, _line_of(data, match.start())))
    for match in KEYRING_RE.finditer(data):
        findings.append(Finding("KEYRING_DOCUMENT", rel, _line_of(data, match.start())))
    for rule, pattern in (
        ("API_CREDENTIAL", API_CREDENTIAL_RE),
        ("WEBHOOK_SECRET", WEBHOOK_SECRET_RE),
        ("CLOUD_ACCESS_KEY", CLOUD_KEY_RE),
    ):
        for match in pattern.finditer(data):
            literal = match.group(0)
            # The secret part of a CF Meet credential is its last 64 hex characters.
            if _looks_synthetic(literal) or (rule == "API_CREDENTIAL" and _looks_synthetic(literal[-64:])):
                continue
            findings.append(Finding(rule, rel, _line_of(data, match.start())))
    if b"\0" in data[:4096]:
        return findings  # binary: textual assignment rule does not apply
    if _is_template(rel) or _is_certification_placeholder(rel) or _is_test_source(rel):
        return findings
    for lineno, line in enumerate(data.split(b"\n"), start=1):
        stripped = line.lstrip()
        if stripped.startswith((b"#", b"//", b"*", b"--", b";")):
            continue  # comments may discuss secrets by name
        for match in ASSIGNMENT_RE.finditer(line):
            literal = match.group(2)
            if ASSIGNMENT_SAFE_RE.search(line) or LITERAL_NOT_VALUE_RE.match(literal):
                continue
            # Need real entropy: letters and digits mixed, not a plain word.
            if not (re.search(rb"[0-9]", literal) and re.search(rb"[A-Za-z]", literal)):
                continue
            if _looks_synthetic(literal):
                continue
            findings.append(Finding("CREDENTIAL_ASSIGNMENT", rel, lineno))
            break
    return findings


def tracked_files(repo: Path) -> list[Path]:
    out = subprocess.run(
        ["git", "ls-files", "-z"], cwd=repo, capture_output=True, check=True
    ).stdout
    return [repo / p.decode() for p in out.split(b"\0") if p]


def tree_files(root: Path) -> list[Path]:
    files: list[Path] = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in PRUNE_DIRS and not os.path.islink(os.path.join(dirpath, d))]
        for name in filenames:
            path = Path(dirpath) / name
            if path.is_symlink() or not path.is_file():
                continue
            files.append(path)
    return files


def package_manifest(root: Path) -> dict[str, str]:
    """path (relative, no leading slash) -> MD5 of every file the installed Debian
    packages of an exported root filesystem declare; empty for a non-Debian tree."""
    manifest: dict[str, str] = {}
    info = root / "var/lib/dpkg/info"
    if not info.is_dir():
        return manifest
    for sums in sorted(info.glob("*.md5sums")):
        for line in sums.read_text(encoding="utf-8", errors="replace").splitlines():
            digest, _, rel = line.partition("  ")
            if len(digest) == 32 and rel:
                manifest[rel.lstrip("/")] = digest.lower()
    return manifest


def scan_paths(root: Path, paths: list[Path], packaged: dict[str, str] | None = None) -> list[Finding]:
    findings: list[Finding] = []
    for path in paths:
        try:
            if path.stat().st_size > MAX_FILE_BYTES:
                continue
            data = path.read_bytes()
        except OSError:
            continue
        rel = path.relative_to(root).as_posix()
        if rel.startswith("scripts/secret-scan/"):
            continue  # the scanner's own patterns and fixtures
        if packaged and packaged.get(rel) == hashlib.md5(data).hexdigest():
            continue  # unmodified Debian package content, verified against dpkg
        findings.extend(scan_bytes(rel, data))
    return findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--repo", type=Path, default=None, help="repository root (default: this checkout)")
    parser.add_argument("--tree", type=Path, default=None, help="scan this directory tree instead of git-tracked files")
    args = parser.parse_args(argv)
    if args.tree is not None:
        root = args.tree.resolve()
        if not root.is_dir():
            print(f"error: {root} is not a directory", file=sys.stderr)
            return 2
        findings = scan_paths(root, tree_files(root), package_manifest(root))
    else:
        root = (args.repo or Path(__file__).resolve().parents[2]).resolve()
        if not (root / ".git").exists():
            print(f"error: {root} is not a git repository", file=sys.stderr)
            return 2
        findings = scan_paths(root, tracked_files(root))
    for finding in sorted(set(findings), key=lambda f: (f.path, f.line, f.rule)):
        print(finding)
    if findings:
        print(f"{len(set(findings))} secret finding(s)", file=sys.stderr)
        return 1
    print("secret scan: clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
