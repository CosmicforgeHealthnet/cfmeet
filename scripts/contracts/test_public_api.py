"""Mutation tests of the public catalogue generator's route checks (Part VI §16)."""
import copy
import re
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

    def test_the_user_token_scheme_maps_to_the_gateway_user_token_mode(self):
        # ADR 0062 / build contract §9: devices present `Authorization: Bearer cfut_…`.
        self.assertEqual(public_api.AUTH_MODES["userToken"], "USER_TOKEN")
        doc = {"security": [{"platformCredential": []}]}
        op = {"operationId": "joinConference",
              "security": [{"userToken": []}, {"bearer_auth": []}, {"userToken": []}]}
        self.assertEqual(public_api.auth_modes("meetings", op, doc), ["USER_TOKEN", "MEETING_SESSION"])
        self.assertNotIn("userToken", public_api.INTERNAL_SCHEMES)

    def test_a_gate_scope_must_be_a_platform_access_registry_scope(self):
        registry = public_api.registered_scopes()
        self.assertIn("privacy.contacts.write", registry)
        public_api.check_required_scope("privacy-relay.ok", "privacy.contacts.write", registry)
        for bad in ("each requested privacy.contacts.* scope", "privacy.contacts.*", "", None, ["calls.write"]):
            with self.assertRaises(public_api.CatalogueError, msg=repr(bad)):
                public_api.check_required_scope("privacy-relay.bad", bad, registry)

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


def meetings_route(method, path, auth, scope="meetings.read", **extra):
    r = {"id": f"meetings.{extra.pop('op', 'op')}", "domain": "meetings", "method": method, "pattern": path,
         "protocol": "REST", "upstream": "cfmeet.meeting-service", "upstreamPath": path, "auth": auth,
         "forwardHeaders": ["x-client-kind"], "requiredScope": scope}
    r.update(extra)
    return r


def machine_operations():
    """(METHOD, OpenAPI path) of the Meeting Service machine-operation table."""
    text = (public_api.ROOT / "services/meeting-service/src/services/platform/operations.rs").read_text(encoding="utf-8")
    return set(re.findall(r'\("([A-Z]+)", "[^"]+", "(/api/v2/[^"]+)", scopes::', text))


class MeetingsPlatformAliases(unittest.TestCase):
    def test_a_platform_credential_meetings_route_gets_a_v1_meetings_alias(self):
        original = meetings_route("POST", "/api/v2/meeting-spaces/{id}/invitations/{invitationId}/revoke",
                                  ["MEETING_SESSION", "PLATFORM_CREDENTIAL"], "meetings.write",
                                  op="v2_revoke_invitation", timeoutMs=5000)
        alias = public_api.meetings_platform_alias(original, "v2_revoke_invitation")
        self.assertEqual(alias, {
            "id": "meetings.platform.v2_revoke_invitation", "domain": "meetings", "method": "POST",
            "pattern": "/v1/meetings/meeting-spaces/{id}/invitations/{invitationId}/revoke", "protocol": "REST",
            "upstream": "cfmeet.meeting-service",
            "upstreamPath": "/api/v2/meeting-spaces/{id}/invitations/{invitationId}/revoke",
            "auth": ["PLATFORM_CREDENTIAL"], "forwardHeaders": ["x-client-kind"],
            "aliasOf": "meetings.v2_revoke_invitation", "requiredScope": "meetings.write", "timeoutMs": 5000})
        self.assertEqual(original["pattern"], original["upstreamPath"], "the /api/v2 route is unchanged")
        self.assertEqual(original["auth"], ["MEETING_SESSION", "PLATFORM_CREDENTIAL"])
        public_api.check_aliases([original, alias])

    def test_no_alias_without_a_platform_credential_or_outside_the_meeting_service_api_v2(self):
        base = meetings_route("GET", "/api/v2/meeting-spaces", ["MEETING_SESSION", "PLATFORM_CREDENTIAL"])
        self.assertIsNotNone(public_api.meetings_platform_alias(base, "x"))
        for name, change in {
            "human only": {"auth": ["MEETING_SESSION", "PARTICIPANT_TOKEN"]},
            "user token": {"auth": ["USER_TOKEN"]},
            "other domain": {"domain": "communications"},
            "websocket": {"protocol": "WEBSOCKET"},
            "other upstream": {"upstream": "cfmeet.calling"},
            "already rewritten": {"upstreamPath": "/internal/v1/x"},
            "api v1": {"pattern": "/api/v1/meetings", "upstreamPath": "/api/v1/meetings"},
        }.items():
            self.assertIsNone(public_api.meetings_platform_alias({**base, **change}, "x"), name)

    def test_alias_checks_refuse_every_other_shape(self):
        original = meetings_route("GET", "/api/v2/meeting-spaces/{id}", ["MEETING_SESSION", "PLATFORM_CREDENTIAL"],
                                  op="v2_get_meeting_space")
        alias = public_api.meetings_platform_alias(original, "v2_get_meeting_space")
        public_api.check_aliases([original, alias])
        for name, change in {
            "dangling": {"aliasOf": "meetings.missing"},
            "wider auth": {"auth": ["PLATFORM_CREDENTIAL", "MEETING_SESSION"]},
            "other method": {"method": "PATCH"},
            "other upstream path": {"upstreamPath": "/api/v2/conferences/{id}"},
            "other public path": {"pattern": "/v1/meetings/spaces/{id}"},
            "outside the family": {"pattern": "/v1/meeting-spaces/{id}"},
            "other id": {"id": "meetings.v2_get_meeting_space_alias"},
        }.items():
            with self.assertRaises(public_api.CatalogueError, msg=name):
                public_api.check_aliases([original, {**alias, **change}])
        with self.assertRaises(public_api.CatalogueError, msg="alias of an alias"):
            public_api.check_aliases([original, alias, {**alias, "id": "meetings.platform.again", "aliasOf": alias["id"]}])

    def test_the_public_meetings_document_lists_the_alias_with_its_route_id_and_platform_security(self):
        routes = []
        for method, path, op in (("GET", "/api/v2/meeting-spaces", "v2_list_meeting_spaces"),
                                 ("POST", "/api/v2/meeting-spaces", "v2_create_meeting_space"),
                                 ("GET", "/api/v2/me/profile", "x")):
            auth = ["MEETING_SESSION"] if op == "x" else ["MEETING_SESSION", "PLATFORM_CREDENTIAL"]
            r = meetings_route(method, path, auth, op=op)
            routes.append(r)
            alias = public_api.meetings_platform_alias(r, op)
            if alias:
                routes.append(alias)
        doc = public_api.public_doc("meetings", {"routes": routes})
        # A human-only operation (/api/v2/me/profile) has no partner alias.
        self.assertEqual(sorted(doc["paths"]), ["/api/v2/me/profile", "/api/v2/meeting-spaces",
                                                "/v1/meetings/meeting-spaces"])
        item = doc["paths"]["/v1/meetings/meeting-spaces"]
        self.assertEqual(sorted(item), ["get", "post"])
        get = item["get"]
        self.assertEqual(get["operationId"], "platform_v2_list_meeting_spaces")
        self.assertEqual(get["x-cfmeet-route-id"], "meetings.platform.v2_list_meeting_spaces")
        self.assertEqual(get["x-cfmeet-auth"], ["PLATFORM_CREDENTIAL"])
        self.assertEqual(get["x-cfmeet-alias-of"], "meetings.v2_list_meeting_spaces")
        self.assertEqual([list(alt) for alt in get["security"]], [["platformCredential"]])
        self.assertIn("GET /api/v2/meeting-spaces", get["description"])
        first_party = doc["paths"]["/api/v2/meeting-spaces"]["get"]
        self.assertEqual(first_party["operationId"], "v2_list_meeting_spaces")
        self.assertEqual(first_party["x-cfmeet-route-id"], "meetings.v2_list_meeting_spaces")
        self.assertIn("bearer_auth", {k for alt in first_party["security"] for k in alt})
        # Deterministic, and an alias without its contract operation fails the build.
        self.assertEqual(public_api.render(doc), public_api.render(public_api.public_doc("meetings", {"routes": routes})))
        orphan = {**routes[1], "id": "meetings.platform.nope", "method": "DELETE", "aliasOf": routes[0]["id"]}
        with self.assertRaises(public_api.CatalogueError):
            public_api.public_doc("meetings", {"routes": routes + [orphan]})
        agg = public_api.aggregate({"meetings": doc})
        self.assertIn("/v1/meetings/meeting-spaces", agg["paths"])

    def test_the_catalogue_aliases_exactly_the_meeting_service_machine_operations(self):
        manifest = public_api.build()
        aliases = [r for r in manifest["routes"] if "aliasOf" in r]
        machine = machine_operations()
        self.assertEqual(len(machine), 21)
        self.assertEqual({(r["method"], r["upstreamPath"]) for r in aliases}, machine)
        by_id = {r["id"]: r for r in manifest["routes"]}
        for a in aliases:
            target = by_id[a["aliasOf"]]
            self.assertEqual(a["id"], "meetings.platform." + target["id"].split(".", 1)[1])
            self.assertEqual(a["pattern"], "/v1/meetings" + target["pattern"][len("/api/v2"):])
            self.assertEqual(a["auth"], ["PLATFORM_CREDENTIAL"])
            self.assertIn(a["requiredScope"], ("meetings.read", "meetings.write"))
            self.assertEqual(a["requiredScope"], target["requiredScope"])
            self.assertIn("PLATFORM_CREDENTIAL", target["auth"])
            self.assertEqual(target["upstreamPath"], target["pattern"], "the first-party route is unchanged")
        self.assertEqual(manifest, public_api.build(), "deterministic")


if __name__ == "__main__":
    unittest.main()
