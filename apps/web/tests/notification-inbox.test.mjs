// The bell (owner report, 19 November 2026: "the bell doesn't get all the notifications"):
// every notification of the account is listed (pages merged through "Load more", no category
// filter, no duplicate when an item is grouped between two loads), and each one opens the
// place it is about: the chat, the call history, Contacts > Requests, the meeting.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { INBOX_PAGE_SIZE, inboxHref, mergeInboxPages } from "../src/lib/notifications/inbox.ts";

const ID = "11111111-1111-4111-8111-111111111111";
const item = (kind, ref = ID, extra = {}) => ({ id: `${kind}-${ref}`, action: { kind, ref }, ...extra });

test("each notification opens the place it is about", () => {
  assert.equal(inboxHref(item("OPEN_CONVERSATION")), `/dashboard/messages?conversation=${ID}`);
  assert.equal(inboxHref(item("OPEN_CALL_HISTORY")), `/calling?call=${ID}`);
  assert.equal(inboxHref(item("OPEN_VOICEMAIL")), "/calling?tab=voicemail");
  assert.equal(inboxHref(item("OPEN_RECORDING")), "/calling");
  assert.equal(inboxHref(item("OPEN_MEETING")), `/conferences/${ID}`);
  assert.equal(inboxHref(item("OPEN_MEETING", null)), "/dashboard/meetings");
  assert.equal(inboxHref(item("OPEN_CONTACT_REQUEST")), "/dashboard/contacts?tab=requests");
  assert.equal(inboxHref(item("OPEN_CONTACT")), "/dashboard/contacts");
  assert.equal(inboxHref(item("OPEN_ORGANIZATION_INVITATION")), "/dashboard/organizations");
  assert.equal(inboxHref(item("OPEN_STATUS")), `/dashboard/updates?status=${ID}`);
  assert.equal(inboxHref(item("OPEN_STATUS", null)), "/dashboard/updates");
  assert.equal(inboxHref(item("OPEN_COMMUNITY")), `/dashboard/communities/${ID}`);
  assert.equal(inboxHref(item("OPEN_COMMUNITY", null)), "/dashboard/communities");
  assert.equal(inboxHref(item("OPEN_ORGANIZATION")), "/dashboard/organizations");
  assert.equal(inboxHref(item("OPEN_SECURITY_SETTINGS", null)), "/dashboard/settings?tab=security");
  assert.equal(inboxHref(item("OPEN_DEVELOPER_APPLICATION")), "/developers");
  assert.equal(inboxHref(item("NONE", null)), null);
  // A reference is always a path segment / query value, never markup or another path.
  assert.equal(inboxHref(item("OPEN_CALL_HISTORY", "../x?y")), "/calling?call=..%2Fx%3Fy");
});

test("every action kind of the contract has a rule", () => {
  const contract = JSON.parse(readFileSync(new URL("../../../packages/contracts/notifications/common/v1.schema.json", import.meta.url), "utf8"));
  const kinds = contract.$defs.ActionKind.enum;
  assert.ok(kinds.length >= 12);
  for (const kind of kinds) {
    if (kind === "NONE") continue;
    assert.ok(inboxHref(item(kind)), kind);
  }
});

test("pages are merged in order, without dismissed items or duplicates", () => {
  const a = item("OPEN_CALL_HISTORY", "a");
  const b = item("OPEN_CONTACT_REQUEST", "b");
  const c = item("OPEN_CONVERSATION", "c");
  const gone = item("OPEN_MEETING", "d", { dismissedAt: "2026-11-19T10:00:00Z" });
  const merged = mergeInboxPages([{ items: [a, b, gone], nextCursor: "x" }, { items: [b, c], nextCursor: null }]);
  assert.deepEqual(merged.map((i) => i.id), [a.id, b.id, c.id]);
  assert.deepEqual(mergeInboxPages(undefined), []);
});

test("the bell pages through the whole inbox with no category filter", () => {
  assert.ok(INBOX_PAGE_SIZE >= 1 && INBOX_PAGE_SIZE <= 100);
  const bell = readFileSync(new URL("../src/components/layout/NotificationBell.tsx", import.meta.url), "utf8");
  const service = readFileSync(new URL("../src/services/v2/notifications.service.ts", import.meta.url), "utf8");
  assert.match(bell, /useInfiniteQuery/);
  assert.match(bell, /fetchNextPage/);
  assert.match(bell, /getNextPageParam: \(last\) => last\.nextCursor/);
  assert.doesNotMatch(service, /category[:=]/);
  assert.doesNotMatch(service, /state: "unread"/);
});
