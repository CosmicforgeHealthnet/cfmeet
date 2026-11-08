// ACC-019 / SEC-021: the web account session — access token in memory only, refresh token
// in an httpOnly SameSite=Strict cookie set by this app's own same-origin routes, single-flight
// refresh serialized across tabs, one-time migration of the legacy JS-readable cookies.
//   node --experimental-strip-types --test tests/account-session.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  ACCESS_TOKEN_SKEW_MS,
  REFRESH_LOCK_NAME,
  SessionEndedError,
  SessionUnavailableError,
  createSessionManager,
  jwtExpiry,
} from "../src/lib/auth/session-core.ts";
import {
  HINT_COOKIE,
  REFRESH_COOKIE,
  accountRefreshUrl,
  clearedSessionCookies,
  isSameOriginSessionRequest,
  readCookie,
  refreshCookieMaxAge,
  refreshOutcome,
  sessionCookies,
} from "../src/lib/auth/session-cookies.ts";

const b64 = (o) => Buffer.from(JSON.stringify(o)).toString("base64url");
const jwt = (claims) => `${b64({ alg: "HS256" })}.${b64(claims)}.sig`;
const NOW = 1_800_000_000_000;
const fresh = (n = 1) => jwt({ exp: NOW / 1000 + 900, n });
const expired = () => jwt({ exp: NOW / 1000 - 10 });

const reply = (status, body = {}) => ({ ok: status >= 200 && status < 300, status, json: async () => body });

function harness({ hint = "persistent", responses = [], legacy = null, locks = null } = {}) {
  const calls = [];
  const state = { hint, legacy, cleared: 0 };
  const queue = [...responses];
  const manager = createSessionManager({
    now: () => NOW,
    readHint: () => state.hint,
    locks: () => locks,
    fetchImpl: async (url, init) => {
      calls.push({ url, init });
      const next = queue.shift();
      if (typeof next === "function") return next(url, init);
      if (!next) throw new Error("network down");
      return next;
    },
    legacy: state.legacy === null ? null : {
      read: () => state.legacy,
      clear: () => { state.legacy = null; state.cleared += 1; },
    },
  });
  return { manager, calls, state };
}

test("establish stores the refresh token through the same-origin route and keeps the access token in memory", async () => {
  const access = fresh();
  const refresh = fresh(2);
  const { manager, calls } = harness({ hint: null, responses: [reply(200, { success: true })] });
  const events = [];
  manager.subscribe((signedIn, change) => events.push([signedIn, change]));
  await manager.establish(access, refresh, true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, "/api/session");
  assert.equal(calls[0].init.method, "POST");
  assert.equal(calls[0].init.credentials, "same-origin");
  assert.equal(calls[0].init.headers["X-CF-Session"], "1", "custom header forces a CORS preflight cross-site");
  assert.deepEqual(JSON.parse(calls[0].init.body), { refresh_token: refresh, remember: true });
  assert.equal(manager.peekAccessToken(), access);
  assert.deepEqual(events, [[true, "signed_in"]]);
});

test("establish refuses a non-JWT refresh token and a failing session route", async () => {
  const { manager } = harness({ responses: [reply(403)] });
  await assert.rejects(manager.establish(fresh(), "not-a-token", false), SessionUnavailableError);
  await assert.rejects(manager.establish(fresh(), fresh(2), false), SessionUnavailableError);
  assert.equal(manager.peekAccessToken(), null);
});

test("signed-out visitors (no hint cookie) never hit the network", async () => {
  const { manager, calls } = harness({ hint: null });
  assert.equal(manager.hasSession(), false);
  assert.equal(await manager.getAccessToken(), null);
  assert.equal(calls.length, 0);
});

test("a reload mints a new access token through the refresh route; only the access token comes back", async () => {
  const access = fresh(3);
  const { manager, calls } = harness({ responses: [reply(200, { access_token: access })] });
  assert.equal(manager.hasSession(), true);
  assert.equal(await manager.getAccessToken(), access);
  assert.equal(calls[0].url, "/api/session/refresh");
  assert.equal(calls[0].init.body, "{}", "the refresh token is never in the request body: it is the httpOnly cookie");
  assert.equal(await manager.getAccessToken(), access, "a usable token is reused");
  assert.equal(calls.length, 1);
});

test("refresh is single-flight in a tab and serialized across tabs with the Web Lock", async () => {
  const lockNames = [];
  const locks = { request: async (name, cb) => { lockNames.push(name); return cb(); } };
  let release;
  const gate = new Promise((r) => { release = r; });
  const access = fresh(4);
  const { manager, calls } = harness({ locks, responses: [async () => { await gate; return reply(200, { access_token: access }); }] });
  const all = Promise.all([manager.refreshAccessToken(), manager.refreshAccessToken(), manager.getAccessToken()]);
  release();
  assert.deepEqual(await all, [access, access, access]);
  assert.equal(calls.length, 1, "refresh tokens are single-use: one call only");
  assert.deepEqual(lockNames, [REFRESH_LOCK_NAME]);
});

test("a token close to expiry is refreshed before use", async () => {
  const soon = jwt({ exp: (NOW + ACCESS_TOKEN_SKEW_MS - 1000) / 1000 });
  const next = fresh(5);
  const { manager, calls } = harness({ responses: [reply(200), reply(200, { access_token: next })] });
  await manager.establish(soon, fresh(6), false);
  assert.equal(manager.peekAccessToken(), null, "inside the skew window: not handed out");
  assert.equal(await manager.getAccessToken(), next);
  assert.equal(calls[1].url, "/api/session/refresh");
  const gone = harness({ responses: [reply(200)] });
  await gone.manager.establish(expired(), fresh(6), false);
  assert.equal(gone.manager.peekAccessToken(), null);
});

test("a rejected refresh ends the session (401) and notifies; an outage does not", async () => {
  const { manager } = harness({ responses: [reply(503), reply(401)] });
  const events = [];
  manager.subscribe((signedIn, change) => events.push([signedIn, change]));
  await assert.rejects(manager.refreshAccessToken(), SessionUnavailableError);
  assert.deepEqual(events, [], "503: the session may still be valid");
  await assert.rejects(manager.refreshAccessToken(), SessionEndedError);
  assert.deepEqual(events, [[false, "ended"]]);
  assert.equal(manager.peekAccessToken(), null);
});

test("getAccessToken answers null (signed out) when the refresh cookie was rejected", async () => {
  const { manager } = harness({ responses: [reply(401)] });
  assert.equal(await manager.getAccessToken(), null);
});

test("a malformed refresh answer is an outage, never a token", async () => {
  const { manager } = harness({ responses: [reply(200, { access_token: "nope" })] });
  await assert.rejects(manager.refreshAccessToken(), SessionUnavailableError);
});

test("clear removes the cookies through DELETE /api/session, the legacy cookies, and notifies", async () => {
  const { manager, calls, state } = harness({ responses: [reply(200), reply(204)], legacy: { accessToken: null, refreshToken: null, remember: false } });
  const events = [];
  manager.subscribe((signedIn, change) => events.push(change));
  await manager.establish(fresh(), fresh(2), false);
  await manager.clear();
  assert.equal(calls.at(-1).init.method, "DELETE");
  assert.equal(calls.at(-1).url, "/api/session");
  assert.equal(manager.peekAccessToken(), null);
  assert.ok(state.cleared >= 1);
  assert.deepEqual(events, ["signed_in", "signed_out"]);
});

test("forget (another tab signed out) drops the token without a network call", async () => {
  const { manager, calls } = harness({ responses: [reply(200)] });
  await manager.establish(fresh(), fresh(2), false);
  const events = [];
  manager.subscribe((s, change) => events.push(change));
  manager.forget();
  assert.equal(manager.peekAccessToken(), null);
  assert.equal(calls.length, 1);
  assert.deepEqual(events, ["signed_out_elsewhere"]);
});

test("legacy JS-readable cookies are migrated once into the httpOnly model, then erased", async () => {
  const legacyRefresh = fresh(7);
  const legacyAccess = fresh(8);
  const { manager, calls, state } = harness({
    hint: null,
    legacy: { accessToken: legacyAccess, refreshToken: legacyRefresh, remember: true },
    responses: [reply(200)],
  });
  assert.equal(manager.hasSession(), true, "a legacy refresh cookie still counts as a session");
  assert.equal(await manager.getAccessToken(), legacyAccess, "the legacy access token is kept in memory only");
  assert.equal(calls[0].url, "/api/session");
  assert.deepEqual(JSON.parse(calls[0].init.body), { refresh_token: legacyRefresh, remember: true });
  assert.equal(state.legacy, null, "legacy cookies removed after the move");
  assert.equal(manager.peekAccessToken(), legacyAccess);
});

test("garbage legacy cookies are erased without being sent anywhere", async () => {
  const { manager, calls, state } = harness({ hint: null, legacy: { accessToken: "x", refreshToken: "y", remember: false } });
  assert.equal(await manager.getAccessToken(), null);
  assert.equal(calls.length, 0);
  assert.equal(state.legacy, null);
});

test("jwtExpiry reads exp without trusting anything else", () => {
  assert.equal(jwtExpiry(jwt({ exp: 42 })), 42);
  assert.equal(jwtExpiry(jwt({})), null);
  assert.equal(jwtExpiry("a.b"), null);
});

// ---- server half: cookies and request guard ------------------------------------------

test("the refresh cookie is HttpOnly, SameSite=Strict, scoped to /api/session and Secure on HTTPS", () => {
  const token = jwt({ exp: NOW / 1000 + 3600 });
  const [rt, hint] = sessionCookies({ refreshToken: token, persistent: true, secure: true, nowMs: NOW });
  assert.ok(rt.startsWith(`${REFRESH_COOKIE}=`));
  for (const part of ["HttpOnly", "SameSite=Strict", "Path=/api/session", "Secure", "Max-Age=3600"]) assert.ok(rt.includes(part), `${part} in ${rt}`);
  assert.ok(hint.startsWith(`${HINT_COOKIE}=persistent`));
  assert.ok(!hint.includes("HttpOnly"), "the hint is readable by script and carries no secret");
  assert.ok(!hint.includes(token), "the hint never carries the token");
  const [sessionOnly] = sessionCookies({ refreshToken: token, persistent: false, secure: false, nowMs: NOW });
  assert.ok(!sessionOnly.includes("Max-Age") && !sessionOnly.includes("Secure"), "browser-session cookie without remember me");
});

test("cookie lifetime never exceeds the token's expiry or 30 days", () => {
  assert.equal(refreshCookieMaxAge(jwt({ exp: NOW / 1000 + 100 }), NOW), 100);
  assert.equal(refreshCookieMaxAge(jwt({ exp: NOW / 1000 + 90 * 86400 }), NOW), 30 * 86400);
  assert.equal(refreshCookieMaxAge(jwt({ exp: NOW / 1000 - 5 }), NOW), 0);
});

test("clearing sets both cookies to Max-Age=0", () => {
  for (const c of clearedSessionCookies(true)) assert.ok(c.includes("Max-Age=0"), c);
});

const headers = (h) => ({ get: (k) => h[k.toLowerCase()] ?? null });

test("session routes accept only same-origin script requests (CSRF / login CSRF)", () => {
  const url = "https://meet.example/api/session";
  assert.equal(isSameOriginSessionRequest(headers({ "x-cf-session": "1", origin: "https://meet.example", "sec-fetch-site": "same-origin" }), url), true);
  assert.equal(isSameOriginSessionRequest(headers({ origin: "https://meet.example", "sec-fetch-site": "same-origin" }), url), false, "custom header required");
  assert.equal(isSameOriginSessionRequest(headers({ "x-cf-session": "1", origin: "https://evil.example", "sec-fetch-site": "cross-site" }), url), false);
  assert.equal(isSameOriginSessionRequest(headers({ "x-cf-session": "1", origin: "https://evil.example" }), url), false);
  assert.equal(isSameOriginSessionRequest(headers({ "x-cf-session": "1" }), url), false, "no Origin and no Sec-Fetch-Site: refused");
  assert.equal(
    isSameOriginSessionRequest(headers({ "x-cf-session": "1", origin: "https://meet.example", "x-forwarded-proto": "https", "x-forwarded-host": "meet.example" }), "http://internal:3000/api/session"),
    true,
    "behind the TLS-terminating proxy",
  );
});

test("refresh outcomes: 400/401 end the session, 429 rate limited, others unavailable", () => {
  assert.equal(refreshOutcome(200), "rotated");
  assert.equal(refreshOutcome(400), "ended");
  assert.equal(refreshOutcome(401), "ended");
  assert.equal(refreshOutcome(429), "rate_limited");
  assert.equal(refreshOutcome(502), "unavailable");
});

test("readCookie and the refresh URL", () => {
  assert.equal(readCookie("a=1; cfm_session=persistent; b=2", HINT_COOKIE), "persistent");
  assert.equal(readCookie("", HINT_COOKIE), null);
  assert.equal(accountRefreshUrl("https://api.example/api/v1/"), "https://api.example/api/v1/auth/refresh");
});

// ---- static guard: no bearer / refresh token in script-readable storage ----------------

const SRC = fileURLToPath(new URL("../src/", import.meta.url));
function* files(dir) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) yield* files(path);
    else if (/\.(ts|tsx)$/.test(name) && !path.includes(`${join("types", "generated")}`)) yield path;
  }
}

test("no source file writes an account token to a JS-readable cookie, localStorage or sessionStorage", () => {
  const offenders = [];
  const write = /(Cookies\.set|document\.cookie\s*=|localStorage\.setItem|sessionStorage\.setItem)\s*\(?([^;\n]*)/g;
  for (const file of files(SRC)) {
    const text = readFileSync(file, "utf8");
    for (const m of text.matchAll(write)) {
      if (/(access|refresh)_?token|accessToken|refreshToken|bearer/i.test(m[2])) offenders.push(`${file}: ${m[0].trim()}`);
    }
  }
  assert.deepEqual(offenders, []);
  assert.equal(
    [...files(SRC)].some((f) => readFileSync(f, "utf8").includes("cookie.store")),
    false,
    "the pre-Section 17 cookie token store is gone",
  );
});
