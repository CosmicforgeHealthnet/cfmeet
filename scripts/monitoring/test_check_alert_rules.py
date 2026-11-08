"""Mutation tests for the alert-rule policy check (SEC-016).

* The real repository is clean.
* Every policy rule fires on a minimal synthetic repository that violates only it.
* The PromQL metric extraction ignores functions, aggregations, keywords and labels.

Run: python3 -m unittest discover -s scripts/monitoring -v
"""

from __future__ import annotations

import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import check_alert_rules as chk  # noqa: E402

REPO = HERE.parents[1]

RUST_METRICS = textwrap.dedent(
    """
    pub fn render(out: &mut String) {
        counter(out, "demo_requests_total", 1);
        histogram(out, "demo_latency_seconds");
    }
    """
)
RUST_EXPORTER = textwrap.dedent(
    """
    fn prometheus_counters(out: &mut String, prefix: &str) {
        let name = format!("{prefix}_{k}_total");
    }
    pub fn metrics() { prometheus_counters(&mut out, "cfmeet_rtc", &M.snapshot()); }
    """
)
RUST_COUNTERS = textwrap.dedent(
    """
    counters!(
        provider_errors,
        // a comment that names no_such_counter
        outbox_retried,
    );
    """
)
RUNBOOK = "# Runbook\n\n## Outbox backlog\n\nText.\n\n## Alerts and severity (§84)\n"

RULE = """\
      - alert: {name}
        expr: |
          {expr}
        for: 5m
        labels:
          severity: {severity}
          approval: {approval}
          service: demo
        annotations:
          summary: "s"
          description: "d"
{threshold}          runbook_url: "{runbook}"
"""

THRESHOLD = '          threshold_status: "APPROVAL REQUIRED: placeholder"\n'

ALERTMANAGER = """\
route:
  receiver: unconfigured
  group_by: [alertname, service]
  routes:
    - matchers: ['severity="critical"']
      receiver: unconfigured
receivers:
  - name: unconfigured
    # pagerduty_configs:
    #   - routing_key_file: <SECRET_STORE_MOUNT>/<PAGER_INTEGRATION_KEY_FILE>
"""


def rule(
    name: str = "DemoAlert",
    expr: str = "sum(rate(demo_requests_total[5m])) > 1",
    severity: str = "warning",
    approval: str = "required",
    threshold: bool = True,
    runbook: str = "docs/runbook.md#outbox-backlog",
) -> str:
    return RULE.format(
        name=name, expr=expr, severity=severity, approval=approval,
        threshold=THRESHOLD if threshold else "", runbook=runbook,
    )


def rule_file(*rules: str) -> str:
    return "groups:\n  - name: demo\n    rules:\n" + "".join(rules)


class SyntheticRepo:
    def __init__(self, rules: str, alertmanager: str = ALERTMANAGER, tests: str = "tests: []\n"):
        self._dir = tempfile.TemporaryDirectory()
        self.root = Path(self._dir.name)
        self.write("services/demo/src/metrics.rs", RUST_METRICS)
        self.write(chk.SYNTHESIZED_EXPORTER, RUST_EXPORTER)
        self.write(chk.SYNTHESIZED_COUNTERS["cfmeet_rtc"], RUST_COUNTERS)
        self.write("docs/runbook.md", RUNBOOK)
        self.write("infra/monitoring/prometheus/rules/demo.rules.yml", rules)
        self.write("infra/monitoring/prometheus/tests/demo.test.yml", tests)
        self.write(chk.ALERTMANAGER, alertmanager)

    def write(self, rel: str, text: str) -> None:
        path = self.root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def findings(self) -> set[str]:
        try:
            return {f.rule for f in chk.check(self.root)}
        finally:
            self._dir.cleanup()


def findings(*rules: str, **kwargs) -> set[str]:
    return SyntheticRepo(rule_file(*rules), **kwargs).findings()


class RepositoryTests(unittest.TestCase):
    def test_repository_rules_and_alertmanager_are_clean(self) -> None:
        self.assertEqual([str(f) for f in chk.check(REPO)], [])

    def test_repository_has_meeting_service_rules(self) -> None:
        self.assertTrue((REPO / "infra/monitoring/prometheus/rules/meeting-service.rules.yml").is_file())


class PolicyMutationTests(unittest.TestCase):
    def test_baseline_synthetic_repository_is_clean(self) -> None:
        self.assertEqual(findings(rule()), set())

    def test_severity_must_be_allowed(self) -> None:
        self.assertEqual(findings(rule(severity="page")), {"SEVERITY"})

    def test_approval_must_be_allowed(self) -> None:
        self.assertEqual(findings(rule(approval="approved")), {"APPROVAL"})

    def test_approval_required_needs_threshold_status(self) -> None:
        self.assertEqual(findings(rule(threshold=False)), {"THRESHOLD_STATUS"})
        self.assertEqual(findings(rule(approval="documented", threshold=False)), set())

    def test_runbook_file_and_anchor_must_exist(self) -> None:
        self.assertEqual(findings(rule(runbook="docs/missing.md")), {"RUNBOOK"})
        self.assertEqual(findings(rule(runbook="docs/runbook.md#no-such-heading")), {"RUNBOOK"})
        self.assertEqual(findings(rule(runbook="docs/runbook.md#alerts-and-severity-84")), set())
        self.assertEqual(findings(rule(runbook="https://wiki.invalid/runbook")), {"RUNBOOK"})

    def test_invented_metric_is_rejected(self) -> None:
        self.assertEqual(findings(rule(expr="rate(demo_invented_total[5m]) > 0")), {"UNKNOWN_METRIC"})

    def test_histogram_series_of_an_exported_histogram_are_accepted(self) -> None:
        expr = "histogram_quantile(0.95, sum by (le) (rate(demo_latency_seconds_bucket[5m]))) > 3"
        self.assertEqual(findings(rule(expr=expr)), set())

    def test_synthesized_counter_needs_a_listed_field(self) -> None:
        self.assertEqual(findings(rule(expr="rate(cfmeet_rtc_outbox_retried_total[5m]) > 0")), set())
        self.assertEqual(findings(rule(expr="rate(cfmeet_rtc_invented_total[5m]) > 0")), {"UNKNOWN_METRIC"})
        # A name that appears only in a comment of the counters! block is not a field.
        self.assertEqual(findings(rule(expr="rate(cfmeet_rtc_no_such_counter_total[5m]) > 0")), {"UNKNOWN_METRIC"})
        # A field of another prefix's block does not count.
        self.assertEqual(findings(rule(expr="rate(cfmeet_ops_outbox_retried_total[5m]) > 0")), {"UNKNOWN_METRIC"})

    def test_prometheus_series_allowlist(self) -> None:
        self.assertEqual(findings(rule(expr='up{job="demo"} == 0')), set())

    def test_recording_rule_output_is_a_known_metric(self) -> None:
        record = "      - record: demo:requests:rate5m\n        expr: sum(rate(demo_requests_total[5m]))\n"
        self.assertEqual(findings(record, rule(expr="demo:requests:rate5m > 1")), set())

    def test_duplicate_alert_name(self) -> None:
        self.assertEqual(findings(rule(), rule()), {"DUPLICATE_ALERT"})

    def test_critical_alert_needs_a_unit_test(self) -> None:
        self.assertEqual(findings(rule(severity="critical")), {"UNTESTED_CRITICAL"})
        tests = "tests:\n  - alert_rule_test:\n      - eval_time: 5m\n        alertname: DemoAlert\n        exp_alerts: []\n"
        self.assertEqual(findings(rule(severity="critical"), tests=tests), set())

    def test_missing_summary(self) -> None:
        text = rule().replace('          summary: "s"\n', "")
        self.assertEqual(findings(text), {"SUMMARY"})

    def test_unparseable_rule_file(self) -> None:
        self.assertEqual(SyntheticRepo("groups:\n  - name: demo\n    rules: {a: b}\n").findings(), {"RULE_FILE"})


class AlertmanagerTests(unittest.TestCase):
    def test_url_is_rejected_even_in_a_comment(self) -> None:
        am = ALERTMANAGER + "    #     url: " + "https" + "://hooks.invalid/services/x\n"
        self.assertEqual(findings(rule(), alertmanager=am), {"ALERTMANAGER"})

    def test_literal_key_is_rejected(self) -> None:
        am = ALERTMANAGER + "    #   - routing_key: " + "Q7f3K9x2M4p8R1t6W0z5" + "\n"
        self.assertEqual(findings(rule(), alertmanager=am), {"ALERTMANAGER"})
        am = ALERTMANAGER.replace("  - name: unconfigured\n", "  - name: unconfigured\n    pagerduty_configs:\n      - service_key: abc\n")
        self.assertEqual(findings(rule(), alertmanager=am), {"ALERTMANAGER"})

    def test_placeholders_and_secret_files_are_allowed(self) -> None:
        am = ALERTMANAGER + "    #   - routing_key: <PAGER_INTEGRATION_KEY>\n    #     url_file: /run/secrets/ticket-url\n"
        self.assertEqual(findings(rule(), alertmanager=am), set())


class PromqlTests(unittest.TestCase):
    def test_metric_extraction_skips_functions_keywords_and_labels(self) -> None:
        expr = (
            'histogram_quantile(0.95, sum by (le, outcome) (rate(a_bucket{outcome=~"x|y"}[5m])))\n'
            "  > on() group_left 3 and sum without (instance) (increase(b_total[1h] offset 5m)) / "
            "(c_total or vector(0)) unless absent(d) > bool 1e3"
        )
        self.assertEqual(chk.metric_names(expr), {"a_bucket", "b_total", "c_total", "d"})

    def test_label_values_and_strings_are_not_metrics(self) -> None:
        self.assertEqual(chk.metric_names('max(m{status="DEAD_LETTER"}) > 0'), {"m"})


class YamlSubsetTests(unittest.TestCase):
    def test_block_style_document(self) -> None:
        doc = chk.parse_yaml(
            "a: 1  # comment\n"
            "b:\n  - x: 'q''s'\n    y: \"d \\\"q\\\"\"\n  - plain # c\n"
            "c: [m, 'n', \"o\"]\n"
            "d: |\n  line1\n    line2\n"
            "e: []\n"
            "f:\n- one\n"
        )
        self.assertEqual(
            doc,
            {"a": "1", "b": [{"x": "q's", "y": 'd "q"'}, "plain"], "c": ["m", "n", "o"],
             "d": "line1\n  line2\n", "e": [], "f": ["one"]},
        )

    def test_unsupported_constructs_are_reported(self) -> None:
        for text in ("a: {b: c}\n", "a: &x 1\n", "a: 1\na: 2\n"):
            with self.assertRaises(chk.YamlError, msg=text):
                chk.parse_yaml(text)


if __name__ == "__main__":
    unittest.main()
