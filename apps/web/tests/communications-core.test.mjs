// MSG-006 / ACC-009 / MSG-009: navigation hooks, Hub change-feed handling, capability gating,
// workspace-aware people sources, and the server per-device ack-cursor resume (MSG-018).
import assert from "node:assert/strict";
import test from "node:test";
import {
  areasForDomains, callHref, chatHref, colleaguesOfOrganization, communicationsAreas, connectedContacts,
  contactLabel, conversationPeopleSource, inviteTokenFromPayload, meetingJoinHref, meetingShareLine,
  normalizeCfMeetId, syncStep,
} from "../src/lib/communications/comms-core.ts";
import { communicationsErrorMessage } from "../src/lib/communications/errors.ts";
import { MessagingRealtimeTransport } from "../src/services/messaging-realtime.ts";
import { messagingSessionDeviceToken } from "../src/services/messaging-session.ts";
import { isMessagingRelayRoute } from "../src/lib/messaging/routes.ts";

const id = "11111111-1111-4111-8111-111111111111";

test("call / chat / meeting links are the navigation contract; invalid ids are refused", () => {
  assert.equal(callHref({ contactId: id }, "VIDEO"), `/calling?contactId=${id}&kind=VIDEO`);
  assert.equal(callHref({ conversationId: id }), `/calling?conversationId=${id}&kind=VOICE`);
  assert.throws(() => callHref({ contactId: "../x" }));
  assert.equal(chatHref(id), `/dashboard/messages?conversation=${id}`);
  assert.equal(meetingJoinHref("abc-defg-hij"), "/abc-defg-hij");
  assert.throws(() => meetingJoinHref("https://evil.example"));
  assert.equal(meetingShareLine("abc-defg-hij", "INSTANT"), "Started a meeting: abc-defg-hij");
});

test("invite payloads: only the connect path's token is accepted", () => {
  const token = "A".repeat(43);
  assert.equal(inviteTokenFromPayload(`https://cfmeet.com/connect/${token}`), token);
  assert.equal(inviteTokenFromPayload(token), token);
  assert.equal(inviteTokenFromPayload(`https://cfmeet.com/other/${token}`), null);
  assert.equal(inviteTokenFromPayload(`https://cfmeet.com/connect/${token}?x=1`), null);
  assert.equal(inviteTokenFromPayload("short"), null);
});

test("CF Meet IDs are normalized and validated", () => {
  const raw = "abcdefghijklmnopqrstuvwxyz".toUpperCase().replace(/[01890]/g, "A");
  assert.equal(normalizeCfMeetId(` cf-${raw.toLowerCase()} `), `CF-${raw}`);
  assert.equal(normalizeCfMeetId("CF-123"), null);
});

test("Hub change-feed domains map to refetch areas; resync is never a silent ack", () => {
  assert.deepEqual([...areasForDomains(["CONTACT", "BLOCK", "PRIVACY"])].sort(), ["blocks", "contacts", "privacy", "requests"]);
  const step = syncStep(5, { resyncRequired: false, latestPosition: 9, nextPosition: 9, hasMore: false, data: [{ position: 7, domain: "TIMELINE" }, { position: 6, domain: "PROFILE" }] });
  assert.equal(step.kind, "apply");
  assert.equal(step.position, 9);
  assert.ok(step.areas.has("timeline") && step.areas.has("profile"));
  assert.deepEqual(syncStep(5, { resyncRequired: true, latestPosition: 40, nextPosition: 5, data: [] }), { kind: "resync", position: 40 });
});

test("capability gating: default deny; MANAGED availability is not E2EE readiness", () => {
  assert.equal(communicationsAreas(null).contacts, false);
  const a = communicationsAreas({
    activationState: "ACTIVE", contacts: { available: true }, personalCalling: { available: false },
    messaging: { managed: { available: true, realtime: "SESSION_DEVICE" }, privateE2ee: { available: false } },
  });
  assert.deepEqual([a.contacts, a.managedMessaging, a.privateE2ee, a.calling, a.realtime], [true, true, false, false, "SESSION_DEVICE"]);
  assert.equal(communicationsAreas({ activationState: "SUSPENDED", contacts: { available: true } }).contacts, false);
});

test("workspace awareness: Personal offers contacts, an organization only its own colleagues", () => {
  assert.equal(conversationPeopleSource("PERSONAL"), "contacts");
  assert.equal(conversationPeopleSource("ORGANIZATION"), "colleagues");
  const directory = [{ userId: "u1" }, { userId: "u2" }, { userId: "u3" }];
  assert.deepEqual(colleaguesOfOrganization(directory, null), [], "unknown membership: nothing offered");
  assert.deepEqual(colleaguesOfOrganization(directory, [{ userId: "u1", status: "ACTIVE" }, { userId: "u2", status: "SUSPENDED" }]).map((p) => p.userId), ["u1"]);
  assert.deepEqual(connectedContacts([{ relationship: "CONNECTED" }, { relationship: "REQUEST_PENDING" }]).length, 1);
  assert.equal(contactLabel({ alias: " Mum ", displayName: "Ada", cfMeetId: "CF-X" }), "Mum");
});

test("Hub errors map to neutral, user-facing sentences", () => {
  assert.match(communicationsErrorMessage({ code: "NOT_FOUND", status: 404 }), /no longer available/);
  assert.match(communicationsErrorMessage({ code: "RATE_LIMITED", status: 429 }), /Too many/);
  assert.match(communicationsErrorMessage({ code: "WHATEVER", status: 503 }), /temporarily unavailable/);
});

test("session-device tokens: SESSION_DEVICE only, and a renewal must keep the same device", () => {
  const body = { token: "t", tokenType: "CF-Client-Token", deviceKind: "SESSION_DEVICE", deviceId: id, sessionId: "22222222-2222-4222-8222-222222222222", expiresAt: new Date(Date.now() + 60_000).toISOString(), scopes: ["messages.read"] };
  assert.equal(messagingSessionDeviceToken(body, null).deviceId, id);
  assert.throws(() => messagingSessionDeviceToken({ ...body, deviceKind: "E2EE_DEVICE" }, null));
  assert.throws(() => messagingSessionDeviceToken(body, "33333333-3333-4333-8333-333333333333"));
});

test("the relay allowlist covers the MANAGED operations the client now uses", () => {
  const c = `v1/conversations/${id}`;
  for (const [m, p] of [["PATCH", `${c}/messages/${id}`], ["PUT", `${c}/messages/${id}/reactions/%F0%9F%91%8D`], ["PUT", `${c}/pins/${id}`],
    ["GET", `${c}/search?q=hi&limit=25`], ["GET", `${c}/events?after=4&limit=100`], ["DELETE", `${c}/messages/${id}?scope=ME`],
    ["GET", "v1/conversations?limit=100&orderBy=lastActivity"], ["PATCH", `${c}/members/${id}`]]) {
    assert.equal(isMessagingRelayRoute(m, p), true, `${m} ${p}`);
  }
  assert.equal(isMessagingRelayRoute("PUT", `${c}/messages/${id}/reactions/..`), false);
  assert.equal(isMessagingRelayRoute("DELETE", `${c}/messages/${id}?scope=ME&principalId=x`), false);
});

// ---- MSG-018: resume from the server's per-device ack cursor

class FakeWebSocket {
  constructor() { this.readyState = 0; this.sent = []; this.callbacks = new Map(); }
  addEventListener(type, cb) { this.callbacks.set(type, cb); }
  emit(type, event = {}) { this.callbacks.get(type)?.(event); }
  open() { this.readyState = 1; this.emit("open"); }
  send(v) { this.sent.push(JSON.parse(v)); }
  close() { this.readyState = 3; this.emit("close", { code: 1000 }); }
}
const tick = () => new Promise((r) => setImmediate(r));

test("subscribeResume omits the cursor, adopts the server's resume point, then acks contiguous events", async () => {
  const sockets = [], events = [], subscribed = [], typing = [];
  const t = new MessagingRealtimeTransport({
    baseUrl: "https://messaging.example.com/",
    getAccessToken: async () => ({ token: "x", tokenType: "CF-Client-Token", expiresAt: new Date(Date.now() + 60_000).toISOString() }),
    fetchImpl: async () => ({ ok: true, json: async () => ({ ticket: "a".repeat(64), subprotocol: "cfmeet.messaging.v1", ticketSubprotocolPrefix: "cfmeet.ticket.", path: "/v1/realtime", expiresAt: new Date(Date.now() + 30_000).toISOString() }) }),
    socketFactory: () => { const s = new FakeWebSocket(); sockets.push(s); queueMicrotask(() => s.open()); return s; },
    onEvent: (e) => { events.push(e.eventSequence); },
    onSyncRequired: () => {},
    onSubscribed: (c, after, head) => subscribed.push([after, head]),
    onTyping: (c, p, state) => typing.push(state),
  });
  t.subscribeResume(id);
  await t.connect();
  assert.deepEqual(sockets[0].sent[0], { type: "subscribe", conversationId: id }, "no afterEventSequence: the server uses its stored ack cursor");
  // An event before `subscribed` is not processed (the resume point is unknown).
  sockets[0].emit("message", { data: JSON.stringify({ type: "event", event: { eventId: "evt_" + "1".repeat(32), conversationId: id, eventSequence: 41 } }) });
  await tick();
  assert.deepEqual(events, []);
  sockets[0].emit("message", { data: JSON.stringify({ type: "subscribed", conversationId: id, afterEventSequence: 40, headEventSequence: 42 }) });
  await tick();
  assert.equal(t.cursor(id), 40);
  assert.deepEqual(subscribed, [[40, 42]]);
  for (const seq of [41, 42]) sockets[0].emit("message", { data: JSON.stringify({ type: "event", event: { eventId: "evt_" + String(seq).repeat(16), conversationId: id, eventSequence: seq } }) });
  await tick(); await tick();
  assert.deepEqual(events, [41, 42]);
  assert.deepEqual(sockets[0].sent.at(-1), { type: "ack", conversationId: id, eventSequence: 42 });
  sockets[0].emit("message", { data: JSON.stringify({ type: "typing", conversationId: id, principalId: id, state: "started", ttlMs: 5000 }) });
  await tick();
  assert.deepEqual(typing, ["started"]);
  // A reconnect resumes from the locally known cursor (never behind the server's ack cursor).
  sockets[0].close();
  await t.connect();
  assert.deepEqual(sockets[1].sent[0], { type: "subscribe", conversationId: id, afterEventSequence: 42 });
  t.close();
});
