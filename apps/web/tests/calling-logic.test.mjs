// Web calling client rules (Blueprint v1.0 Section 17): party control (CAL-002), media
// reconnect state machine (CAL-015), production guard + credential lifetime (CAL-008/CAL-020),
// incoming-call presentation (CAL-004). Run: node --experimental-strip-types --test tests/calling-logic.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { mayControl, ownSession, visibleControls } from "../src/lib/calling/call-permissions.ts";
import { DEFAULT_RECONNECT_POLICY, INITIAL_MEDIA_STATE, backoffDelay, isRetryableCredentialFailure, mediaReducer, mediaStatusLabel } from "../src/lib/calling/media-reconnect.ts";
import { CallingConfigurationError, callingBaseUrl, callingRuntimeMode, renewalDelay } from "../src/lib/calling/runtime-guard.ts";
import { callerLabel, offerHeadline, ringingOffers } from "../src/lib/calling/incoming-call.ts";
import { signalingSocketUrl } from "../src/lib/calling/origin.mjs";

const me = "me";
const call = (overrides = {}) => ({
  state: "CONNECTED",
  initiatorPrincipalId: "caller",
  participants: [
    { participantId: "p-caller", principalId: "caller", role: "INITIATOR", endedAt: null },
    { participantId: "p-me", principalId: me, role: "RECIPIENT", endedAt: null },
  ],
  sessions: [{ sessionId: "s-me", participantId: "p-me", endpointId: "e-me", endedAt: null, removedAt: null }],
  ...overrides,
});
const all = () => true;

// ---- CAL-002 party rules ----

test("the callee may end, hold, resume and transfer, but not cancel", () => {
  for (const op of ["end", "hold", "resume", "transfer", "leave"]) assert.equal(mayControl(call(), me, op), true, op);
  assert.equal(mayControl(call(), me, "cancel"), false);
  assert.equal(mayControl(call({ state: "RINGING" }), me, "cancel"), false);
  assert.equal(mayControl(call({ state: "RINGING", initiatorPrincipalId: me }), me, "cancel"), true);
  assert.equal(mayControl(call({ initiatorPrincipalId: me }), me, "cancel"), false, "cancel is pre-answer only");
});

test("after a transfer both remaining non-initiators may end the call", () => {
  const transferred = call({
    initiatorPrincipalId: "retired",
    participants: [
      { participantId: "p-r", principalId: "retired", role: "INITIATOR", endedAt: "2026-11-08T10:00:00Z" },
      { participantId: "p-me", principalId: me, role: "RECIPIENT", endedAt: null },
      { participantId: "p-t", principalId: "target", role: "RECIPIENT", endedAt: null },
    ],
  });
  assert.equal(mayControl(transferred, me, "end"), true);
  assert.equal(mayControl(transferred, "target", "end"), true);
  assert.equal(mayControl(transferred, "retired", "end"), false, "the retired transferor no longer controls the call");
  assert.equal(visibleControls(transferred, "target", all).end, true);
});

test("non-participants control nothing; ex-participants only replay once the call winds down", () => {
  assert.equal(mayControl(call(), "stranger", "end"), false);
  const left = call({ participants: [...call().participants.slice(0, 1), { participantId: "p-me", principalId: me, endedAt: "x" }, { participantId: "p3", principalId: "z", endedAt: null }] });
  assert.equal(mayControl(left, me, "end"), false);
  assert.equal(mayControl({ ...left, state: "ENDED" }, me, "end"), true, "idempotent replay after termination");
  assert.equal(mayControl({ ...left, state: "ENDING" }, me, "hold"), true);
});

test("IVR keypad input belongs to the active caller", () => {
  assert.equal(mayControl(call(), me, "ivrInput"), false);
  assert.equal(mayControl(call({ initiatorPrincipalId: me }), me, "ivrInput"), true);
});

test("the UI shows only controls that are both permitted and available, never on a finished call", () => {
  const actions = (names) => (name) => names.includes(name);
  assert.deepEqual(visibleControls(call(), me, actions(["END", "HOLD"])), { cancel: false, end: true, hold: true, resume: false, transfer: false });
  assert.deepEqual(visibleControls(call({ state: "ENDED" }), me, all), { cancel: false, end: false, hold: false, resume: false, transfer: false });
  assert.equal(visibleControls(call({ state: "REQUESTED", initiatorPrincipalId: me }), me, all).end, false);
  assert.equal(visibleControls(call({ state: "REQUESTED", initiatorPrincipalId: me }), me, all).cancel, true);
});

test("own session is the live session of my active participant on this endpoint", () => {
  assert.equal(ownSession(call(), me, "e-me")?.sessionId, "s-me");
  assert.equal(ownSession(call(), me, "e-other"), undefined);
  assert.equal(ownSession(call({ sessions: [{ sessionId: "s-me", participantId: "p-me", endpointId: "e-me", removedAt: "x" }] }), me, "e-me"), undefined);
  assert.equal(ownSession(call(), "caller", "e-me"), undefined);
});

// ---- CAL-015 reconnect state machine ----

const run = (events, start = INITIAL_MEDIA_STATE) => events.reduce((state, event) => mediaReducer(state, event), start);

test("a lost connection on a live call schedules an automatic rejoin with backoff", () => {
  let s = run([{ type: "JOIN" }, { type: "CONNECTED" }]);
  assert.equal(s.status, "connected");
  s = mediaReducer(s, { type: "DISCONNECTED", userInitiated: false, callLive: true });
  assert.equal(s.status, "reconnecting");
  assert.equal(s.retryInMs, 1000);
  s = mediaReducer(s, { type: "REJOIN_STARTED" });
  assert.equal(s.status, "connecting");
  assert.equal(s.attempt, 1);
  s = mediaReducer(s, { type: "REJOIN_FAILED", retryable: true });
  assert.equal(s.status, "reconnecting");
  assert.equal(s.retryInMs, 2000);
  s = run([{ type: "REJOIN_STARTED" }, { type: "CONNECTED" }], s);
  assert.deepEqual(s, { status: "connected", attempt: 0, retryInMs: null, reason: null });
});

test("the attempt budget is bounded, then the failure is surfaced and a manual retry restarts it", () => {
  let s = run([{ type: "JOIN" }, { type: "CONNECTED" }, { type: "DISCONNECTED", userInitiated: false, callLive: true }]);
  for (let i = 0; i < DEFAULT_RECONNECT_POLICY.maxAttempts; i += 1) {
    assert.equal(s.status, "reconnecting");
    s = run([{ type: "REJOIN_STARTED" }, { type: "REJOIN_FAILED", retryable: true }], s);
  }
  assert.equal(s.status, "failed");
  assert.match(mediaStatusLabel(s), /could not be restored/);
  s = mediaReducer(s, { type: "RETRY_NOW" });
  assert.equal(s.status, "reconnecting");
  assert.equal(s.attempt, 0);
  assert.equal(s.retryInMs, 0);
});

test("no rejoin after the user's own leave, a finished call, or a non-retryable refusal", () => {
  const connected = run([{ type: "JOIN" }, { type: "CONNECTED" }]);
  assert.equal(mediaReducer(connected, { type: "DISCONNECTED", userInitiated: true, callLive: true }).status, "left");
  assert.equal(mediaReducer(connected, { type: "DISCONNECTED", userInitiated: false, callLive: false }).status, "left");
  const reconnecting = mediaReducer(connected, { type: "DISCONNECTED", userInitiated: false, callLive: true });
  assert.equal(run([{ type: "REJOIN_STARTED" }, { type: "REJOIN_FAILED", retryable: false }], reconnecting).status, "failed");
  assert.equal(mediaReducer(reconnecting, { type: "CALL_ENDED" }).status, "left");
  assert.equal(mediaReducer(mediaReducer(reconnecting, { type: "LEAVE" }), { type: "CONNECTED" }).status, "left");
});

test("LiveKit's own retry is surfaced before the client takes over", () => {
  let s = run([{ type: "JOIN" }, { type: "CONNECTED" }, { type: "PROVIDER_RECONNECTING" }]);
  assert.equal(s.status, "provider-reconnecting");
  assert.match(mediaStatusLabel(s), /Reconnecting/);
  s = mediaReducer(s, { type: "PROVIDER_RECONNECTED" });
  assert.equal(s.status, "connected");
});

test("backoff is exponential and capped; credential refusals that cannot heal are not retried", () => {
  assert.deepEqual([0, 1, 2, 3, 4, 5].map((n) => backoffDelay(n)), [1000, 2000, 4000, 8000, 15000, 15000]);
  for (const status of [401, 403, 404, 409, 410]) assert.equal(isRetryableCredentialFailure(status), false, String(status));
  for (const status of [undefined, 408, 429, 500, 503]) assert.equal(isRetryableCredentialFailure(status), true, String(status));
});

// ---- CAL-008 / CAL-020 production guard, short-lived credentials ----

test("dev mode is honoured only in a development build without a credential", () => {
  assert.equal(callingRuntimeMode({}), "live");
  assert.equal(callingRuntimeMode({ CALLING_DEV_MODE: "true", NODE_ENV: "development" }), "dev");
  assert.throws(() => callingRuntimeMode({ CALLING_DEV_MODE: "true", NODE_ENV: "production" }), CallingConfigurationError);
  assert.throws(() => callingRuntimeMode({ CALLING_DEV_MODE: "true", CALLING_PLATFORM_CREDENTIAL: "secret" }), CallingConfigurationError);
  assert.equal(callingRuntimeMode({ CALLING_DEV_MODE: "1", NODE_ENV: "production" }), "live");
});

test("production requires an explicit https Calling origin; development may fall back locally", () => {
  assert.equal(callingBaseUrl({ NODE_ENV: "production", CALLING_API_URL: "https://calling.example/" }), "https://calling.example");
  for (const value of [undefined, "", "http://calling.example", "ws://calling.example", "https://user:pw@calling.example", "not a url"]) {
    assert.throws(() => callingBaseUrl({ NODE_ENV: "production", CALLING_API_URL: value }), CallingConfigurationError, String(value));
  }
  assert.equal(callingBaseUrl({ NODE_ENV: "development" }), "http://localhost:8090");
});

test("the signaling socket is wss-only in production and never leaves the Calling origin", () => {
  assert.equal(signalingSocketUrl("/v1/calling/signaling", "https://api.cfmeet.com", true).href, "wss://api.cfmeet.com/v1/calling/signaling");
  assert.throws(() => signalingSocketUrl("/v1/calling/signaling", "http://calling.example", true));
  assert.equal(signalingSocketUrl("/v1/calling/signaling", "http://localhost:8090", false).protocol, "ws:");
  assert.throws(() => signalingSocketUrl("https://evil.example/v1/calling/signaling", "https://api.cfmeet.com", true));
});

test("short-lived credentials are renewed before expiry and an over-long or expired one is not trusted", () => {
  const now = Date.parse("2026-11-08T10:00:00Z");
  assert.equal(renewalDelay("2026-11-08T10:10:00Z", now), 600_000 - 15_000);
  assert.equal(renewalDelay("2026-11-08T10:00:05Z", now), 1_000);
  assert.equal(renewalDelay("2026-11-08T09:59:59Z", now), null);
  assert.equal(renewalDelay("2026-11-08T12:00:00Z", now), null, "longer than the backend's 3600 s cap");
  assert.equal(renewalDelay("garbage", now), null);
});

// ---- CAL-004 incoming-call presentation ----

test("only actionable, unexpired offers ring, one per call, oldest first", () => {
  const now = Date.parse("2026-11-08T10:00:00Z");
  const offer = (id, callId, expires, actionable = true) => ({ invitationId: id, callId, callKind: "VOICE", purpose: "INITIAL", actionable, expiresAt: expires });
  const offers = [
    offer("late", "c2", "2026-11-08T10:00:50Z"),
    offer("early", "c1", "2026-11-08T10:00:20Z"),
    offer("dup", "c1", "2026-11-08T10:00:30Z"),
    offer("expired", "c3", "2026-11-08T09:59:00Z"),
    offer("answered", "c4", "2026-11-08T10:01:00Z", false),
  ];
  assert.deepEqual(ringingOffers(offers, now).map((o) => o.invitationId), ["early", "late"]);
  assert.deepEqual(ringingOffers(offers, now, new Set(["early", "dup"])).map((o) => o.invitationId), ["late"]);
});

test("the caller is named from the call view, then from Meeting's principal names, never as a raw id", () => {
  const participants = [{ principalId: "caller", role: "INITIATOR", displayName: null }, { principalId: me, role: "RECIPIENT", displayName: "Me" }];
  assert.equal(callerLabel(participants, me), "CF Meet user");
  assert.equal(callerLabel(participants, me, { caller: "Ada Lovelace" }), "Ada Lovelace");
  assert.equal(callerLabel([{ ...participants[0], displayName: "Grace" }, participants[1]], me, { caller: "Other" }), "Grace");
  assert.equal(callerLabel(undefined, me), "Incoming call");
  assert.equal(offerHeadline({ callKind: "VIDEO", purpose: "TRANSFER" }), "Transferred video call");
  assert.equal(offerHeadline({ callKind: "VOICE", purpose: "INITIAL" }), "Incoming voice call");
});
