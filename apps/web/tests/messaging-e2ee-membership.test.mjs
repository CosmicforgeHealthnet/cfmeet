// Private chats (MLS) behave like WhatsApp: a sender adds every device the others
// registered, a device keeps KeyPackages published so it can be added while offline,
// pending removals never block a chat, and a message this device cannot read (yet) shows
// a clear placeholder — never a blank bar or an empty chat.
import assert from "node:assert/strict";
import test from "node:test";
import {
  KEY_PACKAGE_STOCK,
  KEY_PACKAGE_TARGET,
  WAITING_PREVIEW,
  beforeJoinNote,
  chatPreview,
  joinerEntry,
  keyPackagesToPublish,
  missingDevices,
  removalPlan,
} from "../src/lib/messaging/e2ee-membership.ts";

const alice = "aaaaaaaa-0000-4000-8000-000000000001";
const bob = "bbbbbbbb-0000-4000-8000-000000000002";
const carol = "cccccccc-0000-4000-8000-000000000003";
const dev = (n) => `dddddddd-0000-4000-8000-${String(n).padStart(12, "0")}`;

test("a device keeps its KeyPackage stock published (so others can add it while it is offline)", () => {
  assert.equal(keyPackagesToPublish(undefined), 0, "a server that does not report the stock is left alone");
  assert.equal(keyPackagesToPublish(null), 0);
  assert.equal(keyPackagesToPublish(KEY_PACKAGE_STOCK), 0);
  assert.equal(keyPackagesToPublish(50), 0);
  assert.equal(keyPackagesToPublish(0), KEY_PACKAGE_TARGET);
  assert.equal(keyPackagesToPublish(KEY_PACKAGE_STOCK - 1), KEY_PACKAGE_TARGET - KEY_PACKAGE_STOCK + 1);
  assert.ok(keyPackagesToPublish(-5) <= 20, "never more than one publication allows");
});

test("the sender adds every ACTIVE device of ACTIVE members that is not in the tree", () => {
  const devices = {
    [alice]: [{ deviceId: dev(1), status: "ACTIVE" }, { deviceId: dev(2), status: "ACTIVE" }],
    [bob]: [{ deviceId: dev(3), status: "ACTIVE" }, { deviceId: dev(4), status: "ACTIVE" }, { deviceId: dev(5), status: "REVOKED" }],
    [carol]: [{ deviceId: dev(6), status: "ACTIVE" }],
  };
  const members = [{ principalId: alice }, { principalId: bob, state: "ACTIVE" }, { principalId: carol, state: "REMOVED" }];
  const inTree = new Set([dev(1), dev(3)]);
  const r = missingDevices(members, (p) => devices[p] ?? [], inTree, dev(1));
  // Alice's second browser and Bob's new phone; never a revoked device or a removed member's.
  assert.deepEqual(r.principals, [alice, bob]);
  assert.deepEqual(r.deviceIds, [dev(2), dev(4)]);
  assert.deepEqual(r.activeInTree.sort(), [dev(1), dev(3)]);
  // This device itself (not yet in the tree: it joins by itself) is never "added".
  const own = missingDevices([{ principalId: alice }], (p) => devices[p], new Set([dev(3)]), dev(1));
  assert.deepEqual(own.deviceIds, [dev(2)]);
  assert.deepEqual(missingDevices(members, (p) => devices[p] ?? [], new Set([dev(1), dev(2), dev(3), dev(4)]), dev(1)).principals, []);
});

test("pending removals are always committed: leaves removed, devices outside the tree attested", () => {
  const tree = new Set([dev(1), dev(3)]);
  assert.equal(removalPlan([], tree, dev(1)), null);
  assert.deepEqual(removalPlan([dev(3)], tree, dev(1)), { remove: [dev(3)], attest: [dev(3)] });
  // A device revoked before it ever joined blocks nothing: a key update attests it.
  assert.deepEqual(removalPlan([dev(9)], tree, dev(1)), { remove: [], attest: [dev(9)] });
  assert.deepEqual(removalPlan([dev(3), dev(9)], tree, dev(1)), { remove: [dev(3)], attest: [dev(3), dev(9)] });
  // This device is the one to remove: it never commits its own removal.
  assert.equal(removalPlan([dev(1)], tree, dev(1)), null);
});

test("a verified joiner is named to the worker with its exact key", () => {
  assert.equal(joinerEntry(dev(7), new Uint8Array([0, 1, 0xab, 0xff])), `${dev(7)}:0001abff`);
});

test("older history: a clear placeholder, pointing to the other device when there is one", () => {
  assert.match(beforeJoinNote(true), /other device/);
  assert.match(beforeJoinNote(false), /before this device was linked/);
  assert.doesNotMatch(beforeJoinNote(false), /other device/);
});

test("the chat list never shows a blank bar for a private message", () => {
  const summary = { lifecycle: "ACTIVE", previewText: null, attachmentCount: 0 };
  // The incident: a private chat this browser is not in yet, 2 unread messages.
  assert.equal(chatPreview({ e2ee: true, groupReady: false, loaded: false, latest: undefined, summary }), WAITING_PREVIEW);
  assert.equal(chatPreview({ e2ee: true, groupReady: false, loaded: true, latest: undefined, summary }), WAITING_PREVIEW);
  // Rendered, nothing readable: still a label.
  assert.equal(chatPreview({ e2ee: true, groupReady: true, loaded: true, latest: undefined, summary }), WAITING_PREVIEW);
  // Only while a readable chat is loading for the first time: the brief placeholder bar.
  assert.equal(chatPreview({ e2ee: true, groupReady: true, loaded: false, latest: undefined, summary }), null);
  // The latest decrypted message text, a file, or the label for one that can't be read.
  const latest = (extra) => ({ text: null, file: null, note: null, state: "delivered", ...extra });
  assert.equal(chatPreview({ e2ee: true, groupReady: true, loaded: true, latest: latest({ text: "hi" }), summary }), "hi");
  assert.equal(chatPreview({ e2ee: true, groupReady: true, loaded: true, latest: latest({ file: { name: "a.pdf" } }), summary }), "📎 a.pdf");
  for (const state of ["unreadable", "before-join"]) {
    assert.equal(chatPreview({ e2ee: true, groupReady: true, loaded: true, latest: latest({ state, note: "a long explanation" }), summary }), WAITING_PREVIEW);
  }
  assert.equal(chatPreview({ e2ee: true, groupReady: true, loaded: true, latest: latest({ state: "removed", note: "Message deleted" }), summary }), "Message deleted");
  // Server-readable chats and deleted / empty chats as before.
  assert.equal(chatPreview({ e2ee: false, groupReady: false, loaded: false, latest: undefined, summary: { ...summary, previewText: "managed" } }), "managed");
  assert.equal(chatPreview({ e2ee: false, groupReady: false, loaded: false, latest: undefined, summary: { ...summary, attachmentCount: 1 } }), "📎 Attachment");
  assert.equal(chatPreview({ e2ee: true, groupReady: false, loaded: false, latest: undefined, summary: { ...summary, lifecycle: "DELETED" } }), "Message deleted");
  assert.equal(chatPreview({ e2ee: true, groupReady: false, loaded: false, latest: undefined, summary: null }), "No messages yet");
});
