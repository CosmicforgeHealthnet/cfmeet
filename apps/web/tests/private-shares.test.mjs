import assert from "node:assert/strict";
import test from "node:test";
import { isLiveStatus, sharePrivately, statusGroupTitle, statusPostId, STATUS_LIFETIME_MS } from "../src/lib/messaging/private-shares.ts";

const postId = "11111111-1111-4111-8111-111111111111";
const recipients = [{ cfMeetId: "alice.cf", label: "Alice" }, { cfMeetId: "bob.cf", label: "Bob" }];

test("a broadcast encrypts separate direct messages and never exposes the audience", async () => {
  const calls = [], messages = [];
  const transport = {
    async createConversation(users, profile, options) { calls.push({ users, profile, options }); return options.cfMeetIds[0]; },
    async sendRich(id, payload) { messages.push({ id, payload }); },
  };
  const result = await sharePrivately(transport, [...recipients, recipients[0]], "hello");
  assert.deepEqual(result, { sent: ["alice.cf", "bob.cf"], unconfirmed: [] });
  assert.equal(calls.length, 2);
  assert.ok(calls.every((c) => c.profile === "PRIVATE_E2EE" && c.options.cfMeetIds.length === 1 && !c.options.title));
  assert.deepEqual(messages, [{ id: "alice.cf", payload: { t: "text", text: "hello" } }, { id: "bob.cf", payload: { t: "text", text: "hello" } }]);
});

test("a status uses isolated announcement groups with server expiry and 24-hour retention", async () => {
  const calls = [];
  const createdAt = Date.now();
  const transport = { async createConversation(_, __, options) { calls.push(options); return options.cfMeetIds[0]; }, async sendRich() {} };
  await sharePrivately(transport, recipients, "my day", { postId, createdAt });
  assert.equal(calls.length, 2);
  for (const c of calls) {
    assert.equal(c.title, statusGroupTitle(postId));
    assert.equal(c.retentionSeconds, 86400);
    assert.equal(c.announcementOnly, true);
    assert.equal(Date.parse(c.expiresAt), createdAt + STATUS_LIFETIME_MS);
    assert.equal(c.cfMeetIds.length, 1);
  }
});

test("a transport outage is unconfirmed and never automatically retried", async () => {
  let attempts = 0;
  const transport = { async createConversation(_, __, options) { return options.cfMeetIds[0]; }, async sendRich(id) { attempts++; if (id === "bob.cf") throw new Error("timeout after commit"); } };
  const result = await sharePrivately(transport, recipients, "hello");
  assert.equal(attempts, 2);
  assert.deepEqual(result.sent, ["alice.cf"]);
  assert.deepEqual(result.unconfirmed, [recipients[1]]);
});

test("ordinary chats and expired statuses never appear as a live status", () => {
  const now = Date.now();
  const status = { groupTitle: statusGroupTitle(postId), expiresAt: new Date(now + 1000).toISOString(), contextClass: "GROUP", securityProfile: "PRIVATE_E2EE", announcementOnly: true };
  assert.equal(isLiveStatus(status, now), true);
  assert.equal(isLiveStatus(status, now + 1000), false);
  assert.equal(isLiveStatus({ ...status, announcementOnly: false }, now), false);
  assert.equal(isLiveStatus({ ...status, securityProfile: "MANAGED" }, now), false);
  assert.equal(statusPostId("a normal group"), null);
});
