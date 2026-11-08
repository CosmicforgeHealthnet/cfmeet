// Section 17 account + organization administration: capability gating (default deny, never a
// cached role), workspace context rules, stable error envelopes, optimistic-concurrency bodies.
//   node --experimental-strip-types --test tests/organization-admin.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import {
  ORG_CAPABILITIES,
  assignableRoles,
  canManageMember,
  invitableRoles,
  invitationTokenFromHash,
  legalVerificationLabel,
  lifecycleActions,
  memberRole,
  needsReload,
  organizationErrorText,
  organizationForm,
  organizationFormProblems,
  organizationPatch,
  organizationSections,
  orgCan,
  provisioningSettled,
  safeReturnPath,
  suggestSlug,
} from "../src/lib/organizations/organization-core.ts";
import {
  PERSONAL_WORKSPACE_ID,
  canAdministerOrganization,
  capabilityDecision,
  removedWorkspaceIds,
  resolveWorkspaceId,
  selectableWorkspaces,
  visibleNavigation,
} from "../src/lib/workspace/workspace-core.ts";
import {
  deactivationOutcome,
  normalizeAccountStatus,
  parseGoogleCallback,
  passwordProblem,
  profilePatch,
  signInNotice,
} from "../src/lib/account/account-core.ts";

const ORG = "11111111-1111-4111-8111-111111111111";
const OTHER = "22222222-2222-4222-8222-222222222222";
const viewer = (role, capabilities, status = "ACTIVE") => ({ viewer: { role, status, membershipId: "m-self", capabilities } });
const envelope = (status, code, message = "server text") => ({ response: { status, data: { success: false, error: { code, message } } } });

const org = (over = {}) => ({
  id: ORG,
  name: "Acme",
  slug: "acme",
  version: 7,
  lifecycleStatus: "ACTIVE",
  provisioningState: "READY",
  creationSource: "SELF_SERVICE",
  createdAt: "2026-01-01T00:00:00Z",
  updatedAt: "2026-01-01T00:00:00Z",
  profile: { city: "Leeds", countryCode: "GB", legalRegistration: { registeredName: "Acme Ltd", verification: "SELF_DECLARED" } },
  ...viewer("OWNER", Object.values(ORG_CAPABILITIES)),
  ...over,
});

test("controls follow the server's capabilities, never the role label (default deny)", () => {
  // A role that claims OWNER but carries no capability gets nothing.
  assert.equal(orgCan(viewer("OWNER", []), ORG_CAPABILITIES.lifecycleManage), false);
  assert.equal(orgCan(viewer("MEMBER", [ORG_CAPABILITIES.auditRead]), ORG_CAPABILITIES.auditRead), true);
  assert.equal(orgCan(viewer("OWNER", Object.values(ORG_CAPABILITIES), "SUSPENDED"), ORG_CAPABILITIES.read), false, "a suspended membership grants nothing");
  assert.equal(orgCan(null, ORG_CAPABILITIES.read), false);
  assert.equal(orgCan({ viewer: null }, ORG_CAPABILITIES.read), false);
});

test("administration sections are derived from capabilities", () => {
  const member = organizationSections(viewer("MEMBER", [ORG_CAPABILITIES.read, ORG_CAPABILITIES.membersRead]));
  assert.deepEqual(member, { overview: true, members: true, invitations: false, audit: false, settings: false, lifecycle: false, ownership: false });
  const manager = organizationSections(viewer("MANAGER", [ORG_CAPABILITIES.read, ORG_CAPABILITIES.membersRead, ORG_CAPABILITIES.membersInvite]));
  assert.equal(manager.invitations, true);
  assert.equal(manager.settings, false);
  const owner = organizationSections(org());
  assert.ok(Object.values(owner).every(Boolean));
  assert.deepEqual(Object.values(organizationSections(null)).filter(Boolean), []);
});

test("role options mirror the documented rules (OWNER any; ADMIN below ADMIN; invitations never OWNER)", () => {
  assert.deepEqual(assignableRoles("OWNER", "ADMIN"), ["OWNER", "ADMIN", "MANAGER", "MEMBER", "GUEST"]);
  assert.deepEqual(assignableRoles("ADMIN", "MEMBER"), ["MANAGER", "MEMBER", "GUEST"]);
  assert.deepEqual(assignableRoles("ADMIN", "ADMIN"), []);
  assert.deepEqual(assignableRoles("ADMIN", "OWNER"), []);
  assert.deepEqual(assignableRoles("MANAGER", "MEMBER"), []);
  assert.deepEqual(invitableRoles("OWNER"), ["ADMIN", "MANAGER", "MEMBER", "GUEST"]);
  assert.deepEqual(invitableRoles("ADMIN"), ["MANAGER", "MEMBER", "GUEST"]);
  assert.deepEqual(invitableRoles("MANAGER"), ["MEMBER", "GUEST"]);
  assert.deepEqual(invitableRoles("MEMBER"), []);
});

test("member rows: manage controls need members.manage, a version, and never the caller's own row", () => {
  const admin = viewer("ADMIN", [ORG_CAPABILITIES.membersManage]);
  const row = { id: "m-2", role: "MEMBER", organizationRole: "MANAGER", version: 3 };
  assert.equal(memberRole(row), "MANAGER", "organizationRole wins over the legacy role");
  assert.equal(canManageMember(admin, row), true);
  assert.equal(canManageMember(admin, { ...row, id: "m-self" }), false);
  assert.equal(canManageMember(admin, { ...row, version: null }), false, "legacy items cannot be PATCHed");
  assert.equal(canManageMember(admin, { ...row, organizationRole: "OWNER" }), false);
  assert.equal(canManageMember(viewer("ADMIN", []), row), false);
});

test("stable error envelopes map to clear text", () => {
  for (const code of ["INVITE_EXPIRED", "MEMBERSHIP_REQUIRED", "LAST_OWNER", "ORGANIZATION_SUSPENDED", "VERSION_CONFLICT", "PROVISIONING_PENDING"]) {
    const text = organizationErrorText(envelope(409, code));
    assert.ok(text && text !== "server text", code);
  }
  assert.match(organizationErrorText(envelope(409, "LAST_OWNER")), /owner/i);
  assert.equal(organizationErrorText(envelope(400, "VALIDATION_ERROR", "slug must be 3-63")), "slug must be 3-63");
  assert.equal(organizationErrorText(envelope(418, "SOMETHING_NEW", "Fresh message")), "Fresh message");
  assert.match(organizationErrorText(new Error("offline")), /offline/i);
  assert.equal(needsReload(envelope(409, "VERSION_CONFLICT")), true);
  assert.equal(needsReload(envelope(409, "LAST_OWNER")), false);
});

test("organization PATCH carries expectedVersion and only what changed", () => {
  const o = org();
  assert.equal(organizationPatch(o, organizationForm(o)), null, "nothing changed");
  const form = { ...organizationForm(o), name: "Acme Group", city: "" };
  const patch = organizationPatch(o, form);
  assert.equal(patch.expectedVersion, 7);
  assert.equal(patch.name, "Acme Group");
  assert.equal(patch.slug, undefined);
  assert.equal(patch.profile.city, null, "an emptied field is cleared with null");
  assert.equal(patch.profile.legalRegistration.registeredName, "Acme Ltd");
  assert.equal("verification" in patch.profile.legalRegistration, false, "verification is read-only, never sent");
});

test("organization form bounds", () => {
  const ok = organizationForm(org());
  assert.deepEqual(organizationFormProblems(ok), {});
  const bad = organizationFormProblems({ ...ok, slug: "-x", countryCode: "GBR", publicContactEmail: "nope", incorporatedOn: "01/02/2020" });
  assert.deepEqual(Object.keys(bad).sort(), ["countryCode", "incorporatedOn", "publicContactEmail", "slug"]);
  assert.equal(suggestSlug("  Ünïcode & Co. Ltd  "), "unicode-co-ltd");
});

test("ORG-003 labels: legal fields are self-declared unless verified; lifecycle and provisioning", () => {
  assert.equal(legalVerificationLabel("SELF_DECLARED"), "Self-declared (not verified)");
  assert.equal(legalVerificationLabel(null), "Self-declared (not verified)");
  assert.equal(legalVerificationLabel("VERIFIED"), "Verified by CF Meet");
  assert.deepEqual(lifecycleActions("ACTIVE"), ["suspend"]);
  assert.deepEqual(lifecycleActions("SUSPENDED"), ["reactivate", "archive"]);
  assert.deepEqual(lifecycleActions("ARCHIVED"), []);
  assert.equal(provisioningSettled("IN_PROGRESS"), false);
  assert.equal(provisioningSettled("READY"), true);
});

test("invitation tokens come only from the fragment and are bounded; return paths are allow-listed", () => {
  assert.equal(invitationTokenFromHash("#abc_DEF-123"), "abc_DEF-123");
  assert.equal(invitationTokenFromHash(""), null);
  assert.equal(invitationTokenFromHash("#<script>"), null);
  assert.equal(invitationTokenFromHash(`#${"a".repeat(600)}`), null);
  assert.equal(safeReturnPath("/organizations/invitations"), "/organizations/invitations");
  assert.equal(safeReturnPath("/dashboard/organizations"), "/dashboard/organizations");
  assert.equal(safeReturnPath("//evil.example"), null);
  assert.equal(safeReturnPath("https://evil.example"), null);
  assert.equal(safeReturnPath("/login?x=1"), null);
});

// ---- workspace context ------------------------------------------------------------------

const personal = { id: PERSONAL_WORKSPACE_ID, kind: "PERSONAL", displayName: "Personal" };
const active = { id: ORG, kind: "ORGANIZATION", organizationId: ORG, displayName: "Acme", organizationStatus: "ACTIVE" };
const suspended = { id: OTHER, kind: "ORGANIZATION", organizationId: OTHER, displayName: "Beta", organizationStatus: "SUSPENDED" };

test("workspace selection is per tab and falls back to Personal when the membership is gone", () => {
  assert.deepEqual(selectableWorkspaces([personal, active, suspended]).map((w) => w.id), [PERSONAL_WORKSPACE_ID, ORG]);
  assert.equal(resolveWorkspaceId(ORG, [personal, active]), ORG);
  assert.equal(resolveWorkspaceId(ORG, null), ORG, "while the list loads the tab keeps its choice");
  assert.equal(resolveWorkspaceId(ORG, [personal]), PERSONAL_WORKSPACE_ID);
  assert.equal(resolveWorkspaceId(OTHER, [personal, suspended]), PERSONAL_WORKSPACE_ID, "suspended organizations cannot be selected");
  assert.equal(resolveWorkspaceId("javascript:alert(1)", [personal]), PERSONAL_WORKSPACE_ID);
  assert.deepEqual(removedWorkspaceIds([personal, active], [personal]), [ORG], "its cached data is dropped");
  assert.deepEqual(removedWorkspaceIds(null, [personal]), []);
});

const caps = (granted, features = { meetings: true, calling: true, messaging: true }) => ({
  kind: "ORGANIZATION",
  workspaceId: ORG,
  features,
  capabilities: granted.map((c) => ({ capability: c, granted: true, reason: "ALLOWED" })),
});

test("navigation and 'Manage organization' are capability-driven", () => {
  const items = [
    { label: "Meetings", feature: "meetings" },
    { label: "Calls", feature: "calling" },
    { label: "Admin", capability: "organization.profile.write" },
    { label: "Org only", organizationOnly: true },
  ];
  assert.deepEqual(visibleNavigation(items, { kind: "PERSONAL", capabilities: null }).map((i) => i.label), ["Meetings", "Calls"]);
  const off = caps([], { meetings: true, calling: false, messaging: true });
  assert.deepEqual(visibleNavigation(items, { kind: "ORGANIZATION", capabilities: off }).map((i) => i.label), ["Meetings", "Org only"]);
  const admin = caps(["organization.profile.write"]);
  assert.ok(visibleNavigation(items, { kind: "ORGANIZATION", capabilities: admin }).some((i) => i.label === "Admin"));
  assert.equal(capabilityDecision(admin, "unknown.capability").granted, false);
  assert.equal(canAdministerOrganization(admin), true);
  assert.equal(canAdministerOrganization(caps(["organization.read"])), false);
  assert.equal(canAdministerOrganization({ ...admin, kind: "PERSONAL" }), false);
  assert.equal(canAdministerOrganization(null), false);
});

// ---- account rules -------------------------------------------------------------------------

test("account rules: status spellings, password bound, profile patch, deactivation outcome, notices", () => {
  assert.equal(normalizeAccountStatus("pendingverification"), "pending_verification");
  assert.equal(normalizeAccountStatus("PENDING_VERIFICATION"), "pending_verification");
  assert.equal(passwordProblem("Abcdef1!"), null);
  assert.match(passwordProblem("Ab1!" + "é".repeat(40)), /72 bytes/);
  const profile = { username: "ann", displayName: "Ann", avatarUrl: null, timezone: null, locale: "en" };
  assert.deepEqual(profilePatch(profile, { username: "ann", displayName: "", avatarUrl: "", timezone: "Europe/London", locale: "en" }), {
    displayName: null,
    timezone: "Europe/London",
  });
  const reauth = envelope(403, "REAUTHENTICATION_REQUIRED");
  assert.equal(deactivationOutcome(reauth, true), "wrong_password");
  assert.equal(deactivationOutcome(reauth, false), "reauthenticate");
  assert.equal(deactivationOutcome(envelope(500, "INTERNAL_ERROR"), true), "failed");
  assert.match(signInNotice("password_changed"), /password/);
  assert.equal(signInNotice("<script>"), null);
  const params = (o) => ({ get: (k) => o[k] ?? null });
  assert.deepEqual(parseGoogleCallback(params({ exchange_code: "abc-123" })), { kind: "exchange", exchangeCode: "abc-123" });
  assert.deepEqual(parseGoogleCallback(params({ exchange_code: "a b" })), { kind: "invalid" });
});
