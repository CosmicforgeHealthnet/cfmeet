// Organization-scope Messaging rules (final closure, J-10):
//   node --experimental-strip-types --test tests/organization-messaging.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import {
  addableColleagues,
  colleagueLabel,
  isOrganizationId,
  organizationMessagingBase,
  organizationMessagingProblem,
  organizationRelayPath,
} from "../src/lib/messaging/organization.ts";

const ORG = "0b5f8c9e-2a4d-4c1e-9f3a-7d6b5e4c3a21";

test("organization Messaging lives under the organization's own path, never the personal relay", () => {
  assert.equal(organizationMessagingBase(ORG), `/organizations/${ORG}/messaging`);
  assert.equal(organizationRelayPath(ORG, "v1/conversations"), `/organizations/${ORG}/messaging/relay/v1/conversations`);
  for (const bad of ["not-a-uuid", "", `${ORG}/../x`, ORG.toUpperCase()]) {
    assert.equal(isOrganizationId(bad), false, bad);
    assert.throws(() => organizationMessagingBase(bad));
  }
  for (const path of ["/v1/conversations", "v1/../admin", "v2/conversations", "admin/policy"]) {
    assert.throws(() => organizationRelayPath(ORG, path), path);
  }
});

test("server refusals are explained, never shown as raw codes", () => {
  assert.match(organizationMessagingProblem("ORGANIZATION_POLICY_RESTRICTED"), /turned off by your organization/);
  assert.match(organizationMessagingProblem("FEATURE_DISABLED"), /isn't set up/);
  assert.match(organizationMessagingProblem("ORGANIZATION_SUSPENDED"), /suspended/);
  assert.match(organizationMessagingProblem("MEMBERSHIP_REQUIRED"), /no longer a member/);
  assert.match(organizationMessagingProblem("ORGANIZATION_NOT_FOUND"), /no longer a member/);
  assert.match(organizationMessagingProblem(null, 401), /session has expired/);
  assert.equal(organizationMessagingProblem("SOMETHING_ELSE"), null);
});

test("only other ACTIVE members can be added; labels never fall back to an e-mail", () => {
  const me = "u-me";
  const members = [
    { userId: me, displayName: "Me", status: "ACTIVE" },
    { userId: "u-a", displayName: "Ada", status: "ACTIVE" },
    { userId: "u-b", username: "bola", status: "SUSPENDED" },
    { userId: "u-c", username: "chi" },
  ];
  assert.deepEqual(addableColleagues(members, me).map((m) => m.userId), ["u-a", "u-c"]);
  assert.equal(colleagueLabel({ userId: "x", displayName: null, username: "chi" }), "chi");
  assert.equal(colleagueLabel({ userId: "x" }), "Colleague");
});
