// Section menus (brief §20; ADR 0073): each section's ⋮ menu lists only what the signed-in
// person may do, wired to existing screens; sections not deployed yet stay hidden.
import assert from "node:assert/strict";
import test from "node:test";
import { channelMenu, chatsMenu, communitiesMenu, myStatusMenu, ROUTES, updatesMenu } from "../src/lib/section-menus.ts";

const keys = (items) => items.map((i) => i.key);
const off = { events: false, communities: false };
const on = { events: true, communities: true };

test("Updates: create, discover, privacy, starred, events, settings", () => {
  assert.deepEqual(keys(updatesMenu({ signedIn: true, createChannel: true }, on)), ["create-channel", "discover-channels", "status-privacy", "starred", "events", "settings"]);
  assert.deepEqual(keys(updatesMenu({ signedIn: true, createChannel: false }, off)), ["discover-channels", "status-privacy", "starred", "settings"]);
  assert.deepEqual(updatesMenu({ signedIn: false, createChannel: true }, on), []);
  assert.equal(updatesMenu({ signedIn: true, createChannel: true }, on).find((i) => i.key === "starred").href, ROUTES.starred);
});

test("Chats: new group and conversation only with messaging; lists, starred, events, settings", () => {
  assert.deepEqual(keys(chatsMenu({ messaging: true, lists: true }, on)), ["new-group", "new-chat", "lists", "starred", "events", "settings"]);
  assert.deepEqual(keys(chatsMenu({ messaging: false, lists: false }, off)), ["starred", "settings"]);
  assert.equal(chatsMenu({ messaging: true, lists: true }, off).find((i) => i.key === "new-chat").label, "New conversation");
});

test("Communities: nothing until the section exists", () => {
  assert.deepEqual(communitiesMenu({ createCommunity: true }, off), []);
  assert.deepEqual(keys(communitiesMenu({ createCommunity: true }, on)), ["new-community", "events", "settings"]);
  assert.deepEqual(keys(communitiesMenu({ createCommunity: false }, { events: false, communities: true })), ["settings"]);
});

test("My status: live statuses vs uploads still being added", () => {
  assert.deepEqual(keys(myStatusMenu({ kind: "live", readReceipts: true, allowSharing: true })), ["privacy", "viewers", "sharing", "replies", "delete"]);
  const noReceipts = myStatusMenu({ kind: "live", readReceipts: false, allowSharing: false });
  assert.deepEqual(keys(noReceipts), ["privacy", "sharing", "replies", "delete"]);
  assert.equal(noReceipts.find((i) => i.key === "sharing").label, "Allow sharing");
  assert.deepEqual(keys(myStatusMenu({ kind: "pending", failed: true, fatal: false })), ["retry", "cancel"]);
  assert.deepEqual(keys(myStatusMenu({ kind: "pending", failed: true, fatal: true })), ["cancel"]);
  assert.deepEqual(keys(myStatusMenu({ kind: "pending", failed: false, fatal: false })), ["cancel"]);
});

test("A channel: owners manage and delete; followers get notifications, report and unfollow", () => {
  const base = { notifications: true, report: true, share: true, manage: true };
  assert.deepEqual(keys(channelMenu({ ...base, isOwner: true, following: false })), ["info", "share", "manage", "delete"]);
  assert.deepEqual(keys(channelMenu({ ...base, isOwner: false, following: true })), ["info", "notifications", "share", "report", "unfollow"]);
  assert.deepEqual(keys(channelMenu({ ...base, isOwner: false, following: false })), ["info", "share", "report"]);
  assert.deepEqual(keys(channelMenu({ ...base, isOwner: false, following: true, admin: true })), ["info", "notifications", "share", "report", "unfollow", "manage"]);
  // Features not deployed on this build stay hidden.
  assert.deepEqual(keys(channelMenu({ notifications: false, report: false, share: false, manage: false, isOwner: false, following: true })), ["info", "unfollow"]);
  assert.deepEqual(keys(channelMenu({ notifications: false, report: false, share: true, manage: false, isOwner: true, following: false })), ["info", "share", "delete"]);
});
