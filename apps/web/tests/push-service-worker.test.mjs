// The Web Push service worker (public/cfmeet-push-sw.js) against the payloads Messaging
// sends today and the Notification Service sends after the push cutover (roadmap N9):
// metadata only, values possibly strings; notification text never comes from the payload;
// clicks navigate to same-origin paths only.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const source = readFileSync(new URL("../public/cfmeet-push-sw.js", import.meta.url), "utf8");

function worker({ focused = false, windows = [] } = {}) {
  const listeners = {};
  const shown = [];
  const opened = [];
  const self = {
    location: { origin: "https://cfmeet.com" },
    addEventListener: (type, fn) => { listeners[type] = fn; },
    registration: { showNotification: async (title, options) => { shown.push({ title, options }); } },
    clients: {
      matchAll: async () => (focused ? [{ focused: true, url: "https://cfmeet.com/x" }] : windows),
      openWindow: async (url) => { opened.push(url); },
    },
  };
  vm.runInNewContext(source, { self, URL, Object, encodeURIComponent });
  const run = async (type, event) => {
    let waited;
    listeners[type]({ ...event, waitUntil: (p) => { waited = p; } });
    await waited;
  };
  return { run, shown, opened };
}

const push = (payload) => ({ data: { json: () => payload } });

test("a message wake-up (legacy numbers or service strings) opens its conversation", async () => {
  for (const payload of [
    { v: 1, type: "message", conversationId: "c-1", eventSequence: 4, sequence: 3 },
    { v: "1", type: "message", conversationId: "c-1", eventSequence: "4", sequence: "3" },
  ]) {
    const w = worker();
    await w.run("push", push(payload));
    assert.equal(w.shown.length, 1);
    assert.equal(w.shown[0].title, "New message");
    assert.equal(w.shown[0].options.tag, "chat-c-1");
    assert.equal(w.shown[0].options.data.url, "/dashboard/messages?conversation=c-1");
  }
});

test("call and inbox pushes from the service get their own alert and destination", async () => {
  const call = worker();
  await call.run("push", push({ v: "1", type: "call" }));
  assert.equal(call.shown[0].title, "Incoming call");
  assert.equal(call.shown[0].options.tag, "cfmeet-call");
  assert.equal(call.shown[0].options.data.url, "/dashboard/calls");
  const inbox = worker();
  await inbox.run("push", push({ v: 1, type: "inbox", sequence: 7 }));
  assert.equal(inbox.shown[0].options.data.url, "/dashboard");
  // Unknown or broken payloads fall back to the generic message alert, never their text.
  const odd = worker();
  await odd.run("push", { data: { json: () => { throw new Error("x"); } } });
  await odd.run("push", push({ type: "<b>hi</b>", text: "secret" }));
  assert.deepEqual(odd.shown.map((s) => s.title), ["New message", "New message"]);
  assert.ok(!JSON.stringify(odd.shown).includes("secret"));
});

test("no alert while a CF Meet tab is focused", async () => {
  const w = worker({ focused: true });
  await w.run("push", push({ v: 1, type: "call" }));
  assert.equal(w.shown.length, 0);
});

test("clicks navigate to same-origin paths only", async () => {
  const navigated = [];
  const tab = { url: "https://cfmeet.com/dashboard", focus: () => {}, navigate: async (u) => { navigated.push(u); } };
  const w = worker({ windows: [tab] });
  const click = (url) => ({ notification: { close: () => {}, data: { url } } });
  await w.run("notificationclick", click("/dashboard/calls"));
  await w.run("notificationclick", click("https://evil.example/"));
  await w.run("notificationclick", click("//evil.example/"));
  assert.deepEqual(navigated, ["/dashboard/calls", "/dashboard", "/dashboard"]);
  const none = worker();
  await none.run("notificationclick", click("/dashboard/messages?conversation=c"));
  assert.deepEqual(none.opened, ["/dashboard/messages?conversation=c"]);
});
