// Chat titles (owner reports, 19 November 2026):
// 1. A group created with a name shows that name everywhere, for the creator and every member,
//    MANAGED and PRIVATE_E2EE alike. A private group's name never reaches the server in the
//    clear: it travels inside the end-to-end encrypted group and each member device reads it.
// 2. A direct chat is named after the other person (your saved alias for them first), never a
//    member list, never including you, and "CF Meet user" only while no name is known.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";
import * as privateShares from "../src/lib/messaging/private-shares.ts";
import {
  UNKNOWN_PERSON, conversationTitle, decodeGroupTitle, directCounterpartLabel, encodeGroupTitle, groupTitleFromRecords, isGroupTitleText, normalizeTitle,
} from "../src/lib/messaging/conversation-title.ts";

const ME = "00000000-0000-4000-8000-00000000000a";
const AINZ = "00000000-0000-4000-8000-00000000000b";
const BO = "00000000-0000-4000-8000-00000000000c";
const CONV = "11111111-1111-4111-8111-111111111111";

// ------------------------------------------------------------------ pure rules

test("a direct chat is the other person: alias first, never a list, never yourself", () => {
  const names = { [AINZ]: "Ainzkōji" };
  for (const members of [[ME, AINZ], [AINZ, ME]]) {
    assert.equal(conversationTitle({ contextClass: "DIRECT", members, me: ME, names }), "Ainzkōji");
  }
  // Your own saved contact name wins over their display name (Hub counterpart).
  assert.equal(conversationTitle({ contextClass: "DIRECT", members: [ME, AINZ], me: ME, names, counterpart: { self: false, alias: "Ainz", displayName: "Ainzkōji" } }), "Ainz");
  // The Hub knows the name even when the names lookup did not: never the generic label then.
  assert.equal(conversationTitle({ contextClass: "DIRECT", members: [ME, AINZ], me: ME, names: {}, counterpart: { self: false, displayName: "Ainzkōji" } }), "Ainzkōji");
  // A counterpart that is yourself is ignored.
  assert.equal(conversationTitle({ contextClass: "DIRECT", members: [ME, AINZ], me: ME, names, counterpart: { self: true } }), "Ainzkōji");
  // Nobody known yet: the generic label, alone (never "X, CF Meet user").
  assert.equal(conversationTitle({ contextClass: "DIRECT", members: [ME, AINZ], me: ME, names: {} }), UNKNOWN_PERSON);
  // Even if the member list held an extra unnamed principal, the title is one person.
  const title = conversationTitle({ contextClass: "DIRECT", members: [AINZ, BO, ME], me: ME, names });
  assert.equal(title, "Ainzkōji");
  assert.ok(!title.includes(","));
  // A direct chat ignores any server title (it is about a person).
  assert.equal(conversationTitle({ contextClass: "DIRECT", serverTitle: "Old", members: [ME, AINZ], me: ME, names }), "Ainzkōji");
  assert.equal(directCounterpartLabel({ members: [ME], me: ME, names: {} }), null);
});

test("a group shows its name; without one, its other members", () => {
  const names = { [AINZ]: "Ainzkōji", [BO]: "Bo" };
  assert.equal(conversationTitle({ contextClass: "GROUP", serverTitle: "  Design  ", members: [ME, AINZ], me: ME, names }), "Design");
  assert.equal(conversationTitle({ contextClass: "GROUP", groupTitle: "Weekend trip", members: [ME, AINZ, BO], me: ME, names }), "Weekend trip");
  assert.equal(conversationTitle({ contextClass: "GROUP", members: [ME, AINZ, BO], me: ME, names }), "Ainzkōji, Bo");
  assert.equal(conversationTitle({ contextClass: "GROUP", members: [ME], me: ME, names }), "Group conversation");
  // An invalid name is never shown.
  assert.equal(conversationTitle({ contextClass: "GROUP", groupTitle: "bad\u0007", members: [ME, BO], me: ME, names }), "Bo");
});

test("the private group name envelope round-trips and is never confused with a message", () => {
  const text = encodeGroupTitle("  Weekend trip ");
  assert.ok(isGroupTitleText(text));
  assert.equal(decodeGroupTitle(text), "Weekend trip");
  for (const plain of ["Weekend trip", '{"title":"x"}', "cfmeet:group-title:v1{\"title\":\"x\"}", "", null, 42]) {
    assert.equal(decodeGroupTitle(plain), null);
    assert.equal(isGroupTitleText(plain), false);
  }
  assert.equal(decodeGroupTitle(text.slice(0, -2)), null);
  assert.throws(() => encodeGroupTitle("   "));
  assert.throws(() => encodeGroupTitle("x".repeat(121)));
  assert.equal(normalizeTitle("x".repeat(120)), "x".repeat(120));
  assert.equal(normalizeTitle("a\nb"), null);
});

test("the latest name wins; repeating the same name is not a change", () => {
  const r = (sequence, title) => ({ sequence, senderPrincipalId: ME, text: title === null ? "hello" : encodeGroupTitle(title) });
  const { title, changes } = groupTitleFromRecords([r(1, "A"), r(2, null), r(3, "A"), r(4, "B"), r(5, "B")]);
  assert.equal(title, "B");
  assert.deepEqual([...changes.entries()], [[1, "A"], [4, "B"]]);
  assert.equal(groupTitleFromRecords([r(1, null)]).title, null);
});

// ------------------------------------------------------------------ engine

function load(path, dependencies = {}, globals = {}) {
  const exports = {};
  const js = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true },
  }).outputText;
  vm.runInNewContext(js, {
    exports,
    require: (name) => {
      if (name in dependencies) return dependencies[name];
      throw new Error(`Unexpected dependency ${name}`);
    },
    URL, URLSearchParams, TextEncoder, TextDecoder, Uint8Array, Promise, Map, Set, JSON, Date, Math, Number, Error, Array, Object, String, Boolean,
    setTimeout, clearTimeout, setInterval, clearInterval, atob, btoa, crypto: globalThis.crypto, console,
    process: { env: {} },
    ...globals,
  });
  return exports;
}

const titles = load("../src/lib/messaging/conversation-title.ts");
const errors = load("../src/lib/messaging/errors.ts");
const urls = load("../src/lib/messaging/urls.ts");
const b64u = {
  enc: (b) => Buffer.from(b).toString("base64url"),
  dec: (s) => new Uint8Array(Buffer.from(s, "base64url")),
};

/** A MessagingEngine with the relay, identity API and crypto worker replaced by fakes. */
function engineWith({ routes = {}, names = {}, history = [] } = {}) {
  const calls = [];
  const crypto = [];
  const relay = async (method, path, body) => {
    calls.push({ method, path, body });
    for (const [pattern, answer] of Object.entries(routes)) {
      const [m, p] = pattern.split(" ");
      if (m === method && path.startsWith(p)) return typeof answer === "function" ? answer(body, path) : structuredClone(answer);
    }
    if (method === "GET" && path.includes("/messages?")) return { messages: [] };
    if (method === "GET" && path.startsWith("v1/messaging/principals/")) return { devices: [{ deviceId: "dev-me", status: "ACTIVE", signaturePublicKey: "AQ" }] };
    return {};
  };
  const identityApi = {
    names: async (ids) => ({ principals: ids.filter((id) => names[id]).map((id) => ({ principalId: id, ...names[id] })) }),
    resolve: async () => ({ people: [] }),
  };
  const fakeCrypto = {
    async call(method, args) {
      crypto.push({ method, args });
      switch (method) {
        case "hasGroup": return true;
        case "members": return ["dev-me", "dev-ainz"];
        case "inspectKeyPackage": return { deviceId: "dev-ainz", signatureKey: new Uint8Array([1]) };
        case "addDevices": return { commit: new Uint8Array([2]), welcome: new Uint8Array([3]), epoch: 1 };
        case "encrypt": return { ciphertext: new Uint8Array([9, 9]), epoch: 1 };
        case "history": return history;
        case "historyPage": return { records: history, nextBeforeSequence: null };
        case "drafts": return [];
        case "cursor": return 0;
        case "knownTreeSize": return null;
        default: return null;
      }
    },
    terminate() {},
  };
  class NoTransport {}
  const mod = load("../src/lib/messaging/engine.ts", {
    "@/services/messaging-realtime": { MessagingRealtimeTransport: NoTransport },
    "@/services/messaging-session": { messagingClientAccessToken: () => "", messagingSessionDeviceToken: () => "" },
    "@/services/v2/messaging.service": { messagingService: identityApi, relay },
    "./api": { b64u },
    "./private-shares": privateShares,
    "./errors": errors,
    "@/lib/auth/account-device": { accountDevice: {} },
    "@/lib/auth/session": { accountSession: { subscribe: () => () => {} } },
    "./crypto-client": { CryptoClient: class {}, CryptoWorkerError: class extends Error {} },
    "./managed-store": { ManagedConversationState: class {}, emptyRefresh: () => ({}) },
    "./outbox": { MessageOutbox: class {}, browserOutboxStorage: () => ({}), flushOutbox: async () => 0 },
    "./routes": { reactionSegment: (r) => r },
    "./urls": urls,
    "@/lib/auth/device-label": { browserDeviceLabel: () => "Browser" },
    "./conversation-title": titles,
    "./e2ee-membership": load("../src/lib/messaging/e2ee-membership.ts"),
    "./local-authenticator": load("../src/lib/messaging/local-authenticator.ts"),
    "./member-pages": load("../src/lib/messaging/member-pages.ts"),
    "./chat-list": load("../src/lib/messaging/chat-list.ts"),
    "./rich-content": load("../src/lib/messaging/rich-content.ts"),
    "./private-history": load("../src/lib/messaging/private-history.ts"),
  });
  const engine = new mod.MessagingEngine();
  engine.crypto = fakeCrypto;
  engine.set({ identity: { principalId: ME, tenantId: "t", applicationId: "a", environmentId: "e" }, phase: "ready", deviceId: "dev-me" });
  return { engine, calls, crypto };
}

const groupConv = (members = [ME, AINZ, BO], extra = {}) => ({
  conversationId: CONV, contextClass: "GROUP", securityProfile: "PRIVATE_E2EE", lifecycleState: "ACTIVE", createdAt: "2026-11-19T10:00:00Z",
  encryption: { mlsGroupId: "AAAA", serverCanReadContent: false }, members: members.map((principalId, i) => ({ principalId, role: i === 0 ? "OWNER" : "MEMBER" })),
  ownerPrincipalId: members[0], version: 1, title: null, ...extra,
});
const settle = () => new Promise((resolve) => setTimeout(resolve, 20));

test("creating a private group carries its name inside the encrypted group, never to the server", async () => {
  const conv = groupConv([ME]);
  const { engine, calls, crypto } = engineWith({
    names: { [AINZ]: { displayName: "Ainzkōji" }, [BO]: { displayName: "Bo" } },
    routes: {
      [`POST v1/conversations/${CONV}/mls/key-package-claims`]: { keyPackages: [{ principalId: AINZ, deviceId: "dev-ainz", keyPackage: "AQ" }] },
      "POST v1/conversations/": {},
      "POST v1/conversations": conv,
      "GET v1/conversations/11111111-1111-4111-8111-111111111111/members": { members: [ME, AINZ, BO].map((principalId) => ({ principalId, state: "ACTIVE" })) },
      "GET v1/conversations?": () => ({ conversations: [groupConv()], nextCursor: null }),
    },
  });
  await engine.createConversation([], "PRIVATE_E2EE", { cfMeetIds: ["CF-AAAAAA", "CF-BBBBBB"], title: "  Weekend trip " });
  await settle();
  const create = calls.find((c) => c.method === "POST" && c.path === "v1/conversations");
  assert.equal(create.body.contextClass, "GROUP");
  assert.equal("title" in create.body, false, "a private group's name is never sent in the clear");
  assert.ok(!calls.some((c) => c.method === "PATCH"), "no server rename for a private group");
  for (const c of calls) assert.ok(!JSON.stringify(c.body ?? {}).includes("Weekend trip"), `plaintext name leaked in ${c.method} ${c.path}`);
  // It was encrypted for the group (after the members' devices were added) and posted as ciphertext.
  const added = crypto.findIndex((c) => c.method === "addDevices");
  const encrypted = crypto.findIndex((c) => c.method === "encrypt");
  assert.ok(added >= 0 && encrypted > added);
  assert.equal(titles.decodeGroupTitle(crypto[encrypted].args.payload.text), "Weekend trip");
  const sent = calls.find((c) => c.method === "POST" && c.path === `v1/conversations/${CONV}/messages`);
  assert.ok(sent.body.ciphertext && !("text" in sent.body));
  // The creator sees the name in the list (and so the header / chat info).
  assert.equal(engine.getSnapshot().conversations[0].title, "Weekend trip");
});

test("every member device shows the private group's name read from the encrypted history", async () => {
  const history = [
    { sequence: 1, messageId: "m1", senderPrincipalId: AINZ, status: "DECRYPTED", kind: "APPLICATION", createdAt: "2026-11-19T10:00:01Z", payload: { t: "text", text: encodeGroupTitle("Weekend trip") } },
    { sequence: 2, messageId: "m2", senderPrincipalId: AINZ, status: "DECRYPTED", kind: "APPLICATION", createdAt: "2026-11-19T10:00:02Z", payload: { t: "text", text: "Hi all" } },
    // Re-shared for a newly added device: silent.
    { sequence: 3, messageId: "m3", senderPrincipalId: BO, status: "DECRYPTED", kind: "APPLICATION", createdAt: "2026-11-19T10:00:03Z", payload: { t: "text", text: encodeGroupTitle("Weekend trip") } },
  ];
  const { engine } = engineWith({ history, names: { [AINZ]: { displayName: "Ainzkōji" }, [BO]: { displayName: "Bo" } } });
  engine.server.set(CONV, groupConv([AINZ, ME, BO]));
  await engine.renderConversations();
  assert.equal(engine.getSnapshot().conversations[0].title, "Ainzkōji, Bo", "before the history is read: the members");
  await engine.renderE2ee(CONV);
  const view = engine.getSnapshot().conversations[0];
  assert.equal(view.title, "Weekend trip");
  assert.equal(view.groupTitle, "Weekend trip");
  const messages = engine.getSnapshot().messages[CONV];
  assert.deepEqual(JSON.parse(JSON.stringify(messages.map((m) => [m.state, m.text, m.note]))), [
    ["system", null, "Ainzkōji named the group “Weekend trip”"],
    ["delivered", "Hi all", null],
  ]);
  // The name survives a list refresh.
  await engine.renderConversations();
  assert.equal(engine.getSnapshot().conversations[0].title, "Weekend trip");
});

test("renaming a private group sends the new name encrypted; managed groups use the server title", async () => {
  const { engine, calls, crypto } = engineWith({ routes: { "GET v1/conversations?": () => ({ conversations: [groupConv()], nextCursor: null }) } });
  engine.server.set(CONV, groupConv());
  await engine.renderConversations();
  await engine.renameConversation(CONV, "Ski club");
  await settle();
  assert.ok(!calls.some((c) => c.method === "PATCH"));
  assert.equal(titles.decodeGroupTitle(crypto.find((c) => c.method === "encrypt").args.payload.text), "Ski club");
  assert.equal(engine.getSnapshot().conversations[0].title, "Ski club");
  await assert.rejects(engine.renameConversation(CONV, "   "));

  const managed = { ...groupConv(), securityProfile: "MANAGED", encryption: { mlsGroupId: null, serverCanReadContent: true } };
  const m = engineWith({ routes: { "POST v1/conversations/": {}, "GET v1/conversations/": { members: [] }, "POST v1/conversations": managed, "GET v1/conversations?": () => ({ conversations: [{ ...managed, title: "Design" }], nextCursor: null }) } });
  await m.engine.createConversation([], "MANAGED", { cfMeetIds: ["CF-AAAAAA", "CF-BBBBBB"], title: "Design" });
  const create = m.calls.find((c) => c.method === "POST" && c.path === "v1/conversations");
  assert.equal(create.body.title, "Design");
  assert.equal(m.engine.getSnapshot().conversations[0].title, "Design");
});

test("a private direct chat is titled with the other person only (alias first)", async () => {
  const direct = { ...groupConv([AINZ, ME]), contextClass: "DIRECT" };
  const a = engineWith({ names: { [AINZ]: { displayName: "Ainzkōji", alias: "Ainz" }, [ME]: { displayName: "Me Myself" } } });
  a.engine.server.set(CONV, direct);
  await a.engine.renderConversations();
  assert.equal(a.engine.getSnapshot().conversations[0].title, "Ainz");

  const b = engineWith({ names: { [AINZ]: { displayName: "Ainzkōji" } } });
  b.engine.server.set(CONV, direct);
  await b.engine.renderConversations();
  assert.equal(b.engine.getSnapshot().conversations[0].title, "Ainzkōji");

  const c = engineWith({ names: {} });
  c.engine.server.set(CONV, direct);
  await c.engine.renderConversations();
  assert.equal(c.engine.getSnapshot().conversations[0].title, UNKNOWN_PERSON);
});
