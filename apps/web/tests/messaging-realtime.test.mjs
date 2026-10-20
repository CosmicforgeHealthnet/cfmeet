import assert from "node:assert/strict";
import test from "node:test";
import { MessagingRealtimeTransport, MessagingTransportError } from "../src/services/messaging-realtime.ts";

const conversationId = "11111111-1111-4111-8111-111111111111";
const token = { token: "signed.messaging.token", tokenType: "CF-Client-Token", expiresAt: new Date(Date.now() + 600_000).toISOString() };
const ticket = {
  ticket: "a".repeat(64), subprotocol: "cfmeet.messaging.v1", ticketSubprotocolPrefix: "cfmeet.ticket.",
  path: "/v1/realtime", expiresAt: new Date(Date.now() + 30_000).toISOString(),
};

class FakeWebSocket {
  constructor(url, protocols) {
    this.url = url; this.protocols = protocols; this.readyState = 0;
    this.sent = []; this.callbacks = new Map();
  }
  addEventListener(type, callback) { this.callbacks.set(type, callback); }
  emit(type, event = {}) { this.callbacks.get(type)?.(event); }
  open() { this.readyState = 1; this.emit("open"); }
  send(value) { this.sent.push(JSON.parse(value)); }
  close() { this.readyState = 3; this.emit("close", { code: 1000 }); }
}

function fixture(overrides = {}) {
  const sockets = [], requests = [], events = [], sync = [];
  let issued = 0;
  const transport = new MessagingRealtimeTransport({
    baseUrl: "https://messaging.example.com/",
    getAccessToken: async () => { issued++; return token; },
    fetchImpl: async (input, init) => {
      requests.push({ url: String(input), init });
      return { ok: true, json: async () => ticket };
    },
    socketFactory: (url, protocols) => {
      const socket = new FakeWebSocket(url, protocols);
      sockets.push(socket);
      queueMicrotask(() => socket.open());
      return socket;
    },
    onEvent: async event => { events.push(event); },
    onSyncRequired: (conversation, frame) => { sync.push({ conversation, frame }); },
    ...overrides,
  });
  return { transport, sockets, requests, events, sync, issued: () => issued };
}
const tick = () => new Promise(resolve => setImmediate(resolve));

test("rejects remote plaintext, credentials and untrusted URL path/query", () => {
  for (const baseUrl of [
    "http://messaging.example.com/", "https://user:secret@messaging.example.com/",
    "https://messaging.example.com/v1/", "https://messaging.example.com/?token=secret",
  ]) {
    assert.throws(
      () => new MessagingRealtimeTransport({ baseUrl, getAccessToken: async () => token, onEvent: () => {}, onSyncRequired: () => {} }),
      MessagingTransportError,
    );
  }
});

test("one-use tickets are used only as subprotocols; every reconnect gets a new token and ticket", async () => {
  const f = fixture();
  f.transport.subscribe(conversationId, 9);
  await f.transport.connect();
  assert.equal(f.issued(), 1);
  assert.equal(f.requests[0].url, "https://messaging.example.com/v1/realtime/tickets");
  assert.equal(f.requests[0].init.headers.Authorization, "CF-Client-Token " + token.token);
  assert.equal(f.requests[0].init.credentials, "omit");
  assert.equal(f.requests[0].init.redirect, "error");
  assert.equal(f.sockets[0].url, "wss://messaging.example.com/v1/realtime");
  assert.deepEqual(f.sockets[0].protocols, ["cfmeet.messaging.v1", "cfmeet.ticket." + ticket.ticket]);
  assert.deepEqual(f.sockets[0].sent, [{ type: "subscribe", conversationId, afterEventSequence: 9 }]);
  f.sockets[0].close();
  await f.transport.connect();
  assert.equal(f.issued(), 2);
  assert.equal(f.requests.length, 2);
  f.transport.close();
});

test("mismatched ticket metadata is rejected before opening a socket", async () => {
  let opened = false;
  const f = fixture({
    fetchImpl: async () => ({ ok: true, json: async () => ({ ...ticket, path: "https://attacker.example/" }) }),
    socketFactory: () => { opened = true; throw new Error("must not open"); },
  });
  await assert.rejects(f.transport.connect(), MessagingTransportError);
  assert.equal(opened, false);
});

test("acknowledge after processing, suppress duplicates, detect replay gaps and resubscribe after REST sync", async () => {
  const f = fixture();
  f.transport.subscribe(conversationId);
  await f.transport.connect();
  f.sockets[0].emit("message", { data: JSON.stringify({
    type: "event", event: { eventId: "evt_" + "a".repeat(32), conversationId, eventSequence: 1 },
  }) });
  await tick();
  assert.equal(f.transport.cursor(conversationId), 1);
  assert.deepEqual(f.sockets[0].sent.at(-1), { type: "ack", conversationId, eventSequence: 1 });
  f.sockets[0].emit("message", { data: JSON.stringify({
    type: "event", event: { eventId: "evt_" + "a".repeat(32), conversationId, eventSequence: 1 },
  }) });
  f.sockets[0].emit("message", { data: JSON.stringify({
    type: "event", event: { eventId: "evt_" + "c".repeat(32), conversationId, eventSequence: 3 },
  }) });
  await tick();
  assert.equal(f.events.length, 1);
  assert.equal(f.sync.length, 1);
  assert.equal(f.transport.cursor(conversationId), 1);
  f.transport.synced(conversationId, 3);
  assert.deepEqual(f.sockets[0].sent.at(-1), { type: "subscribe", conversationId, afterEventSequence: 3 });
  f.transport.close();
});

test("failed application processing never acknowledges and invalid cursors fail closed", async () => {
  const f = fixture({ onEvent: () => { throw new Error("local cryptographic processing failed"); } });
  assert.throws(() => f.transport.subscribe("invalid"), MessagingTransportError);
  assert.throws(() => f.transport.subscribe(conversationId, -1), MessagingTransportError);
  f.transport.subscribe(conversationId);
  await f.transport.connect();
  f.sockets[0].emit("message", { data: JSON.stringify({
    type: "event", event: { eventId: "evt_" + "a".repeat(32), conversationId, eventSequence: 1 },
  }) });
  await tick();
  assert.equal(f.transport.cursor(conversationId), 0);
  assert.equal(f.sockets[0].sent.some(message => message.type === "ack"), false);
  f.transport.close();
});

test("refuses ordinary login bearer tokens; does not call privileged Messaging paths", async () => {
  const f = fixture({ getAccessToken: async () => ({ ...token, tokenType: "Bearer" }) });
  await assert.rejects(f.transport.connect(), MessagingTransportError);
  assert.equal(f.requests.length, 0);
});

test("serializes concurrent callback processing to preserve event order", async () => {
  const processed = [];
  const f = fixture({
    onEvent: async event => {
      if (event.eventSequence === 1) await tick();
      processed.push(event.eventSequence);
    },
  });
  f.transport.subscribe(conversationId);
  await f.transport.connect();
  for (const eventSequence of [1, 2]) {
    f.sockets[0].emit("message", { data: JSON.stringify({
      type: "event",
      event: { eventId: "evt_" + String(eventSequence).repeat(32), conversationId, eventSequence },
    }) });
  }
  await tick();
  await tick();
  assert.deepEqual(processed, [1, 2]);
  assert.equal(f.transport.cursor(conversationId), 2);
  f.transport.close();
});
