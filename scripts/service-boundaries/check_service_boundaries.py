#!/usr/bin/env python3
"""CF Meet service boundary guard (Part B section 13: cross-service data ownership).

Every deployable service under ``services/<name>/`` owns its source, its migrations and
its database. Shared code lives under ``packages/``. This guard fails when a service
crosses another service's boundary:

  CARGO_CROSS_SERVICE_DEPENDENCY  a Cargo manifest of service S has a path (dependency,
                                  patch, workspace member, target path ...) into
                                  services/T, T != S
  PACKAGE_DEPENDS_ON_SERVICE      a manifest under packages/ has a path into services/
  CARGO_PATH_OUTSIDE_REPOSITORY   a manifest path leaves the repository
  CROSS_SERVICE_SOURCE_INCLUDE    include!/include_str!/include_bytes!/#[path] in S (or a
                                  package) resolves into services/T
  CROSS_SERVICE_PATH_LITERAL      Rust source of S names a path inside services/T
  CROSS_SERVICE_DATABASE_URL      S reads another service's database location, e.g.
                                  MEETING_DATABASE_URL (code and deployment config)
  CROSS_SERVICE_TABLE_ACCESS      production SQL of S names a table that only another
                                  service's migrations create
  CROSS_SERVICE_DB_HELPER         production code of S defines or uses a helper named
                                  for another service's database, e.g.
                                  load_user_from_meeting_db
  PRIVACY_RELAY_DOMAIN_ELSEWHERE  another service's migrations create a Contact Vault,
                                  relay-number or inbound-relay table (e.g. a relay-number
                                  inventory under Calling)
  CONTACT_VAULT_OUTSIDE_OWNER     Contact Vault code or crates (AES-GCM, phonenumber, the
                                  plaintext accessor, keyrings) outside Privacy Relay
  PLAINTEXT_CONTACT_FIELD         a plaintext telephone field in Calling's production code
  NUMBERING_IN_PRIVACY_RELAY      permanent Numbering concepts (porting, ownership,
                                  purchase, marketplace) inside Privacy Relay
  FUTURE_SERVICE_IMPLEMENTED      services/numbering or services/contact-vault exists
                                  before its phase (Telephony is a Part E service)

Table ownership is derived from each service's own migrations (raw ``CREATE TABLE`` and
SeaORM ``X::Table`` idents), so it cannot drift from the schema. Known service names are
the directories under services/ plus the service identities registered in
packages/service-auth (future services such as privacy-relay and telephony).

Standard library only. Exit status: 0 clean, 1 violations, 2 usage / repository error.
"""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

PRUNE_DIRS = {".git", "target", "node_modules", "dist", "build", ".next", "__pycache__"}
CONFIG_SUFFIXES = {".toml", ".yml", ".yaml", ".sh", ".env", ".json", ".example"}
CONFIG_NAMES = {"Dockerfile", "docker-compose.yml", "compose.yml", ".env", ".env.example"}
IDENTITY_REGISTRY = Path("packages/service-auth/src/identity.rs")

DB_LOCATION_SUFFIX = r"(?:DATABASE_URL|DB_URL|DATABASE_NAME|POSTGRES_URL|PG_URL|DSN)"
DB_LOCATION_RE = re.compile(r"\b([A-Z][A-Z0-9_]*?)_" + DB_LOCATION_SUFFIX + r"\b")
INCLUDE_RE = re.compile(r"\b(?:include|include_str|include_bytes)!\s*\(\s*\"([^\"]*)\"")
INCLUDE_MANIFEST_RE = re.compile(
    r"\b(?:include|include_str|include_bytes)!\s*\(\s*concat!\s*\(\s*env!\s*\(\s*"
    r"\"CARGO_MANIFEST_DIR\"\s*\)\s*,\s*\"([^\"]*)\""
)
PATH_ATTR_RE = re.compile(r"#\s*\[\s*path\s*=\s*\"([^\"]*)\"\s*\]")
SQL_TABLE_RE = re.compile(
    r"\b(?:from|join|into|update|table|references|truncate)\s+"
    r"(?:if\s+(?:not\s+)?exists\s+)?(?:only\s+)?(?:\\?\")?(?:public(?:\\?\")?\.(?:\\?\")?)?([a-z_][a-z0-9_]*)"
)
RAW_CREATE_RE = re.compile(r"create\s+(?:unlogged\s+)?table\s+(?:if\s+not\s+exists\s+)?\"?([a-z_][a-z0-9_]*)", re.I)
IDEN_TABLE_RE = re.compile(r"\b([A-Z][A-Za-z0-9]*)::Table\b")
IDENT_RE = re.compile(r"\b[A-Za-z_][A-Za-z0-9_]*\b")
HELPER_SUFFIXES = ("db", "database", "pool", "repo", "repository", "conn", "connection", "tables")


@dataclass(frozen=True, order=True)
class Violation:
    rule: str
    path: str
    line: int
    message: str

    def __str__(self) -> str:
        return f"{self.path}:{self.line}: {self.rule}: {self.message}"


# ---------------------------------------------------------------------------
# Rust lexing: strip comments, optionally string / char literals.
# ---------------------------------------------------------------------------


def strip_rust(source: str, keep_strings: bool) -> str:
    """Remove // and (nested) /* */ comments; replace string and char literals with
    empty ones unless ``keep_strings``. Newlines are preserved so line numbers hold."""
    out: list[str] = []
    i, n = 0, len(source)
    while i < n:
        c = source[i]
        nxt = source[i + 1] if i + 1 < n else ""
        if c == "/" and nxt == "/":
            while i < n and source[i] != "\n":
                i += 1
            continue
        if c == "/" and nxt == "*":
            depth, i = 1, i + 2
            while i < n and depth:
                if source.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif source.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    if source[i] == "\n":
                        out.append("\n")
                    i += 1
            continue
        prev = source[i - 1] if i else ""
        raw = re.match(r'(?:b?r)(#*)"', source[i:]) if c in "rb" and not (prev.isalnum() or prev == "_") else None
        if raw:
            hashes = raw.group(1)
            start = i + raw.end()
            end = source.find('"' + hashes, start)
            end = n if end < 0 else end
            body = source[start:end]
            out.append(source[i:start] + (body if keep_strings else "\n" * body.count("\n")) + '"' + hashes)
            i = end + 1 + len(hashes)
            continue
        if c == '"':
            j = i + 1
            while j < n and source[j] != '"':
                j += 2 if source[j] == "\\" else 1
            body = source[i + 1 : j]
            out.append('"' + (body if keep_strings else "\n" * body.count("\n")) + '"')
            i = j + 1
            continue
        if c == "'":
            # char literal ('x', '\n', '\u{..}') versus lifetime ('a)
            if nxt == "\\":
                j = source.find("'", i + 2)
                j = n if j < 0 else j
                out.append("''" if not keep_strings else source[i : j + 1])
                i = j + 1
                continue
            if i + 2 < n and source[i + 2] == "'":
                out.append("''" if not keep_strings else source[i : i + 3])
                i += 3
                continue
        out.append(c)
        i += 1
    return "".join(out)


def remove_cfg_test_items(code: str) -> str:
    """Blank every brace-balanced ``#[cfg(test)]`` item (unit-test modules), keeping
    newlines so line numbers are unchanged."""
    while True:
        start = code.find("#[cfg(test)]")
        if start < 0:
            return code
        open_brace = code.find("{", start)
        semicolon = code.find(";", start)
        if open_brace < 0 or (0 <= semicolon < open_brace):
            end = semicolon + 1 if semicolon >= 0 else len(code)
        else:
            depth, end = 0, len(code)
            for k in range(open_brace, len(code)):
                if code[k] == "{":
                    depth += 1
                elif code[k] == "}":
                    depth -= 1
                    if depth == 0:
                        end = k + 1
                        break
        code = code[:start] + "\n" * code[start:end].count("\n") + code[end:]


def line_of(text: str, index: int) -> int:
    return text.count("\n", 0, index) + 1


# ---------------------------------------------------------------------------
# Repository model.
# ---------------------------------------------------------------------------


def snake(name: str) -> str:
    return name.replace("-", "_").lower()


def camel_to_snake(name: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "_", name).lower()


# Logical components that live INSIDE a service's trust boundary (Part D: the Contact
# Vault is part of Privacy Relay), so `CONTACT_VAULT_DATABASE_URL` is Privacy Relay's.
COMPONENT_ALIASES = {"privacy-relay": {"contact_vault"}}

# Part D §31–§36 boundaries.
CONTACT_VAULT_OWNER = "privacy-relay"
CONTACT_VAULT_CRATES = {"aes-gcm", "phonenumber"}
CONTACT_VAULT_MARKER_RE = re.compile(
    r"\b(?:aes_gcm|phonenumber|expose_e164|ContactKeyProvider|FileKeyring|VaultKeys|contact_vault)\b"
)
PLAINTEXT_FIELD_SERVICES = {"calling"}
PLAINTEXT_FIELD_RE = re.compile(
    r"\b(phone_number|telephone_number|msisdn|e164|destination_number|caller_number|private_number|raw_number)\s*:",
    re.I,
)
NUMBERING_RE = re.compile(
    r"\b[a-z_]*(?:number_porting|port_in|port_out|number_port|permanent_number|number_marketplace|"
    r"number_ownership|purchase_number|number_purchase)[a-z_]*\b",
    re.I,
)
FUTURE_SERVICES = {"numbering", "contact-vault"}
PRIVACY_RELAY_DOMAIN_PREFIXES = (
    "protected_contact", "contact_fingerprint", "contact_access", "contact_verification",
    "contact_resolution", "relay_number", "relay_inbound",
)


def aliases(service: str) -> set[str]:
    """Lower-case snake aliases a service is referred to by: meeting-service ->
    {meeting_service, meeting}; privacy-relay -> {privacy_relay, contact_vault}."""
    s = snake(service)
    out = {s} | COMPONENT_ALIASES.get(service, set())
    if s.endswith("_service") and len(s) > len("_service"):
        out.add(s[: -len("_service")])
    return out


class Repository:
    def __init__(self, root: Path):
        self.root = root.resolve()
        services_dir = self.root / "services"
        if not services_dir.is_dir():
            raise SystemExit(f"error: {services_dir} not found (run from the repository root or pass --root)")
        self.services = sorted(p.name for p in services_dir.iterdir() if p.is_dir() and not p.name.startswith("."))
        self.known_services = set(self.services) | self._registered_identities()
        self.tables = {s: self._owned_tables(s) for s in self.services}

    def _registered_identities(self) -> set[str]:
        registry = self.root / IDENTITY_REGISTRY
        if not registry.is_file():
            return set()
        code = strip_rust(registry.read_text(encoding="utf-8"), keep_strings=True)
        code = remove_cfg_test_items(code)
        return set(re.findall(r'"cfmeet\.([a-z][a-z0-9]*(?:-[a-z0-9]+)*)"', code))

    def _owned_tables(self, service: str) -> set[str]:
        tables: set[str] = set()
        for path in self.files(self.root / "services" / service):
            if path.suffix != ".rs" or "migration" not in path.relative_to(self.root).parts:
                continue
            code = strip_rust(path.read_text(encoding="utf-8", errors="replace"), keep_strings=True)
            tables |= {m.lower() for m in RAW_CREATE_RE.findall(code)}
            tables |= {camel_to_snake(m) for m in IDEN_TABLE_RE.findall(code)}
        return tables

    def files(self, base: Path):
        if not base.exists():
            return
        stack = [base]
        while stack:
            current = stack.pop()
            for entry in sorted(current.iterdir()):
                if entry.is_dir():
                    if entry.name not in PRUNE_DIRS:
                        stack.append(entry)
                elif entry.is_file():
                    yield entry

    def owner(self, path: Path) -> tuple[str, str | None]:
        """('service', name) | ('package', name) | ('repository', None) | ('outside', None)."""
        try:
            rel = path.resolve().relative_to(self.root)
        except ValueError:
            return ("outside", None)
        parts = rel.parts
        if len(parts) >= 2 and parts[0] == "services":
            return ("service", parts[1])
        if len(parts) >= 2 and parts[0] == "packages":
            return ("package", parts[1])
        return ("repository", None)

    def rel(self, path: Path) -> str:
        try:
            return path.resolve().relative_to(self.root).as_posix()
        except ValueError:
            return str(path)


# ---------------------------------------------------------------------------
# Rules.
# ---------------------------------------------------------------------------


def toml_paths(value, keypath=()):
    """Yield (keypath, string) for every path-like value of a Cargo manifest: every
    ``path`` key at any depth and every workspace member / exclude entry."""
    if isinstance(value, dict):
        for k, v in value.items():
            if k == "path" and isinstance(v, str):
                yield keypath + (k,), v
            elif k in ("members", "exclude", "default-members") and keypath[-1:] == ("workspace",) and isinstance(v, list):
                for item in v:
                    if isinstance(item, str):
                        yield keypath + (k,), item
            else:
                yield from toml_paths(v, keypath + (k,))
    elif isinstance(value, list):
        for item in value:
            yield from toml_paths(item, keypath)


def resolve_glob_free(base: Path, value: str) -> Path:
    # workspace members may be globs ("crates/*"); the directory part decides ownership.
    cleaned = value.split("*")[0].rstrip("/") or "."
    return (base / cleaned).resolve()


def check_manifest(repo: Repository, manifest: Path, out: list[Violation]):
    kind, name = repo.owner(manifest)
    try:
        data = tomllib.loads(manifest.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as e:
        out.append(Violation("CARGO_MANIFEST_INVALID", repo.rel(manifest), 1, str(e)))
        return
    text_lines = manifest.read_text(encoding="utf-8").splitlines()
    if name != CONTACT_VAULT_OWNER:
        for section in ("dependencies", "dev-dependencies", "build-dependencies"):
            for crate in CONTACT_VAULT_CRATES & set(data.get(section, {})):
                line = next((i + 1 for i, l in enumerate(text_lines) if l.strip().startswith(crate)), 1)
                out.append(Violation("CONTACT_VAULT_OUTSIDE_OWNER", repo.rel(manifest), line, f"`{crate}` is Contact Vault machinery; only services/{CONTACT_VAULT_OWNER} encrypts or parses private numbers"))
    for keypath, value in toml_paths(data):
        target = resolve_glob_free(manifest.parent, value)
        target_kind, target_name = repo.owner(target)
        line = next((i + 1 for i, l in enumerate(text_lines) if value in l), 1)
        where = ".".join(keypath)
        if target_kind == "outside":
            out.append(Violation("CARGO_PATH_OUTSIDE_REPOSITORY", repo.rel(manifest), line, f"`{where} = \"{value}\"` leaves the repository"))
        elif kind == "service" and target_kind == "service" and target_name != name:
            out.append(Violation("CARGO_CROSS_SERVICE_DEPENDENCY", repo.rel(manifest), line, f"`{where} = \"{value}\"` reaches into services/{target_name}; share code through packages/ instead"))
        elif kind == "package" and target_kind == "service":
            out.append(Violation("PACKAGE_DEPENDS_ON_SERVICE", repo.rel(manifest), line, f"`{where} = \"{value}\"`: shared packages must not depend on services/{target_name}"))


def crate_root(path: Path) -> Path:
    for parent in path.parents:
        if (parent / "Cargo.toml").is_file():
            return parent
    return path.parent


def is_production_rust(repo: Repository, path: Path) -> bool:
    parts = Path(repo.rel(path)).parts
    if any(p in ("tests", "benches", "examples") for p in parts):
        return False
    return path.name not in ("tests.rs", "test.rs") and not path.name.endswith("_test.rs")


def foreign_aliases(repo: Repository, service: str | None) -> dict[str, str]:
    """alias -> service, for every known service other than ``service``."""
    own = aliases(service) if service else set()
    out: dict[str, str] = {}
    for other in sorted(repo.known_services):
        if other == service:
            continue
        for alias in aliases(other):
            if alias not in own:
                out[alias] = other
    return out


def db_location_violations(repo, path, text, service, out):
    foreign = foreign_aliases(repo, service)
    for m in DB_LOCATION_RE.finditer(text):
        prefix = m.group(1).lower()
        segments = prefix.split("_")
        hit = None
        for alias, other in foreign.items():
            a = alias.split("_")
            if any(segments[k : k + len(a)] == a for k in range(len(segments) - len(a) + 1)):
                hit = other
                break
        if hit:
            out.append(Violation("CROSS_SERVICE_DATABASE_URL", repo.rel(path), line_of(text, m.start()), f"`{m.group(0)}` locates the database of {hit}; services never connect to another service's database"))


def check_rust(repo: Repository, path: Path, out: list[Violation]):
    kind, name = repo.owner(path)
    source = path.read_text(encoding="utf-8", errors="replace")
    with_strings = strip_rust(source, keep_strings=True)
    rel = repo.rel(path)

    def foreign_target(target: Path) -> str | None:
        t_kind, t_name = repo.owner(target)
        if t_kind != "service":
            return None
        if kind == "package" or (kind == "service" and t_name != name):
            return t_name
        return None

    for regex, base in ((INCLUDE_RE, path.parent), (PATH_ATTR_RE, path.parent), (INCLUDE_MANIFEST_RE, crate_root(path))):
        for m in regex.finditer(with_strings):
            literal = m.group(1).lstrip("/") if regex is INCLUDE_MANIFEST_RE else m.group(1)
            target = (base / literal).resolve()
            other = foreign_target(target)
            if other:
                out.append(Violation("CROSS_SERVICE_SOURCE_INCLUDE", rel, line_of(with_strings, m.start()), f"`{m.group(0)}` compiles source of services/{other}"))

    if kind == "service":
        for other in sorted(repo.services):
            if other == name:
                continue
            pattern = re.compile(r"\"[^\"\n]*(?:(?:\.\./)+|services/)" + re.escape(other) + r"(?:/[^\"\n]*)?\"")
            for m in pattern.finditer(with_strings):
                out.append(Violation("CROSS_SERVICE_PATH_LITERAL", rel, line_of(with_strings, m.start()), f"{m.group(0)} reaches into services/{other}"))

    if kind != "service":
        return
    db_location_violations(repo, path, with_strings, name, out)

    if not is_production_rust(repo, path):
        return
    production = remove_cfg_test_items(with_strings)
    own_tables = repo.tables.get(name, set())
    foreign_tables = {t: s for s, ts in repo.tables.items() if s != name for t in ts if t not in own_tables}
    lowered = production.lower()
    for m in SQL_TABLE_RE.finditer(lowered):
        table = m.group(1)
        if table in foreign_tables:
            out.append(Violation("CROSS_SERVICE_TABLE_ACCESS", rel, line_of(lowered, m.start()), f"`{m.group(0).strip()}` uses table `{table}` owned by {foreign_tables[table]}"))

    identifiers_only = remove_cfg_test_items(strip_rust(source, keep_strings=False))
    foreign = foreign_aliases(repo, name)
    for m in IDENT_RE.finditer(identifiers_only):
        ident = m.group(0).lower()
        segments = ident.split("_")
        for alias, other in foreign.items():
            a = alias.split("_")
            hit = any(
                segments[k : k + len(a)] == a and k + len(a) < len(segments) and segments[k + len(a)] in HELPER_SUFFIXES
                for k in range(len(segments) - len(a))
            )
            if hit:
                out.append(Violation("CROSS_SERVICE_DB_HELPER", rel, line_of(identifiers_only, m.start()), f"`{m.group(0)}` accesses the data store of {other}; call its authenticated API or consume its events"))
                break

    if name != CONTACT_VAULT_OWNER:
        for m in CONTACT_VAULT_MARKER_RE.finditer(identifiers_only):
            out.append(Violation("CONTACT_VAULT_OUTSIDE_OWNER", rel, line_of(identifiers_only, m.start()), f"`{m.group(0)}`: Contact Vault code exists only in services/{CONTACT_VAULT_OWNER}"))
    if name in PLAINTEXT_FIELD_SERVICES:
        for m in PLAINTEXT_FIELD_RE.finditer(identifiers_only):
            out.append(Violation("PLAINTEXT_CONTACT_FIELD", rel, line_of(identifiers_only, m.start()), f"`{m.group(1)}`: {name} handles ContactReferences, never telephone numbers"))
    if name == CONTACT_VAULT_OWNER:
        for m in NUMBERING_RE.finditer(identifiers_only):
            out.append(Violation("NUMBERING_IN_PRIVACY_RELAY", rel, line_of(identifiers_only, m.start()), f"`{m.group(0)}`: permanent public-number lifecycle belongs to the future Numbering service"))


def is_config(path: Path) -> bool:
    return path.name in CONFIG_NAMES or path.suffix in CONFIG_SUFFIXES or path.name.startswith(".env")


def check_config(repo: Repository, path: Path, out: list[Violation]):
    kind, name = repo.owner(path)
    if kind != "service":
        return
    text = path.read_text(encoding="utf-8", errors="replace")
    if path.suffix != ".json":
        # full-line comments may discuss other services
        text = "\n".join("" if l.lstrip().startswith("#") else l for l in text.splitlines())
    db_location_violations(repo, path, text, name, out)


def check(root: Path) -> list[Violation]:
    repo = Repository(root)
    out: list[Violation] = []
    for future in sorted(FUTURE_SERVICES & set(repo.services)):
        out.append(Violation("FUTURE_SERVICE_IMPLEMENTED", f"services/{future}", 1, f"services/{future} belongs to a later phase and must not exist yet"))
    for service, tables in sorted(repo.tables.items()):
        if service == CONTACT_VAULT_OWNER:
            continue
        for table in sorted(t for t in tables if t.startswith(PRIVACY_RELAY_DOMAIN_PREFIXES)):
            out.append(Violation("PRIVACY_RELAY_DOMAIN_ELSEWHERE", f"services/{service}", 1, f"table `{table}` belongs to services/{CONTACT_VAULT_OWNER} (Contact Vault / relay numbers); no other service stores it"))
    for top in ("services", "packages"):
        for path in repo.files(repo.root / top):
            if path.name == "Cargo.toml":
                check_manifest(repo, path, out)
            if path.suffix == ".rs":
                check_rust(repo, path, out)
            elif is_config(path):
                check_config(repo, path, out)
    return sorted(set(out))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2], help="repository root")
    args = parser.parse_args(argv)
    try:
        violations = check(args.root)
    except SystemExit as e:
        print(e, file=sys.stderr)
        return 2
    repo = Repository(args.root)
    for v in violations:
        print(v)
    owned = ", ".join(f"{s}: {len(t)} tables" for s, t in sorted(repo.tables.items()))
    if violations:
        print(f"service boundaries: FAILED — {len(violations)} violation(s)", file=sys.stderr)
        return 1
    print(f"service boundaries: OK — services {repo.services}, known identities {sorted(repo.known_services)}; {owned}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
