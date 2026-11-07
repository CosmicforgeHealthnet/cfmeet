"""Tests of the FE-018 first-party ingress list generator and the FE-015 web parity report."""
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import first_party_ingress as gen  # noqa: E402
import web_contract_parity_report as report  # noqa: E402

PUBLIC = {
    "reasons": {"LEGACY_DIRECT_INGRESS": "legacy", "CONTACT_VAULT_PRIVATE": "vault"},
    "operations": [
        {"domain": "meetings", "method": "GET", "path": "/api/v1/meetings", "class": "EXCLUDED", "reason": "LEGACY_DIRECT_INGRESS"},
        {"domain": "privacy-relay", "method": "POST", "path": "/v1/privacy-relay/contacts", "class": "EXCLUDED", "reason": "CONTACT_VAULT_PRIVATE"},
        {"domain": "meetings", "method": "GET", "path": "/api/v2/meeting-spaces", "class": "PUBLIC_CUSTOMER"},
    ],
}
MANIFEST = {
    "routes": [{"method": "GET", "pattern": "/api/v2/meeting-spaces"}],
    "catalogue": [
        {"domain": "meetings", "contract": "c.meet", "operationId": "list_meetings", "method": "GET", "path": "/api/v1/meetings", "class": "EXCLUDED", "reason": "LEGACY_DIRECT_INGRESS"},
        {"domain": "privacy-relay", "contract": "c.relay", "operationId": "createContact", "method": "POST", "path": "/v1/privacy-relay/contacts", "class": "EXCLUDED", "reason": "CONTACT_VAULT_PRIVATE"},
        {"domain": "meetings", "contract": "c.meet", "operationId": "v2_list", "method": "GET", "path": "/api/v2/meeting-spaces", "class": "PUBLIC_CUSTOMER"},
        {"domain": "meetings", "contract": "c.meet", "operationId": "v2_get", "method": "GET", "path": "/api/v2/meeting-spaces/{id}", "class": "PUBLIC_CUSTOMER"},
    ],
}


def copy(o):
    return json.loads(json.dumps(o))


class FirstPartyIngress(unittest.TestCase):
    def test_groups_by_reason(self):
        groups, errors = gen.collect(PUBLIC, MANIFEST)
        self.assertEqual(errors, [])
        self.assertEqual({k: len(v) for k, v in groups.items()}, {"CONTACT_VAULT_PRIVATE": 1, "LEGACY_DIRECT_INGRESS": 1})
        self.assertEqual(groups["CONTACT_VAULT_PRIVATE"][0]["operationId"], "createContact")

    def test_undefined_reason_rejected(self):
        p = copy(PUBLIC)
        p["operations"][0]["reason"] = "MADE_UP"
        self.assertTrue(any("without a defined reason" in e for e in gen.collect(p, MANIFEST)[1]))

    def test_excluded_but_routed_rejected(self):
        m = copy(MANIFEST)
        m["routes"].append({"method": "GET", "pattern": "/api/v1/meetings"})
        self.assertTrue(any("routed by the gateway" in e for e in gen.collect(PUBLIC, m)[1]))

    def test_catalogue_disagreement_rejected(self):
        m = copy(MANIFEST)
        m["catalogue"][0]["reason"] = "CONTACT_VAULT_PRIVATE"
        self.assertTrue(any("in the catalogue" in e for e in gen.collect(PUBLIC, m)[1]))
        m = copy(MANIFEST)
        m["catalogue"].append({"method": "DELETE", "path": "/x", "class": "EXCLUDED", "reason": "LEGACY_DIRECT_INGRESS"})
        self.assertTrue(any("not in public-routes.json" in e for e in gen.collect(PUBLIC, m)[1]))

    def test_render_and_splice(self):
        groups, _ = gen.collect(PUBLIC, MANIFEST)
        block = gen.render(PUBLIC, groups)
        self.assertIn("| **Total** | **2** |", block)
        doc = f"# t\n\n{gen.BEGIN}\nold\n{gen.END}\ntail\n"
        out = gen.splice(doc, block)
        self.assertTrue(out.startswith("# t\n\n" + gen.BEGIN))
        self.assertTrue(out.endswith(gen.END + "\ntail\n"))
        self.assertNotIn("old", out)
        self.assertIsNone(gen.splice("no markers", block))

    def test_repository_record_is_current(self):
        self.assertEqual(gen.main(["first_party_ingress.py", "--check"]), 0)


class WebParityReport(unittest.TestCase):
    def test_classifies_calls_and_never_fails(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "src/services").mkdir(parents=True)
            (root / "src/types").mkdir(parents=True)
            (root / "src/types/meeting.ts").write_text("export interface M { id: string }\n")
            (root / "src/services/m.ts").write_text(
                "await apiV2.get<X>(`/meeting-spaces/${id}`);\n"
                "await api.get(\"/meetings\");\n"
                "await apiV2.post('/nowhere');\n"
                "await other.get('/ignored');\n"
            )
            index = report.catalogue_index(MANIFEST)
            self.assertEqual(report.match(index, "GET", "/api/v2/meeting-spaces/{}")["operationId"], "v2_get")
            self.assertEqual(report.match(index, "GET", "/api/v2/meeting-spaces")["operationId"], "v2_list")
            r = report.scan(root)
            by_path = {c["path"]: c for c in r["calls"]}
            self.assertEqual(by_path["/api/v2/meeting-spaces/{}"]["bucket"], "PUBLIC_CUSTOMER")
            self.assertEqual(by_path["/api/v1/meetings"]["bucket"], "EXCLUDED")
            self.assertEqual(by_path["/api/v2/nowhere"]["bucket"], "UNMATCHED")
            self.assertNotIn("/ignored", by_path)
            self.assertEqual(r["handwrittenTypeModules"], ["src/types/meeting.ts"])
            self.assertEqual(r["sdkImports"], [])
            self.assertIn("does **not** import", report.markdown(r))
            env = os.environ.pop("GITHUB_STEP_SUMMARY", None)
            try:
                self.assertEqual(report.main(["r", "--web-root", str(root)]), 0)
                self.assertEqual(report.main(["r", "--web-root", str(root / "missing")]), 0)
            finally:
                if env is not None:
                    os.environ["GITHUB_STEP_SUMMARY"] = env

    def test_sdk_import_detected(self):
        self.assertTrue(report.SDK_IMPORT.search("import { Meetings } from '@cfmeet/platform-sdk';"))
        self.assertTrue(report.SDK_IMPORT.search('import type { X } from "../../sdks/typescript/src/generated/models";'))
        self.assertFalse(report.SDK_IMPORT.search('import axios from "axios";'))


if __name__ == "__main__":
    unittest.main()
