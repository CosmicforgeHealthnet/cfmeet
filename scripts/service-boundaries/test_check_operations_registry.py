"""Mutation tests for the operations registry check: the real registry passes; a missing
required responsibility, a renamed entry point, a deleted proving test, an unknown owner
and a duplicated responsibility each fail.

Run: python3 -m unittest discover -s scripts/service-boundaries -v
"""

from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import check_operations_registry as registry_check  # noqa: E402

REPO = HERE.parents[1]
REGISTRY = json.loads((HERE / "operations_registry.json").read_text(encoding="utf-8"))


class OperationsRegistry(unittest.TestCase):
    def run_with(self, mutate) -> list[str]:
        data = json.loads(json.dumps(REGISTRY))
        mutate(data)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "registry.json"
            path.write_text(json.dumps(data), encoding="utf-8")
            return registry_check.check(REPO, path)

    def item(self, data, rid):
        return next(i for i in data["responsibilities"] if i["id"] == rid)

    def test_the_real_registry_is_complete(self):
        self.assertEqual(registry_check.check(REPO, HERE / "operations_registry.json"), [])

    def test_a_required_responsibility_without_an_owner_fails(self):
        found = self.run_with(lambda d: d.update(responsibilities=[i for i in d["responsibilities"] if i["id"] != "telephony-reconciler"]))
        self.assertTrue(any("telephony-reconciler" in e for e in found), found)

    def test_a_renamed_entry_point_fails(self):
        found = self.run_with(lambda d: self.item(d, "call-timeout").update(entry="expire_stalled_setups_renamed"))
        self.assertTrue(any("entry point" in e for e in found), found)

    def test_a_deleted_proving_test_fails(self):
        found = self.run_with(lambda d: self.item(d, "relay-expiry")["tests"].append("tests/expiry.rs::no_such_test"))
        self.assertTrue(any("no_such_test" in e for e in found), found)

    def test_an_unknown_owner_fails(self):
        found = self.run_with(lambda d: self.item(d, "usage-finalizer").update(owner="operations"))
        self.assertTrue(any("services/operations" in e for e in found), found)

    def test_a_responsibility_with_two_owners_fails(self):
        def dup(d):
            copy = dict(self.item(d, "webhook-delivery-worker"))
            copy["owner"] = "telephony"
            d["responsibilities"].append(copy)
        found = self.run_with(dup)
        self.assertTrue(any("listed twice" in e for e in found), found)


if __name__ == "__main__":
    unittest.main()
