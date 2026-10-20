import assert from "node:assert/strict";
import test from "node:test";
import { MessagingSessionError, messagingTokenProvider } from "../src/services/messaging-session.ts";

const deviceId = "11111111-1111-4111-8111-111111111111";
const sessionId = "22222222-2222-4222-8222-222222222222";
const valid = () => ({
  token: "signed.messaging.client.token", tokenType: "CF-Client-Token",
  deviceId, sessionId, scopes: ["messages.read", "messages.send"],
  expiresAt: new Date(Date.now() + 5 * 60_000).toISOString(),
});
const defaults = {
  meetingApiV1Url: "https://api.cfmeet.example/api/v1",
  getLoginToken: async () => "signed.meeting.login",
  deviceId,
};

test("exchanges CF Meet login with the trusted Meeting Service, never Messaging directly", async () => {
  const sent = [];
  const mint = messagingTokenProvider({
    ...defaults,
    fetchImpl: async (url, init) => {
      sent.push({ url: String(url), init });
      return { ok: true, json: async () => valid() };
    },
  });
  assert.deepEqual(await mint(), {
    token: valid().token, tokenType: "CF-Client-Token", expiresAt: valid().expiresAt,
  });
  assert.equal(sent.length, 1);
  assert.equal(sent[0].url, "https://api.cfmeet.example/api/v2/messaging/client-tokens");
  assert.equal(sent[0].init.method, "POST");
  assert.equal(sent[0].init.credentials, "omit");
  assert.equal(sent[0].init.redirect, "error");
  assert.equal(sent[0].init.cache, "no-store");
  assert.equal(sent[0].init.headers.Authorization, "Bearer signed.meeting.login");
  assert.deepEqual(JSON.parse(sent[0].init.body), { deviceId });
  assert.ok(!sent[0].init.body.includes("signed.meeting.login"), "bearer is never put in JSON");
});

test("rejects insecure or potentially attacker-controlled CF Meet API destinations", () => {
  for (const meetingApiV1Url of [
    "http://api.cfmeet.example/api/v1",
    "https://user:password@api.cfmeet.example/api/v1",
    "https://api.cfmeet.example/api/v1?next=https://evil.example",
    "https://api.cfmeet.example/api/v1#frag",
    "https://api.cfmeet.example/other/api/v1",
    "https://api.cfmeet.example/api/v2",
    "javascript:alert(1)",
  ]) {
    assert.throws(() => messagingTokenProvider({ ...defaults, meetingApiV1Url }), MessagingSessionError);
  }
  assert.doesNotThrow(() => messagingTokenProvider({
    ...defaults, meetingApiV1Url: "http://localhost:3001/api/v1",
  }));
  assert.throws(() => messagingTokenProvider({ ...defaults, deviceId: "not-a-device" }), MessagingSessionError);
});

test("fails closed before network on missing login, not sending an anonymous token request", async () => {
  let called = false;
  const mint = messagingTokenProvider({
    ...defaults,
    getLoginToken: () => null,
    fetchImpl: async () => { called = true; throw new Error("must not call"); },
  });
  await assert.rejects(mint(), MessagingSessionError);
  assert.equal(called, false);
});

test("rejects expired, wrong-device and wrong-kind tokens, without returning session credentials", async () => {
  const changes = [
    { tokenType: "Bearer" },
    { deviceId: "33333333-3333-4333-8333-333333333333" },
    { sessionId: "forged-session" },
    { expiresAt: new Date(Date.now() - 1000).toISOString() },
    { token: "" },
  ];
  for (const change of changes) {
    const mint = messagingTokenProvider({
      ...defaults, fetchImpl: async () => ({ ok: true, json: async () => ({ ...valid(), ...change }) }),
    });
    await assert.rejects(mint(), MessagingSessionError);
  }
});

test("authorization failures, malformed JSON and transport outages do not leak secrets", async () => {
  for (const fetchImpl of [
    async () => ({ ok: false, status: 401 }),
    async () => ({ ok: false, status: 503 }),
    async () => ({ ok: true, json: async () => { throw new Error("secret credentials"); } }),
    async () => { throw new Error("TLS error containing secret"); },
  ]) {
    const mint = messagingTokenProvider({ ...defaults, fetchImpl });
    await assert.rejects(mint(), (error) => {
      assert.ok(error instanceof MessagingSessionError);
      assert.ok(!error.message.includes("secret"));
      return true;
    });
  }
});
