import assert from "node:assert/strict";
import { test } from "node:test";
import { nativePushRegistration, type NativePushSource, type NativePushToken } from "../features/notifications/native-push-registration.ts";

const device = "11111111-1111-4111-8111-111111111111";
const token: NativePushToken = { platform: "FCM", handle: "native-fcm-token" };

test("disabled native push and denied permission never register a device", async () => {
  let requests = 0;
  const source: NativePushSource = { permission: async () => { requests++; return false; }, token: async () => token, subscribe: () => () => {} };
  const registration = nativePushRegistration({ register: async () => { throw Error("unexpected"); }, remove: async () => {} });
  assert.equal(await registration.start(device, source, false), false);
  assert.equal(requests, 0);
  assert.equal(await registration.start(device, source, true), false);
  await registration.stop();
});

test("logout drains a token write before removal and ignores subsequent native refreshes", async () => {
  const calls: string[] = [];
  let refresh: ((token: NativePushToken) => void) | undefined;
  let finish: (() => void) | undefined;
  const pending = new Promise<void>((resolve) => { finish = resolve; });
  const registration = nativePushRegistration({
    register: async () => { calls.push("register"); await pending; calls.push("registered"); },
    remove: async (id, platform) => { assert.equal(id, device); assert.equal(platform, "FCM"); calls.push("removed"); },
  });
  const source: NativePushSource = { permission: async () => true, token: async () => token, subscribe: (listener) => { refresh = listener; return () => calls.push("unsubscribed"); } };
  const starting = registration.start(device, source, true);
  while (!calls.includes("register")) await new Promise((resolve) => setTimeout(resolve, 0));
  const stopping = registration.stop();
  refresh?.({ ...token, handle: "rotated-after-logout" });
  finish?.();
  await Promise.all([starting, stopping]);
  assert.deepEqual(calls, ["register", "unsubscribed", "registered", "removed"]);
});

test("an ambiguous registration failure is still removed with the original account credentials", async () => {
  let removed = false;
  const registration = nativePushRegistration({ register: async () => { throw Error("response lost"); }, remove: async () => { removed = true; } });
  const source: NativePushSource = { permission: async () => true, token: async () => token, subscribe: () => () => {} };
  assert.equal(await registration.start(device, source, true), false);
  await registration.stop();
  assert.equal(removed, true);
});

test("a permission reply from a previous account cannot register on the next account's device", async () => {
  let allow: ((value: boolean) => void) | undefined;
  const permission = new Promise<boolean>((resolve) => { allow = resolve; });
  const registrations: string[] = [];
  const registration = nativePushRegistration({ register: async (id) => { registrations.push(id); }, remove: async () => {} });
  const old: NativePushSource = { permission: () => permission, token: async () => token, subscribe: () => () => {} };
  const starting = registration.start(device, old, true);
  await registration.stop();
  const next = "22222222-2222-4222-8222-222222222222";
  const current: NativePushSource = { ...old, permission: async () => true };
  assert.equal(await registration.start(next, current, true), true);
  allow?.(true);
  assert.equal(await starting, false);
  assert.deepEqual(registrations, [next]);
  await registration.stop();
});
