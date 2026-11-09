"""Mutation tests of the operation test-reference coverage gate (INV-002)."""
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import operation_test_coverage as cov  # noqa: E402

CONTRACT = {
    "openapi": "3.1.0",
    "paths": {
        "/": {"get": {}},
        "/v1/things": {"get": {}, "post": {}},
        "/v1/things/{thingId}": {"get": {}, "delete": {}},
        "/v1/things/{thingId}/parts/{partId}": {"patch": {}},
        "/v1/other": {"get": {}},
    },
}
MIRROR = ("svc.http", "/internal/v1/delegated", ("/v1/things",), "services/svc/src/http.rs", 'nest("/delegated"')


class Repo:
    def __init__(self, tests: dict[str, str], router: str = '.nest("/delegated", d)'):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        contracts = root / "packages/contracts"
        (contracts / "svc").mkdir(parents=True)
        (contracts / "svc/v1.openapi.json").write_text(json.dumps(CONTRACT), encoding="utf-8")
        (contracts / "bundled/svc").mkdir(parents=True)
        (contracts / "bundled/svc/v1.openapi.json").write_text(json.dumps(CONTRACT), encoding="utf-8")
        (contracts / "manifest.json").write_text(json.dumps({"contracts": [
            {"id": "svc.http", "kind": "openapi", "owner": "svc", "path": "svc/v1.openapi.json"},
            {"id": "svc.http-bundled", "kind": "openapi", "owner": "svc", "path": "bundled/svc/v1.openapi.json"},
            {"id": "svc.schema", "kind": "json-schema", "owner": "svc", "path": "svc/v1.schema.json"},
        ]}), encoding="utf-8")
        (root / "services/svc/src").mkdir(parents=True)
        (root / "services/svc/src/http.rs").write_text(router, encoding="utf-8")
        for rel, text in tests.items():
            (root / rel).parent.mkdir(parents=True, exist_ok=True)
            (root / rel).write_text(text, encoding="utf-8")
        self.root = root

    def untested(self, key: str = "svc.http", mirrors=(MIRROR,), sweeps=None):
        report, errors = cov.analyse(self.root, mirrors=mirrors, sweeps=sweeps or {})
        self.errors = errors
        self.report = report
        return {f"{m} {p}" for m, p in report[key]["untested"]}

    def close(self):
        self.tmp.cleanup()


ALL = {
    "GET /", "GET /v1/things", "POST /v1/things", "GET /v1/things/{thingId}",
    "DELETE /v1/things/{thingId}", "PATCH /v1/things/{thingId}/parts/{partId}", "GET /v1/other",
}


class Matching(unittest.TestCase):
    def check(self, tests: dict[str, str], expected_untested: set[str], **kw):
        repo = Repo(tests)
        try:
            self.assertEqual(repo.untested(**kw), expected_untested)
            return repo
        finally:
            repo.close()

    def test_no_tests_means_everything_untested_and_bundles_ignored(self):
        repo = Repo({})
        try:
            self.assertEqual(repo.untested(), ALL)
            self.assertNotIn("svc.http-bundled", repo.report)
            self.assertNotIn("svc.schema", repo.report)
        finally:
            repo.close()

    def test_literals_with_placeholders_concrete_ids_and_queries_match(self):
        self.check(
            {"services/svc/tests/a.rs": '''
                app.get("/").await;
                app.get("/v1/things?limit=5").await;
                app.get(format!("/v1/things/{}", id)).await;
                app.patch(&format!("/v1/things/{t}/parts/{p}"), body).await;
            '''},
            {"GET /v1/other"},
        )

    def test_js_template_and_full_url_and_method_prefix_match(self):
        self.check(
            {"scripts/staging/run.mjs": "await fetch(`${base}/x`); await fetch(`https://api.example.com/v1/other`);",
             "certification/tests/x.rs": 'let line = "DELETE /v1/things/3f1e6a9c-1b2d-4e5f-8a9b-0c1d2e3f4a5b";'},
            ALL - {"GET /v1/other", "GET /v1/things/{thingId}", "DELETE /v1/things/{thingId}"},
        )

    def test_static_segment_mismatch_and_length_mismatch_do_not_match(self):
        self.check(
            {"services/svc/tests/a.rs": '"/v1/thing", "/v1/things/x/parts", "/v1/things/a/b/c/d", "/v2/other"'},
            ALL,
        )

    def test_placeholder_never_matches_a_static_segment(self):
        self.check(
            {"services/svc/tests/a.rs": 'format!("/v1/{kind}")'},
            ALL,
        )

    def test_src_files_are_not_tests(self):
        self.check({"services/svc/src/routes.rs": '.route("/v1/other", get(h))'}, ALL)

    def test_placeholder_led_suffix_counts_only_in_the_owner_tests(self):
        self.check({"services/svc/tests/a.rs": 'format!("{base}/other")'}, ALL - {"GET /v1/other"})
        self.check({"services/elsewhere/tests/a.rs": 'format!("{base}/other")'}, ALL)
        self.check({"certification/tests/a.rs": 'format!("{base}/other")'}, ALL)

    def test_trailing_slash_literal_is_a_prefix_for_one_parameter(self):
        self.check(
            {"services/svc/tests/a.rs": 'let u = "/v1/things/".to_string() + &id;'},
            ALL - {"GET /v1/things/{thingId}", "DELETE /v1/things/{thingId}"},
        )


class Mirrors(unittest.TestCase):
    MIRRORED = {
        "GET /internal/v1/delegated/v1/things", "POST /internal/v1/delegated/v1/things",
        "GET /internal/v1/delegated/v1/things/{thingId}", "DELETE /internal/v1/delegated/v1/things/{thingId}",
        "PATCH /internal/v1/delegated/v1/things/{thingId}/parts/{partId}",
    }

    def test_mirror_needs_its_own_full_path(self):
        repo = Repo({"services/svc/tests/a.rs": '"/v1/things", "/internal/v1/delegated/v1/things/{id}"'})
        try:
            self.assertEqual(
                repo.untested(cov.mirror_key("svc.http")),
                self.MIRRORED - {"GET /internal/v1/delegated/v1/things/{thingId}",
                                 "DELETE /internal/v1/delegated/v1/things/{thingId}"},
            )
            self.assertEqual(repo.errors, [])
        finally:
            repo.close()

    def test_mirror_suffix_requires_the_prefix_in_the_same_owner_file(self):
        with_prefix = Repo({"services/svc/tests/a.rs": 'let base = "/internal/v1/delegated/v1"; format!("{base}/things")'})
        without = Repo({"services/svc/tests/b.rs": 'format!("{base}/things")'})
        try:
            self.assertNotIn("GET /internal/v1/delegated/v1/things", with_prefix.untested(cov.mirror_key("svc.http")))
            self.assertIn("GET /internal/v1/delegated/v1/things", without.untested(cov.mirror_key("svc.http")))
        finally:
            with_prefix.close()
            without.close()

    def test_a_stale_mirror_rule_is_an_error(self):
        repo = Repo({}, router="Router::new()")
        try:
            report, errors = cov.analyse(repo.root, mirrors=(MIRROR,), sweeps={})
            self.assertTrue(any("no longer matches" in e for e in errors), errors)
            self.assertNotIn(cov.mirror_key("svc.http"), report)
        finally:
            repo.close()


class Sweeps(unittest.TestCase):
    def test_sweep_marks_operations_route_only(self):
        repo = Repo({"services/svc/tests/s.rs": "fn every_op_is_routed() {}"})
        try:
            untested = repo.untested(sweeps={"svc.http": ("services/svc/tests/s.rs", "fn every_op_is_routed")})
            self.assertEqual(untested, set())
            self.assertEqual(len(repo.report["svc.http"]["route_only"]), len(ALL))
        finally:
            repo.close()

    def test_missing_sweep_is_an_error(self):
        repo = Repo({})
        try:
            repo.untested(sweeps={"svc.http": ("services/svc/tests/s.rs", "fn gone")})
            self.assertTrue(any("sweep" in e for e in repo.errors), repo.errors)
        finally:
            repo.close()


class Gate(unittest.TestCase):
    def report(self, n: int):
        return {"svc.http": {"owner": "svc", "total": 9, "untested": [("GET", f"/p{i}") for i in range(n)], "route_only": []}}

    def test_rise_above_baseline_fails(self):
        failures, _ = cov.gate(self.report(3), {"svc.http": 2})
        self.assertEqual(len(failures), 1)
        self.assertIn("GET /p2", failures[0])

    def test_equal_passes_and_decrease_hints(self):
        self.assertEqual(cov.gate(self.report(2), {"svc.http": 2}), ([], []))
        failures, hints = cov.gate(self.report(1), {"svc.http": 2})
        self.assertEqual(failures, [])
        self.assertEqual(len(hints), 1)

    def test_unknown_contract_with_untested_operations_fails(self):
        failures, _ = cov.gate(self.report(1), {})
        self.assertEqual(len(failures), 1)
        self.assertEqual(cov.gate(self.report(0), {}), ([], []))

    def test_stale_baseline_entry_fails(self):
        failures, _ = cov.gate(self.report(0), {"svc.http": 0, "gone": 1})
        self.assertTrue(any("gone" in f for f in failures))


class RealRepository(unittest.TestCase):
    def test_the_repository_passes_its_frozen_baseline(self):
        report, errors = cov.analyse(cov.ROOT)
        self.assertEqual(errors, [])
        failures, _ = cov.gate(report, cov.BASELINE)
        self.assertEqual(failures, [])

    def test_every_operation_is_tested_and_the_baseline_is_empty(self):
        # INV-002 closed: no contract may carry untested operations, so nothing is frozen.
        self.assertEqual(cov.BASELINE, {})
        report, _ = cov.analyse(cov.ROOT)
        untested = {k: v["untested"] for k, v in report.items() if v["untested"]}
        self.assertEqual(untested, {})

    def test_removing_a_test_reference_raises_a_count_above_the_baseline(self):
        # Single mutation of the real corpus: drop every test reference to one tested
        # meeting-service path and the gate must fail.
        report, _ = cov.analyse(cov.ROOT)
        key = "cfmeet.meeting-service.http-api"
        bumped = dict(cov.BASELINE)
        bumped[key] = len(report[key]["untested"]) - 1 if report[key]["untested"] else -1
        failures, _ = cov.gate(report, bumped)
        self.assertTrue(any(key in f for f in failures))


if __name__ == "__main__":
    unittest.main()
