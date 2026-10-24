"""Mutation tests of the public catalogue generator's route checks (Part VI §16)."""
import copy
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import public_api  # noqa: E402


def route(rid, **extra):
    r = {"id": rid, "domain": rid.split(".")[0], "method": "GET", "pattern": "/v1/" + rid}
    r.update(extra)
    return r


class RouteChecks(unittest.TestCase):
    def test_the_generated_catalogue_builds(self):
        manifest = public_api.build()
        ids = [r["id"] for r in manifest["routes"]]
        self.assertEqual(len(ids), len(set(ids)), "route ids are unique")
        self.assertTrue(all(not r["pattern"].startswith("/internal") for r in manifest["routes"]))

    def test_a_deprecated_route_needs_a_live_public_replacement(self):
        ok = [route("calling.oldUsage", deprecated=True, replacedBy="developer-platform.usage"),
              route("developer-platform.usage")]
        public_api.check_replacements(ok)
        for bad in (
            [route("a.old", deprecated=True, replacedBy="a.missing")],
            [route("a.old", deprecated=True, replacedBy="a.old")],
            [route("a.old", deprecated=True, replacedBy="a.mid"),
             route("a.mid", deprecated=True, replacedBy="a.new"), route("a.new")],
        ):
            with self.assertRaises(public_api.CatalogueError):
                public_api.check_replacements(copy.deepcopy(bad))

    def test_operations_are_checked_for_ids_and_deprecation_metadata(self):
        original = public_api.operations

        def patched(mutate):
            def ops():
                for item in original():
                    domain, service, rel, cid, method, path, op, doc = item
                    if domain == "developer-platform" and op.get("operationId") == "listScopeRegistry":
                        op = mutate(copy.deepcopy(op))
                    yield domain, service, rel, cid, method, path, op, doc
            return ops

        cases = {
            "missing operationId": lambda op: {k: v for k, v in op.items() if k != "operationId"},
            "duplicate route id": lambda op: {**op, "operationId": "listOrganizations"},
            "deprecated without replacement": lambda op: {**op, "deprecated": True},
            "replacement on a live operation": lambda op: {**op, "x-cfmeet-replaced-by": "developer-platform.getProject"},
            "dangling replacement": lambda op: {**op, "deprecated": True, "x-cfmeet-replaced-by": "developer-platform.nope"},
        }
        try:
            for name, mutate in cases.items():
                public_api.operations = patched(mutate)
                with self.assertRaises(public_api.CatalogueError, msg=name):
                    public_api.build()
            public_api.operations = patched(lambda op: {**op, "deprecated": True,
                                                        "x-cfmeet-replaced-by": "developer-platform.listProjectScopes"})
            r = next(r for r in public_api.build()["routes"] if r["id"] == "developer-platform.listScopeRegistry")
            self.assertEqual((r["deprecated"], r["replacedBy"]), (True, "developer-platform.listProjectScopes"))
        finally:
            public_api.operations = original


if __name__ == "__main__":
    unittest.main()
