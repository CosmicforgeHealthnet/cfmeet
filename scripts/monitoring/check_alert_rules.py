#!/usr/bin/env python3
"""CF Meet alert-rule policy check (SEC-016).

Complements ``promtool check rules`` / ``promtool test rules`` / ``amtool check-config``
(syntax and behaviour) with repository policy that those tools cannot know:

  RULE_FILE          a rule file parses (simple block-style YAML subset, see below) and
                     has groups -> rules -> alert entries with expr
  DUPLICATE_ALERT    an alert name is defined once across all rule files
  SEVERITY           label ``severity`` is one of critical / warning / info
  APPROVAL           label ``approval`` is ``documented`` (threshold stated in a runbook)
                     or ``required`` (runbook says APPROVAL REQUIRED / gives no value)
  SERVICE            label ``service`` is present
  THRESHOLD_STATUS   an ``approval: required`` alert carries annotation
                     ``threshold_status`` starting with "APPROVAL REQUIRED"
  RUNBOOK            annotation ``runbook_url`` names a repository file that exists, and its
                     ``#anchor`` (if any) matches a heading of that file
  SUMMARY            annotations ``summary`` and ``description`` are present
  UNKNOWN_METRIC     every metric name used in an expr is exported by service code: the
                     name (or the histogram base of ``_bucket`` / ``_sum`` / ``_count``)
                     appears in ``services/*/src/**/*.rs``, or it is a synthesized
                     ``<prefix>_<field>_total`` counter whose field is listed in the
                     ``counters!`` block the prefix is exported from, or it is in
                     METRIC_ALLOWLIST (Prometheus' own series)
  UNTESTED_CRITICAL  every critical alert is exercised by a promtool unit test
                     (``alertname: <name>`` in ``infra/monitoring/prometheus/tests``)
  ALERTMANAGER       the Alertmanager config contains no URL and no literal key / token /
                     password / secret value (comments included; ``<PLACEHOLDER>`` values
                     and ``*_file`` paths are allowed), and the repository secret scan
                     finds nothing in it

The YAML reader is deliberately small (standard library only, like every repository
guard): block mappings and sequences, plain / quoted scalars, ``|`` / ``>`` block scalars
and flow sequences of scalars. Anything else is reported instead of guessed.

Run: python3 scripts/monitoring/check_alert_rules.py   (exit 0 clean, 1 findings)
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]

RULES_GLOB = "infra/monitoring/prometheus/rules/*.rules.yml"
TESTS_GLOB = "infra/monitoring/prometheus/tests/*.test.yml"
ALERTMANAGER = "infra/monitoring/alertmanager/alertmanager.yml"

SEVERITIES = {"critical", "warning", "info"}
APPROVALS = {"documented", "required"}

# Series Prometheus itself produces for every scrape target; not exported by service code.
METRIC_ALLOWLIST = {"up", "scrape_duration_seconds", "scrape_samples_scraped"}

# Counters whose exported names are synthesized at runtime as "<prefix>_<field>_total"
# (services/meeting-service/src/handlers/internal.rs::prometheus_counters). The field
# must be listed in the counters! block of the named file, and the exporter must pass the
# prefix literally.
SYNTHESIZED_COUNTERS = {
    "cfmeet_rtc": "services/meeting-service/src/services/rtc_metrics.rs",
    "cfmeet_collab": "services/meeting-service/src/services/collaboration/metrics.rs",
    "cfmeet_ops": "services/meeting-service/src/services/ops_metrics.rs",
}
SYNTHESIZED_EXPORTER = "services/meeting-service/src/handlers/internal.rs"
SYNTHESIZED_FORMAT = "{prefix}_{k}_total"

# PromQL words that are not metric names (aggregations, operators, modifiers, literals).
PROMQL_KEYWORDS = {
    "and", "or", "unless", "by", "without", "on", "ignoring", "group_left", "group_right",
    "bool", "offset", "atan2", "sum", "min", "max", "avg", "count", "stddev", "stdvar",
    "topk", "bottomk", "quantile", "count_values", "group", "limitk", "limit_ratio",
    "inf", "nan", "start", "end",
}

SENSITIVE_KEY_RE = re.compile(
    r"(?i)(url|key|token|secret|password|passwd|credential|auth|bearer|webhook|routing|service_key|api)"
)
PLACEHOLDER_RE = re.compile(r"^<[A-Z][A-Z0-9_]*>(?:/<[A-Z][A-Z0-9_]*>)*$")
URL_RE = re.compile(r"(?i)\b[a-z][a-z0-9+.-]*://")


@dataclass(frozen=True)
class Finding:
    rule: str
    path: str
    detail: str

    def __str__(self) -> str:  # pragma: no cover - formatting only
        return f"{self.rule}: {self.path}: {self.detail}"


# --------------------------------------------------------------------------- YAML subset


class YamlError(ValueError):
    pass


def _strip_comment(text: str) -> str:
    """Removes a trailing ``# comment`` that is outside quotes."""
    quote = ""
    for i, ch in enumerate(text):
        if quote:
            if ch == quote:
                quote = ""
        elif ch in "'\"":
            quote = ch
        elif ch == "#" and (i == 0 or text[i - 1] in " \t"):
            return text[:i].rstrip()
    return text.rstrip()


def _split_flow(body: str) -> list[str]:
    items, quote, depth, cur = [], "", 0, ""
    for ch in body:
        if quote:
            cur += ch
            if ch == quote:
                quote = ""
            continue
        if ch in "'\"":
            quote = ch
        elif ch in "[{":
            depth += 1
        elif ch in "]}":
            depth -= 1
        if ch == "," and depth == 0:
            items.append(cur.strip())
            cur = ""
        else:
            cur += ch
    if cur.strip():
        items.append(cur.strip())
    return items


def _scalar(raw: str, lineno: int):
    raw = raw.strip()
    if raw.startswith("[") and raw.endswith("]"):
        return [_scalar(item, lineno) for item in _split_flow(raw[1:-1])]
    if raw.startswith("{"):
        raise YamlError(f"line {lineno}: flow mappings are not supported")
    if len(raw) >= 2 and raw[0] == raw[-1] == "'":
        return raw[1:-1].replace("''", "'")
    if len(raw) >= 2 and raw[0] == raw[-1] == '"':
        body = raw[1:-1]
        return re.sub(r"\\(.)", lambda m: {"n": "\n", "t": "\t"}.get(m.group(1), m.group(1)), body)
    if raw[:1] in "&*!|>":
        raise YamlError(f"line {lineno}: unsupported YAML construct {raw[:1]!r}")
    return raw


_KEY_RE = re.compile(r"^([A-Za-z0-9_.\-/]+|'[^']*'|\"[^\"]*\")\s*:(?:\s+(.*))?$")


class _Reader:
    def __init__(self, text: str):
        self.raw = text.replace("\r\n", "\n").split("\n")
        self.i = 0

    def _peek(self):
        """(index, indent, content) of the next significant line, or None."""
        j = self.i
        while j < len(self.raw):
            line = self.raw[j]
            stripped = line.strip()
            if stripped and not stripped.startswith("#") and stripped != "---":
                if "\t" in line[: len(line) - len(line.lstrip())]:
                    raise YamlError(f"line {j + 1}: tab indentation")
                return j, len(line) - len(line.lstrip(" ")), _strip_comment(line.strip())
            j += 1
        return None

    def node(self, min_indent: int):
        nxt = self._peek()
        if nxt is None or nxt[1] < min_indent:
            return None
        _, indent, content = nxt
        if content == "-" or content.startswith("- "):
            return self.seq(indent)
        return self.mapping(indent)

    def block_scalar(self, parent_indent: int, style: str) -> str:
        lines: list[str] = []
        block_indent = None
        while self.i < len(self.raw):
            line = self.raw[self.i]
            if line.strip():
                ind = len(line) - len(line.lstrip(" "))
                if ind <= parent_indent:
                    break
                if block_indent is None:
                    block_indent = ind
                lines.append(line[block_indent:])
            else:
                lines.append("")
            self.i += 1
        while lines and not lines[-1]:
            lines.pop()
        if style.startswith(">"):
            return " ".join(part.strip() for part in lines if part.strip())
        return "\n".join(lines) + ("" if style.endswith("-") else "\n")

    def mapping(self, indent: int) -> dict:
        out: dict = {}
        while True:
            nxt = self._peek()
            if nxt is None or nxt[1] < indent:
                return out
            j, ind, content = nxt
            if ind > indent:
                raise YamlError(f"line {j + 1}: unexpected indentation")
            if content.startswith("- ") or content == "-":
                return out
            m = _KEY_RE.match(content)
            if not m:
                raise YamlError(f"line {j + 1}: expected 'key: value'")
            key = _scalar(m.group(1), j + 1)
            value = (m.group(2) or "").strip()
            if key in out:
                raise YamlError(f"line {j + 1}: duplicate key {key!r}")
            self.i = j + 1
            if value in ("|", "|-", "|+", ">", ">-", ">+"):
                out[key] = self.block_scalar(ind, value)
            elif value:
                out[key] = _scalar(value, j + 1)
            else:
                child = self._peek()
                if child is not None and child[1] == ind and (child[2].startswith("- ") or child[2] == "-"):
                    out[key] = self.seq(ind)
                else:
                    out[key] = self.node(ind + 1)

    def seq(self, indent: int) -> list:
        out: list = []
        while True:
            nxt = self._peek()
            if nxt is None or nxt[1] != indent:
                return out
            j, ind, content = nxt
            if not (content.startswith("- ") or content == "-"):
                return out
            rest = content[1:].strip()
            if not rest:
                self.i = j + 1
                out.append(self.node(ind + 1))
            elif _KEY_RE.match(rest):
                # "- key: value" opens a mapping whose keys sit two columns deeper.
                line = self.raw[j]
                dash = line.index("-")
                self.raw[j] = line[:dash] + " " + line[dash + 1 :]
                self.i = j
                out.append(self.mapping(self._peek()[1]))
            else:
                self.i = j + 1
                out.append(_scalar(rest, j + 1))


def parse_yaml(text: str):
    reader = _Reader(text)
    doc = reader.node(0)
    rest = reader._peek()
    if rest is not None:
        raise YamlError(f"line {rest[0] + 1}: unparsed content")
    return doc


# ------------------------------------------------------------------------------- PromQL


def metric_names(expr: str) -> set[str]:
    """Metric names referenced by a PromQL expression (functions, aggregations, keywords,
    label matchers, ranges, durations and grouping labels excluded)."""
    s = re.sub(r'"(?:[^"\\]|\\.)*"|\'(?:[^\'\\]|\\.)*\'|`[^`]*`', '""', expr)
    s = re.sub(r"\{[^}]*\}", " ", s)
    s = re.sub(r"\[[^\]]*\]", " ", s)
    s = re.sub(r"(?i)\b(by|without|on|ignoring|group_left|group_right)\s*\([^)]*\)", r"\1 ", s)
    names: set[str] = set()
    for m in re.finditer(r"(?<![\w.:])([A-Za-z_:][A-Za-z0-9_:]*)", s):
        name = m.group(1)
        if re.match(r"\s*\(", s[m.end() :]):
            continue  # function call or aggregation
        if name.lower() in PROMQL_KEYWORDS:
            continue
        names.add(name)
    return names


class MetricIndex:
    """What service code exports, read from ``services/*/src/**/*.rs``."""

    def __init__(self, root: Path):
        self.root = root
        texts = [p.read_text(encoding="utf-8", errors="replace") for p in sorted(root.glob("services/*/src/**/*.rs"))]
        self.corpus = "\n".join(texts)
        self.fields: dict[str, set[str]] = {}
        exporter = root / SYNTHESIZED_EXPORTER
        exporter_text = exporter.read_text(encoding="utf-8") if exporter.is_file() else ""
        for prefix, rel in SYNTHESIZED_COUNTERS.items():
            path = root / rel
            if not path.is_file() or f'"{prefix}"' not in exporter_text or SYNTHESIZED_FORMAT not in exporter_text:
                continue
            self.fields[prefix] = counters_fields(path.read_text(encoding="utf-8"))

    def exported(self, name: str) -> bool:
        if name in METRIC_ALLOWLIST:
            return True
        candidates = {name}
        for suffix in ("_bucket", "_sum", "_count"):
            if name.endswith(suffix):
                candidates.add(name[: -len(suffix)])
        for cand in candidates:
            if re.search(r"(?<![A-Za-z0-9_])" + re.escape(cand) + r"(?![A-Za-z0-9_])", self.corpus):
                return True
        if name.endswith("_total"):
            for prefix, fields in self.fields.items():
                if name.startswith(prefix + "_") and name[len(prefix) + 1 : -len("_total")] in fields:
                    return True
        return False


def counters_fields(text: str) -> set[str]:
    fields: set[str] = set()
    for block in re.findall(r"counters!\s*\((.*?)\);", text, flags=re.S):
        if "$" in block:
            continue  # the macro definition, not an invocation
        for line in block.splitlines():
            line = line.split("//", 1)[0].strip()
            fields.update(re.findall(r"\b([a-z_][a-z0-9_]*)\b", line))
    return fields


# ------------------------------------------------------------------------------- checks


def github_slug(heading: str) -> str:
    slug = heading.strip().lower()
    slug = re.sub(r"[^\w\- ]", "", slug)
    return slug.replace(" ", "-")


def runbook_ok(root: Path, ref: str) -> str | None:
    """None when ``path[#anchor]`` resolves, otherwise the reason."""
    path, _, anchor = ref.partition("#")
    if not path or "://" in path:
        return f"runbook_url must be a repository path, got {ref!r}"
    target = root / path
    if not target.is_file():
        return f"runbook file {path} does not exist"
    if anchor:
        headings = re.findall(r"^#{1,6}\s+(.+?)\s*#*\s*$", target.read_text(encoding="utf-8"), flags=re.M)
        if anchor not in {github_slug(h) for h in headings}:
            return f"anchor #{anchor} is not a heading of {path}"
    return None


def _rules(doc) -> list:
    groups = doc.get("groups") if isinstance(doc, dict) else None
    if not isinstance(groups, list):
        return []
    return [r for g in groups if isinstance(g, dict) and isinstance(g.get("rules"), list) for r in g["rules"]]


def recorded_names(docs) -> set[str]:
    """Series produced by recording rules (usable in alert expressions)."""
    return {str(r["record"]) for doc in docs for r in _rules(doc) if isinstance(r, dict) and "record" in r}


def check_rule_file(
    root: Path, rel: str, doc, index: MetricIndex, records: set[str], seen: dict[str, str]
) -> tuple[list[Finding], list[tuple[str, str]]]:
    findings: list[Finding] = []
    alerts: list[tuple[str, str]] = []
    groups = doc.get("groups") if isinstance(doc, dict) else None
    if not isinstance(groups, list) or not groups:
        return [Finding("RULE_FILE", rel, "no groups")], alerts
    for group in groups:
        rules = group.get("rules") if isinstance(group, dict) else None
        if not isinstance(rules, list) or not rules:
            findings.append(Finding("RULE_FILE", rel, "group without rules"))
            continue
        for rule in rules:
            if not isinstance(rule, dict):
                findings.append(Finding("RULE_FILE", rel, "rule is not a mapping"))
                continue
            if "record" in rule:
                name = str(rule["record"])
            elif "alert" in rule:
                name = str(rule["alert"])
            else:
                findings.append(Finding("RULE_FILE", rel, "rule without alert or record"))
                continue
            expr = rule.get("expr")
            if not isinstance(expr, str) or not expr.strip():
                findings.append(Finding("RULE_FILE", rel, f"{name}: missing expr"))
                expr = ""
            for metric in sorted(metric_names(expr)):
                if not index.exported(metric) and metric not in records:
                    findings.append(Finding("UNKNOWN_METRIC", rel, f"{name}: {metric} is not exported by services/*/src"))
            if "record" in rule:
                continue
            if name in seen:
                findings.append(Finding("DUPLICATE_ALERT", rel, f"{name} also defined in {seen[name]}"))
            seen[name] = rel
            labels = rule.get("labels") if isinstance(rule.get("labels"), dict) else {}
            annotations = rule.get("annotations") if isinstance(rule.get("annotations"), dict) else {}
            severity = labels.get("severity")
            approval = labels.get("approval")
            if severity not in SEVERITIES:
                findings.append(Finding("SEVERITY", rel, f"{name}: severity {severity!r} not in {sorted(SEVERITIES)}"))
            if approval not in APPROVALS:
                findings.append(Finding("APPROVAL", rel, f"{name}: approval {approval!r} not in {sorted(APPROVALS)}"))
            if not labels.get("service"):
                findings.append(Finding("SERVICE", rel, f"{name}: missing service label"))
            if approval == "required" and not str(annotations.get("threshold_status", "")).startswith("APPROVAL REQUIRED"):
                findings.append(Finding("THRESHOLD_STATUS", rel, f"{name}: approval required needs threshold_status 'APPROVAL REQUIRED: ...'"))
            for key in ("summary", "description"):
                if not annotations.get(key):
                    findings.append(Finding("SUMMARY", rel, f"{name}: missing {key} annotation"))
            ref = annotations.get("runbook_url")
            if not ref:
                findings.append(Finding("RUNBOOK", rel, f"{name}: missing runbook_url annotation"))
            else:
                reason = runbook_ok(root, str(ref))
                if reason:
                    findings.append(Finding("RUNBOOK", rel, f"{name}: {reason}"))
            alerts.append((name, str(severity)))
    return findings, alerts


def _secret_scan_findings(rel: str, data: bytes) -> list[str]:
    sys.path.insert(0, str(REPO / "scripts" / "secret-scan"))
    try:
        import scan_secrets  # noqa: PLC0415
    finally:
        sys.path.pop(0)
    return [f.rule for f in scan_secrets.scan_bytes(rel, data)]


def check_alertmanager(rel: str, text: str) -> list[Finding]:
    findings: list[Finding] = []
    try:
        parse_yaml(text)
    except YamlError as e:
        findings.append(Finding("ALERTMANAGER", rel, str(e)))
    for lineno, line in enumerate(text.splitlines(), start=1):
        body = line.strip().lstrip("#").strip().lstrip("-").strip()
        if URL_RE.search(body):
            findings.append(Finding("ALERTMANAGER", rel, f"line {lineno}: URL not allowed (use a *_file from the secret store)"))
            continue
        m = re.match(r"^([A-Za-z0-9_]+)\s*:\s*(.+)$", body)
        if not m:
            continue
        key, value = m.group(1), m.group(2).strip().strip("'\"")
        if not SENSITIVE_KEY_RE.search(key) or not value:
            continue
        if key.endswith("_file") and (value.startswith("/") or PLACEHOLDER_RE.match(value)):
            continue
        if PLACEHOLDER_RE.match(value):
            continue
        findings.append(Finding("ALERTMANAGER", rel, f"line {lineno}: {key} has a literal value (use a <PLACEHOLDER> or a *_file)"))
    for rule in _secret_scan_findings(rel, text.encode()):
        findings.append(Finding("ALERTMANAGER", rel, f"secret scan: {rule}"))
    return findings


def check(root: Path) -> list[Finding]:
    findings: list[Finding] = []
    rule_files = sorted(root.glob(RULES_GLOB))
    if not rule_files:
        findings.append(Finding("RULE_FILE", RULES_GLOB, "no rule files"))
    index = MetricIndex(root)
    docs: list[tuple[str, object]] = []
    for path in rule_files:
        rel = path.relative_to(root).as_posix()
        try:
            docs.append((rel, parse_yaml(path.read_text(encoding="utf-8"))))
        except YamlError as e:
            findings.append(Finding("RULE_FILE", rel, str(e)))
    records = recorded_names(doc for _, doc in docs)
    seen: dict[str, str] = {}
    alerts: list[tuple[str, str]] = []
    for rel, doc in docs:
        f, a = check_rule_file(root, rel, doc, index, records, seen)
        findings.extend(f)
        alerts.extend(a)
    tested = set()
    for path in sorted(root.glob(TESTS_GLOB)):
        tested.update(re.findall(r"^\s*-?\s*alertname:\s*['\"]?([A-Za-z0-9_:]+)", path.read_text(encoding="utf-8"), flags=re.M))
    for name, severity in alerts:
        if severity == "critical" and name not in tested:
            findings.append(Finding("UNTESTED_CRITICAL", TESTS_GLOB, f"{name} has no promtool unit test"))
    am = root / ALERTMANAGER
    if not am.is_file():
        findings.append(Finding("ALERTMANAGER", ALERTMANAGER, "missing"))
    else:
        findings.extend(check_alertmanager(ALERTMANAGER, am.read_text(encoding="utf-8")))
    return findings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=REPO, help="repository root (default: this checkout)")
    args = parser.parse_args(argv)
    findings = check(args.root.resolve())
    for finding in findings:
        print(finding)
    if findings:
        print(f"{len(findings)} alert-rule policy finding(s)", file=sys.stderr)
        return 1
    print("alert rules: policy clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
