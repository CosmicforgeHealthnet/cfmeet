import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import vm from "node:vm";
import ts from "typescript";

function load(path, dependencies = {}) {
  const exports = {};
  const source = ts.transpileModule(readFileSync(new URL(path, import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, esModuleInterop: true },
  }).outputText;
  vm.runInNewContext(source, { exports, require: name => {
    if (name in dependencies) return dependencies[name];
    throw new Error(`Unexpected dependency ${name}`);
  } });
  return exports;
}

function collaboration() {
  const requests = [];
  const api = Object.fromEntries(["get", "post", "patch", "delete"].map(method => [method, async (url, data) => {
    requests.push({ method, url, data });
    return { data: { changed: true } };
  }]));
  return {
    requests,
    service: load("../src/services/v2/collaboration.service.ts", {
      "./api": { __esModule: true, default: api },
    }).collaborationService,
  };
}

const plain = value => JSON.parse(JSON.stringify(value));

test("Part F collaboration operations use conference-scoped v2 routes and camelCase payloads", async () => {
  const { requests, service } = collaboration();
  const conferenceId = "conf/1", participantId = "participant/1";
  await service.getState(conferenceId);
  await service.updatePolicy(conferenceId, { collaborationVersion: 4, changes: { audioLocked: true } });
  await service.getMediaCredential(conferenceId, { participantSessionId: "session-1" });
  await service.lockAudio(conferenceId, participantId); await service.unlockAudio(conferenceId, participantId);
  await service.lockVideo(conferenceId, participantId); await service.unlockVideo(conferenceId, participantId);
  await service.lockScreenShare(conferenceId, participantId); await service.unlockScreenShare(conferenceId, participantId);
  await service.stopScreenShare(conferenceId, participantId);
  await service.addSpotlight(conferenceId, { participantId }); await service.removeSpotlight(conferenceId, participantId);
  await service.getMessages(conferenceId, { before: "cursor-1", limit: 20 });
  await service.sendMessage(conferenceId, { content: "Hello" }); await service.deleteMessage(conferenceId, "message/1");
  await service.updateChatPolicy(conferenceId, { collaborationVersion: 4, chatEnabled: false });
  await service.restrictChat(conferenceId, participantId); await service.unrestrictChat(conferenceId, participantId);
  await service.sendReaction(conferenceId, { reaction: "clap" });
  await service.getRaisedHands(conferenceId); await service.raiseHand(conferenceId); await service.lowerHand(conferenceId);
  await service.lowerOtherHand(conferenceId, participantId);

  assert.deepEqual(requests.map(({ method, url }) => [method, url]), [
    ["get", "/conferences/conf%2F1/collaboration"],
    ["patch", "/conferences/conf%2F1/collaboration-policy"],
    ["post", "/conferences/conf%2F1/media-credential"],
    ["post", "/conferences/conf%2F1/participants/participant%2F1/audio-lock"],
    ["delete", "/conferences/conf%2F1/participants/participant%2F1/audio-lock"],
    ["post", "/conferences/conf%2F1/participants/participant%2F1/video-lock"],
    ["delete", "/conferences/conf%2F1/participants/participant%2F1/video-lock"],
    ["post", "/conferences/conf%2F1/participants/participant%2F1/screen-share-lock"],
    ["delete", "/conferences/conf%2F1/participants/participant%2F1/screen-share-lock"],
    ["post", "/conferences/conf%2F1/screen-share/participant%2F1/stop"],
    ["post", "/conferences/conf%2F1/spotlights"],
    ["delete", "/conferences/conf%2F1/spotlights/participant%2F1"],
    ["get", "/conferences/conf%2F1/messages"],
    ["post", "/conferences/conf%2F1/messages"],
    ["delete", "/conferences/conf%2F1/messages/message%2F1"],
    ["patch", "/conferences/conf%2F1/chat-policy"],
    ["post", "/conferences/conf%2F1/participants/participant%2F1/chat-restriction"],
    ["delete", "/conferences/conf%2F1/participants/participant%2F1/chat-restriction"],
    ["post", "/conferences/conf%2F1/reactions"],
    ["get", "/conferences/conf%2F1/hand-raise"],
    ["post", "/conferences/conf%2F1/hand-raise"],
    ["delete", "/conferences/conf%2F1/hand-raise"],
    ["delete", "/conferences/conf%2F1/participants/participant%2F1/hand-raise"],
  ]);
  assert.deepEqual(plain(requests[1].data), { collaborationVersion: 4, changes: { audioLocked: true } });
  assert.deepEqual(plain(requests[2].data), { participantSessionId: "session-1" });
  assert.deepEqual(plain(requests[12].data), { params: { before: "cursor-1", limit: 20 } });
  assert.deepEqual(plain(requests[13].data), { content: "Hello" });
  assert.deepEqual(requests[14].data, undefined);
  assert.deepEqual(requests[16].data, undefined);
});

test("collaboration policy enums match the backend contract", () => {
  const types = readFileSync(new URL("../src/types/v2/index.ts", import.meta.url), "utf8");
  assert.match(types, /ChatSendMode\s*=\s*"EVERYONE"\s*\|\s*"HOSTS_ONLY"/);
  assert.match(types, /PrivateChatMode\s*=\s*"EVERYONE"\s*\|\s*"HOSTS_ONLY"\s*\|\s*"DISABLED"/);
  assert.match(types, /screenSharePolicy\?:\s*ScreenSharePolicy/);
  assert.match(types, /audioLocked\?:\s*boolean/);
  assert.match(types, /videoLocked\?:\s*boolean/);
  assert.match(types, /participantSessionId:\s*string/);
  assert.match(readFileSync(new URL("../src/services/v2/collaboration.service.ts", import.meta.url), "utf8"), /delete<ChangedResponse>/);
});

test("media credential refresh reconnects without closing the participant session", () => {
  const controls = readFileSync(new URL("../src/components/room/conference-controls.tsx", import.meta.url), "utf8");
  const room = readFileSync(new URL("../src/components/room/live-room.tsx", import.meta.url), "utf8");
  assert.match(controls, /participantSessionId:\s*sessionId/);
  assert.match(controls, /room\.disconnect\(false\)/);
  assert.match(controls, /room\.connect\(result\.rtc\.url,\s*result\.rtc\.credential\)/);
  assert.match(room, /if\s*\(!isRefreshingCredentialRef\.current\)\s*\{\s*void handleDisconnect/);
});

test("media actions follow role and policy authority; missing roles never gain controls", () => {
  const authority = load("../src/lib/meeting-authority.ts");
  for (const role of ["OWNER", "HOST", "CO_HOST"]) {
    assert.equal(authority.meetingAllows(role, "participant.audio_lock"), true);
    assert.equal(authority.meetingAllows(role, "participant.video_lock"), true);
    assert.equal(authority.meetingAllows(role, "participant.screen_share_stop"), true);
    assert.equal(authority.meetingAllows(role, "participant.spotlight"), true);
  }
  assert.equal(authority.meetingAllows("MODERATOR", "participant.audio_lock"), true);
  assert.equal(authority.meetingAllows("MODERATOR", "participant.audio_lock", { overrides: { MODERATOR: { "participant.audio_lock": false } } }), false);
  assert.equal(authority.meetingAllows("PARTICIPANT", "participant.audio_lock", { overrides: { PARTICIPANT: { "participant.audio_lock": true } } }), false);
  assert.equal(authority.meetingAllows(undefined, "participant.audio_lock"), false);
});
