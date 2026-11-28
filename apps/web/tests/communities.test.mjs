// Communities (ADR 0073, Messaging): the screens' rules (validation, permissions mirroring the
// server, group order, invite links, chat-list filter and the together / separate preference)
// and the client's request paths and bodies through the Messaging relay, which must also be
// on the web's relay allowlist.
import assert from "node:assert/strict";
import test from "node:test";
import {
  canAddGroups, canDelete, canDemote, canInvite, canLeave, canManageSettings, canPromote, canRemove, canTransfer, cleanText, communityActivity, communityChats,
  communityConversationIds, communityOf, createBody, draftError, draftOf, groupLabel, inviteLinkUrl, moveGroup, outcomeText, patchBody, shownInAll,
  sortCommunities, sortGroups, splitGroups, tokenFrom, MAX_NAME,
} from "../src/lib/communities.ts";
import { createCommunitiesApi } from "../src/lib/communities-api.ts";
import { filterChips, matchesChatFilter } from "../src/lib/messaging/chat-list.ts";
import { isMessagingRelayRoute } from "../src/lib/messaging/routes.ts";

const C = "11111111-1111-4111-8111-111111111111";
const G = "22222222-2222-4222-8222-222222222222";
const P = "33333333-3333-4333-8333-333333333333";
const R = "44444444-4444-4444-8444-444444444444";

const community = (over = {}) => ({
  communityId: C, name: "Riverside", description: null, rules: null, privacy: "APPROVAL", membersCanAddGroups: false, ownerPrincipalId: P,
  announcementsConversationId: G, pictureRef: null, memberCount: 3, groupCount: 2, myRole: "MEMBER", joinedAt: "2026-11-20T10:00:00Z",
  joinedConversationIds: [G], pendingRequestCount: null, version: 4, createdAt: "2026-11-20T10:00:00Z", updatedAt: "2026-11-20T10:00:00Z", ...over,
});
const group = (id, over = {}) => ({ conversationId: id, title: null, description: null, avatarRef: null, securityProfile: "MANAGED", access: "OPEN", position: 0, announcements: false, joined: false, memberCount: null, myRole: null, canManage: false, pendingRequest: false, linkedAt: "2026-11-20T10:00:00Z", ...over });

test("names, descriptions and rules are trimmed and bounded", () => {
  assert.equal(cleanText("  Riverside  ", MAX_NAME), "Riverside");
  assert.equal(cleanText("   ", MAX_NAME), null);
  assert.equal(cleanText("", 10, true), "");
  assert.equal(cleanText("x".repeat(101), MAX_NAME), null);
  assert.equal(cleanText("bad\u0007bell", 50), null);
  assert.equal(cleanText("line one\nline two", 50), "line one\nline two");
  const draft = { name: "", description: "", rules: "", privacy: "OPEN", membersCanAddGroups: false };
  assert.match(draftError(draft), /name/);
  assert.equal(draftError({ ...draft, name: "Parents" }), null);
  assert.match(draftError({ ...draft, name: "P", rules: "r".repeat(4001) }), /rules/);
  assert.deepEqual(createBody({ ...draft, name: " Parents ", description: "  " }), { name: "Parents", privacy: "OPEN", membersCanAddGroups: false });
});

test("a settings change sends only what differs, with the version, and null clears a text", () => {
  const c = community({ description: "Old" });
  assert.deepEqual(patchBody(c, draftOf(c)), { expectedVersion: 4 });
  assert.deepEqual(patchBody(c, { ...draftOf(c), description: "", privacy: "OPEN", name: "River" }), { expectedVersion: 4, name: "River", description: null, privacy: "OPEN" });
  assert.deepEqual(patchBody(c, { ...draftOf(c), rules: "Be kind", membersCanAddGroups: true }), { expectedVersion: 4, rules: "Be kind", membersCanAddGroups: true });
});

test("permissions mirror the server's roles", () => {
  assert.ok(canManageSettings("OWNER") && canManageSettings("ADMIN") && !canManageSettings("MEMBER"));
  assert.ok(canInvite("OPEN", "MEMBER") && canInvite("APPROVAL", "MEMBER") && !canInvite("INVITE_ONLY", "MEMBER") && canInvite("INVITE_ONLY", "ADMIN"));
  assert.equal(canInvite("OPEN", null), false);
  assert.ok(!canAddGroups(community()) && canAddGroups(community({ membersCanAddGroups: true })) && canAddGroups(community({ myRole: "ADMIN" })));
  // The owner removes anyone but the owner; an admin removes members only; members nobody.
  assert.ok(canRemove("OWNER", "ADMIN") && canRemove("OWNER", "MEMBER") && !canRemove("OWNER", "OWNER"));
  assert.ok(canRemove("ADMIN", "MEMBER") && !canRemove("ADMIN", "ADMIN") && !canRemove("MEMBER", "MEMBER"));
  assert.ok(canPromote("ADMIN", "MEMBER") && !canPromote("MEMBER", "MEMBER") && !canPromote("OWNER", "ADMIN"));
  // Only the owner dismisses an admin; an admin may step down.
  assert.ok(canDemote("OWNER", "ADMIN") && !canDemote("ADMIN", "ADMIN") && canDemote("ADMIN", "ADMIN", true) && !canDemote("OWNER", "MEMBER"));
  assert.ok(canTransfer("OWNER") && !canTransfer("ADMIN") && canDelete("OWNER") && !canDelete("ADMIN"));
  assert.ok(!canLeave("OWNER") && canLeave("ADMIN") && canLeave("MEMBER"));
});

test("groups: announcements first, then the community's order; joined and joinable split", () => {
  const groups = [group("c", { position: 2 }), group("a", { announcements: true, joined: true, position: 5 }), group("b", { position: 1, joined: true })];
  assert.deepEqual(sortGroups(groups).map((g) => g.conversationId), ["a", "b", "c"]);
  const { joined, joinable } = splitGroups(groups);
  assert.deepEqual(joined.map((g) => g.conversationId), ["a", "b"]);
  assert.deepEqual(joinable.map((g) => g.conversationId), ["c"]);
  // Moving never includes the Announcements group and stops at the ends.
  assert.deepEqual(moveGroup(groups, "c", -1), ["c", "b"]);
  assert.equal(moveGroup(groups, "b", -1), null);
  assert.equal(moveGroup(groups, "a", 1), null);
  assert.equal(groupLabel({ title: null, announcements: true }), "Announcements");
  assert.equal(groupLabel({ title: null, announcements: false }, "Known"), "Known");
  assert.equal(groupLabel({ title: " ", announcements: false }), "Group");
});

test("invite links: the URL carries the token, and only community tokens are accepted back", () => {
  const token = "mcl_" + "A".repeat(43);
  const url = inviteLinkUrl("https://cfmeet.com/", token);
  assert.equal(url, `https://cfmeet.com/dashboard/communities/join?token=${token}`);
  assert.equal(tokenFrom(url), token);
  assert.equal(tokenFrom(`  ${token} `), token);
  assert.equal(tokenFrom("mil_" + "A".repeat(43)), null, "a group invite link is not a community link");
  assert.equal(tokenFrom("https://cfmeet.com/dashboard/communities/join"), null);
  assert.equal(tokenFrom("javascript:alert(1)"), null);
});

test("chat list: the Communities filter and the together / separate preference", () => {
  const communities = [community(), community({ communityId: "x", joinedConversationIds: [R] })];
  const ids = communityConversationIds(communities);
  assert.deepEqual([...ids].sort(), [G, R].sort());
  assert.equal(communityOf(communities, R).communityId, "x");
  assert.equal(communityOf(communities, P), null);
  const chats = [{ id: G }, { id: P }, { id: R }];
  assert.deepEqual(communityChats(chats, ids).map((c) => c.id), [G, R]);
  assert.ok(shownInAll(G, ids, "TOGETHER") && !shownInAll(G, ids, "SEPARATE") && shownInAll(P, ids, "SEPARATE"));
  const chat = (id) => ({ id, unreadCount: 0, favourite: false, contextClass: "GROUP", securityProfile: "MANAGED" });
  const ctx = { selected: null, lists: [], communityIds: ids };
  assert.equal(matchesChatFilter(chat(G), { kind: "communities" }, ctx), true);
  assert.equal(matchesChatFilter(chat(P), { kind: "communities" }, ctx), false);
  assert.equal(matchesChatFilter(chat(G), { kind: "all" }, ctx), true);
  assert.equal(matchesChatFilter(chat(G), { kind: "all" }, { ...ctx, communitiesSeparate: true }), false);
  assert.equal(matchesChatFilter(chat(P), { kind: "all" }, { ...ctx, communitiesSeparate: true }), true);
  // Older callers without communities are unchanged.
  assert.equal(matchesChatFilter(chat(G), { kind: "all" }, { selected: null, lists: [] }), true);
  assert.deepEqual(filterChips({ unread: 0, hasPrivate: false, lists: [], communities: true }).map((c) => c.key), ["all", "unread", "favourites", "groups", "communities"]);
  assert.ok(!filterChips({ unread: 0, hasPrivate: false, lists: [] }).some((c) => c.key === "communities"));
});

test("activity: unread and the latest activity of the community's chats order the list", () => {
  const chats = [
    { id: G, unreadCount: 2, lastActivityAt: "2026-11-27T10:00:00Z" },
    { id: R, unreadCount: 0, lastActivityAt: "2026-11-28T09:00:00Z" },
  ];
  assert.deepEqual(communityActivity(community(), chats), { unreadChats: 1, unread: 2, lastActivityAt: "2026-11-27T10:00:00Z" });
  const a = community({ name: "A" });
  const b = community({ name: "B", joinedConversationIds: [R] });
  const quiet = community({ name: "C", joinedConversationIds: [], joinedAt: "2026-01-01T00:00:00Z" });
  assert.deepEqual(sortCommunities([quiet, a, b], chats).map((c) => c.name), ["B", "A", "C"]);
  assert.match(outcomeText({ outcome: "REQUESTED" }, "community"), /admin/);
  assert.match(outcomeText({ outcome: "JOINED" }, "group"), /joined the group/);
});

test("the client calls the documented paths with the documented bodies, all on the relay allowlist", async () => {
  const calls = [];
  const api = createCommunitiesApi(async (method, path, body, key) => { calls.push({ method, path, body, key }); return {}; }, () => "web-key");
  await api.list(null, 100);
  await api.list("abc", 20);
  await api.create({ name: "Riverside", privacy: "OPEN" });
  await api.get(C);
  await api.update(C, { expectedVersion: 4, name: "R" });
  await api.remove(C);
  await api.transfer(C, P);
  await api.leave(C);
  await api.members(C, "next");
  await api.setRole(C, P, "ADMIN");
  await api.removeMember(C, P);
  await api.bans(C);
  await api.ban(C, P);
  await api.unban(C, P);
  await api.invitations(C);
  await api.invite(C, [P]);
  await api.revokeInvitation(C, R);
  await api.myInvitations();
  await api.acceptInvitation(R);
  await api.declineInvitation(R);
  await api.inviteLinks(C);
  await api.createInviteLink(C, { expiresInSeconds: 3600, maxUses: 5 });
  await api.revokeInviteLink(C, R);
  await api.previewLink("mcl_x");
  await api.joinByLink("mcl_x", "  hi  ");
  await api.joinByLink("mcl_x");
  await api.joinRequests(C);
  await api.approve(C, R);
  await api.reject(C, R);
  await api.myRequests();
  await api.cancelRequest(R);
  await api.groups(C);
  await api.createGroup(C, { title: "Football", access: "RESTRICTED" });
  await api.linkGroup(C, G, "OPEN");
  await api.linkGroup(C, G);
  await api.orderGroups(C, [G]);
  await api.updateGroup(C, G, "RESTRICTED");
  await api.unlinkGroup(C, G);
  await api.joinGroup(C, G);
  await api.preferences();
  await api.setPreferences("SEPARATE");
  const base = "v1/messaging/communities";
  assert.deepEqual(calls[0], { method: "GET", path: `${base}?limit=100`, body: undefined, key: undefined });
  assert.equal(calls[1].path, `${base}?limit=20&cursor=abc`);
  assert.deepEqual(calls[2], { method: "POST", path: base, body: { name: "Riverside", privacy: "OPEN" }, key: "web-key" });
  assert.deepEqual(calls[9], { method: "PUT", path: `${base}/${C}/members/${P}/role`, body: { role: "ADMIN" }, key: undefined });
  assert.deepEqual(calls[15].body, { principalIds: [P] });
  assert.deepEqual(calls[24].body, { token: "mcl_x", message: "hi" });
  assert.deepEqual(calls[25].body, { token: "mcl_x" });
  assert.equal(calls[26].path, `${base}/${C}/join-requests?state=PENDING`);
  assert.equal(calls[32].key, "web-key");
  assert.deepEqual(calls[33].body, { conversationId: G, access: "OPEN" });
  assert.deepEqual(calls[34].body, { conversationId: G });
  assert.deepEqual(calls[40], { method: "PUT", path: `${base}/preferences`, body: { chatListMode: "SEPARATE" }, key: undefined });
  for (const c of calls) assert.equal(isMessagingRelayRoute(c.method, c.path), true, `${c.method} ${c.path}`);
  // Never another principal's administration, traversal or unknown query keys.
  assert.equal(isMessagingRelayRoute("GET", `${base}?principalId=${P}`), false);
  assert.equal(isMessagingRelayRoute("POST", `${base}/../policy`), false);
  assert.equal(isMessagingRelayRoute("GET", `${base}/not-an-id`), false);
});
