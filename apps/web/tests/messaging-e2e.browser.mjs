// The REAL CF Meet web Messaging client in Chromium, against real services: the production
// Next build of apps/web, the Meeting Service (identity bridge + client relay), Platform
// Access and Messaging processes, PostgreSQL, Redis and S3-compatible storage.
//
// Driven by services/meeting-service/tests/unified_messaging_e2e.rs
// (`web_messaging_client_in_chromium_across_real_services`), which starts the services,
// seeds the accounts and passes them here:
//
//   E2E_WEB_PORT    the port `next start` listens on (the build baked the service URLs)
//   E2E_USERS       {"alice":{"id","email","code","name"},"bob":{...},"mallory":{...}}
//   E2E_API_URL     the account API (`/api/v1`) the build targets, for the one-time-code sign-in
//   E2E_SECRET      a unique phrase: it must never appear outside the devices
//   E2E_EVIDENCE    where to write the step log (JSON)
//
// Steps: device registration; an end-to-end encrypted conversation created in the UI while
// the recipient is offline, read when the recipient opens it with nobody else online;
// encrypted send / receive; XSS payloads rendered as text; reload (encrypted local
// state); offline delivery; network loss and reconnect; an encrypted attachment;
// new chats are private-only (the new-chat dialog offers no MANAGED choice); a second browser
// trusted by its own sign-in (no verification step, ADR 0067) that joins the chat BY ITSELF
// while every other device is offline (MLS external commit), older messages labelled "Sent
// before this browser joined", its history recovered device-to-device with comparison
// codes; device removal
// erasing local state; CSP / Trusted Types / worker integrity; expired session.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { chromium } from "playwright";

const PORT = Number(process.env.E2E_WEB_PORT ?? 3100);
const BASE = `http://127.0.0.1:${PORT}`;
const USERS = JSON.parse(process.env.E2E_USERS ?? "{}");
const API = (process.env.E2E_API_URL ?? "http://127.0.0.1:18080/api/v1").replace(/\/+$/, "");
const SECRET = process.env.E2E_SECRET ?? `secret-${Date.now()}`;
const EVIDENCE = process.env.E2E_EVIDENCE;
const T = 60_000;
const log = [];
const step = (name, detail = {}) => {
  log.push({ name, ...detail, at: new Date().toISOString() });
  console.log(`STEP ${name} ${JSON.stringify(detail)}`);
};
let server;
let browser;

before(async () => {
  server = spawn("npx", ["next", "start", "-p", String(PORT), "-H", "127.0.0.1"], {
    env: { ...process.env, NODE_ENV: "production", NEXT_TELEMETRY_DISABLED: "1" },
    stdio: "ignore",
    detached: true,
  });
  for (let i = 0; i < 240; i++) {
    try {
      const r = await fetch(`${BASE}/login`, { signal: AbortSignal.timeout(5000) });
      if (r.status < 500) break;
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  browser = await chromium.launch();
});

after(async () => {
  if (EVIDENCE) writeFileSync(EVIDENCE, JSON.stringify(log, null, 2));
  await browser?.close();
  if (server) process.kill(-server.pid, "SIGTERM");
});

/**
 * Signs a browser context in as the passwordless sign-in does (ACC-019, PR #97): a one-time
 * code verified with the account authority (the journey users are fixed-code test accounts,
 * AUTH_TEST_ACCOUNTS), then the app's own `POST /api/session` stores the refresh
 * token in its httpOnly cookie. The access token stays out of the browser: the app mints
 * its in-memory token through `/api/session/refresh` on load. One login per context, so
 * no two contexts share a single-use refresh token.
 */
async function signIn(ctx, who, origin = BASE) {
  const login = await fetch(`${API}/auth/otp/verify`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify({ email: USERS[who].email, code: USERS[who].code }),
  });
  const body = await login.json().catch(() => null);
  assert.equal(login.status, 200, `one-time-code sign-in for ${who}: ${JSON.stringify(body)}`);
  assert.ok(body?.refresh_token, `the login for ${who} returned a refresh token`);
  const session = await ctx.request.post(`${origin}/api/session`, {
    headers: { "X-CF-Session": "1", Origin: origin },
    data: { refresh_token: body.refresh_token, remember: false },
  });
  assert.equal(session.status(), 200, `session cookie for ${who}: ${await session.text()}`);
}

async function open(who, { route, mobile = false, authenticator = false } = {}) {
  const origin = authenticator ? BASE.replace("127.0.0.1", "localhost") : BASE;
  const ctx = await browser.newContext({ acceptDownloads: true, ...(mobile ? {
    viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true,
    userAgent: "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36",
  } : {}) });
  await signIn(ctx, who, origin);
  // What the login flow stores for a signed-in account (the dashboard requires it).
  await ctx.addInitScript((u) => {
    if (!localStorage.getItem("cosmic_user_info")) localStorage.setItem("cosmic_user_info", JSON.stringify(u));
  }, { id: USERS[who].id, email: USERS[who].email, username: USERS[who].name });
  await ctx.addInitScript(() => {
    window.__violations = [];
    document.addEventListener("securitypolicyviolation", (e) => window.__violations.push(`${e.effectiveDirective} ${e.blockedURI} ${e.sample}`));
  });
  if (route) await route(ctx);
  const page = await ctx.newPage();
  if (authenticator) {
    const cdp = await ctx.newCDPSession(page);
    await cdp.send("WebAuthn.enable", { enableUI: false });
    await cdp.send("WebAuthn.addVirtualAuthenticator", { options: {
      protocol: "ctap2", ctap2Version: "ctap2_1", transport: "internal",
      hasResidentKey: true, hasUserVerification: true, hasPrf: true,
      automaticPresenceSimulation: true, isUserVerified: true,
    } });
  }
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  // Every 429 on any page of this browser (reopened pages included) is named in the log, so a
  // rate-limited run says which request hit the limit. Paths only: no query, no bodies.
  ctx.on("response", (r) => {
    if (r.status() !== 429) return;
    const u = new URL(r.url());
    console.log(`[${who} 429] ${r.request().method()} ${u.origin}${u.pathname} retry-after=${r.headers()["retry-after"] ?? "-"}`);
  });
  if (process.env.E2E_DEBUG) {
    page.on("console", (m) => console.log(`[${who} console.${m.type()}] ${m.text()}`));
    page.on("requestfailed", (r) => console.log(`[${who} failed] ${r.method()} ${r.url()} ${r.failure()?.errorText}`));
    page.on("response", (r) => r.status() >= 400 && console.log(`[${who} ${r.status()}] ${r.request().method()} ${r.url()}`));
  }
  await page.goto(`${origin}/dashboard/messages`, { waitUntil: "load" });
  return { ctx, page, errors, origin };
}

// The device / connection status lives in the "Security & devices" sheet, which stays
// mounted (hidden) while closed: wait for the state, not for the sheet to be open.
async function phase(page, p) {
  try {
    await page.waitForSelector(`[data-testid=device-state][data-phase=${p}]`, { timeout: T, state: "attached" });
  } catch (e) {
    const text = await page.locator("[data-testid=messages-page]").innerText().catch(() => "(no page)");
    throw new Error(`waiting for phase ${p}; the page shows:\n${text}`, { cause: e });
  }
}
const violations = (page) => page.evaluate(() => window.__violations);
const connection = (page, sel) => page.locator(`[data-testid=connection-state]${sel}`).waitFor({ timeout: T, state: "attached" });

/** New chat: tapping a person starts the chat with them (WhatsApp-style). */
async function pick(page, name) {
  const row = page.locator(`[data-testid=colleague][data-name="${name}"]`);
  await row.waitFor({ timeout: T, state: "attached" });
  await row.click();
}

/** Private messaging sets itself up after sign-in: no activation control exists. */
async function register(page) {
  await phase(page, "ready");
  assert.equal(await page.locator("[data-testid=register-device]").count(), 0, "no manual activation button");
}

async function openConversation(page, profile, nth = 0) {
  const item = page.locator(`[data-testid=conversation-item][data-profile=${profile}]`).nth(nth);
  await item.waitFor({ timeout: T });
  await item.click();
  await page.waitForSelector(`[data-testid=conversation][data-profile=${profile}]`, { timeout: T });
}

async function send(page, text) {
  await page.fill("[data-testid=composer]", text);
  await page.click("[data-testid=send]");
}

const shown = (page, text) => page.locator("[data-testid=message-text]", { hasText: text }).first().waitFor({ timeout: T });
async function loadAllPrivateHistory(page) {
  for (let pages = 0; pages < 20; pages++) {
    const button = page.locator('[data-testid=load-older]');
    if (!await button.count()) return;
    const previous = await page.locator('[data-testid=message]').count();
    await button.click();
    await page.waitForFunction(count => !document.querySelector('[data-testid=load-older]') || document.querySelectorAll('[data-testid=message]').length > count, previous, { timeout: T });
  }
  assert.fail('private history cursor did not reach the start within the fixture page budget');
}


/** Reopens a closed browser of `who` (same profile: same device) on the messages page. */
async function reopen(who, s) {
  s.page = await s.ctx.newPage();
  pages[who] = s.page;
  await s.page.goto(`${BASE}/dashboard/messages`, { waitUntil: "load" });
  await phase(s.page, "ready");
}

/** The chat list never shows a blank placeholder bar for a private chat that has messages. */
async function previewIsText(page) {
  const item = page.locator("[data-testid=conversation-item][data-profile=PRIVATE_E2EE]").first();
  await item.waitFor({ timeout: T });
  await page.waitForFunction(
    () => {
      const el = document.querySelector("[data-testid=conversation-item][data-profile=PRIVATE_E2EE]");
      return el && !el.querySelector("[aria-label='Loading message']");
    },
    null,
    { timeout: T },
  );
}

/** Every IndexedDB value of the Messaging store: is the needle anywhere, are keys extractable? */
function inspectStorage(page, needle) {
  return page.evaluate(async (needle) => {
    const db = await new Promise((res, rej) => {
      const r = indexedDB.open("cfmeet-messaging");
      r.onsuccess = () => res(r.result);
      r.onerror = () => rej(r.error);
    });
    const enc = new TextEncoder().encode(needle);
    const contains = (u8) => {
      outer: for (let i = 0; i + enc.length <= u8.length; i++) {
        for (let j = 0; j < enc.length; j++) if (u8[i + j] !== enc[j]) continue outer;
        return true;
      }
      return false;
    };
    let found = 0;
    let values = 0;
    const keys = [];
    for (const name of db.objectStoreNames) {
      const all = await new Promise((r) => {
        const q = db.transaction(name).objectStore(name).getAll();
        q.onsuccess = () => r(q.result);
      });
      for (const v of all) {
        values++;
        if (v instanceof CryptoKey) {
          keys.push({ extractable: v.extractable, algorithm: v.algorithm.name });
          continue;
        }
        if (JSON.stringify(v).includes(needle)) found++;
        if (v?.ct instanceof Uint8Array && contains(v.ct)) found++;
      }
    }
    db.close();
    return { found, values, keys, local: localStorage.length, session: sessionStorage.length };
  }, needle);
}

const pages = {};
test("the CF Meet web app runs real end-to-end encrypted messaging", { timeout: 15 * 60_000 }, async () => {
  try {
    await journey();
  } catch (e) {
    for (const [who, p] of Object.entries(pages)) {
      const text = await p.locator("[data-testid=messages-page]").innerText().catch(() => "(closed)");
      console.log(`--- ${who} page at failure ---\n${text.length > 24000 ? text.slice(0, 12000) + "\n[large fixture text omitted]\n" + text.slice(-12000) : text}`);
    }
    throw e;
  }
});

async function journey() {
  assert.ok(USERS.alice && USERS.bob, "E2E_USERS must name alice and bob");

  // ---- 1. Automatic device setup in the signed-in app (keys generated in the worker; the
  //         first browser of an account is its trusted device, no activation step).
  const alice = await open("alice");
  const bob = await open("bob");
  pages.alice = alice.page;
  pages.bob = bob.page;
  await register(alice.page);
  await register(bob.page);
  // Ten independent profiles and account sign-ins; no storage/token sharing.
  const alicePhone = await open("alice", { mobile: true });
  const aliceOther = await open("alice");
  const bobPhone = await open("bob", { mobile: true });
  const otherDevices = [["alice", alicePhone], ["alice", aliceOther], ["bob", bobPhone]];
  Object.assign(pages, { alicePhone: alicePhone.page, aliceOther: aliceOther.page, bobPhone: bobPhone.page });
  for (const [user, count] of [["alice", 2], ["bob", 3]]) {
    for (let i = 0; i < count; i++) {
      const device = await open(user, { mobile: i % 2 === 0 });
      otherDevices.push([user, device]);
      pages[`${user}Extra${i}`] = device.page;
    }
  }
  for (const [, device] of otherDevices) await register(device.page);

  const pageCsp = (await alice.page.request.get(`${BASE}/dashboard/messages`)).headers()["content-security-policy"];
  assert.ok(pageCsp && !pageCsp.includes("wasm-unsafe-eval") && !pageCsp.includes("'unsafe-eval'"), pageCsp);
  step("device registration", { devices: 10, accountADevices: 5, accountBDevices: 5, pageCspWithoutWasmEval: true });

  // ---- 2. Alice starts an end-to-end encrypted conversation with Bob in the UI while Bob is
  //         OFFLINE: his device published its keys in advance, so Alice's device adds it
  //         on its own (WhatsApp), and Bob reads her message later with nobody else online.
  await bob.page.close();
  await alice.page.click("[data-testid=new-conversation]");
  await pick(alice.page, USERS.bob.name);
  await alice.page.waitForSelector("[data-testid=conversation][data-profile=PRIVATE_E2EE]", { timeout: T });
  // Presence/typing replaces the header badge. Chat info always exposes the security
  // profile, so verify it through the same control a user opens.
  await alice.page.click("[data-testid=conversation-members-toggle]");
  assert.equal(await alice.page.locator("[data-testid=members] [data-testid=profile-badge]").getAttribute("data-profile"), "PRIVATE_E2EE");
  await alice.page.click("[data-testid=conversation-members-toggle]");
  step("encrypted conversation created", {});

  // ---- 3. Encrypted send and receive, both directions. Alice goes offline right after
  //         sending; Bob's device opens the chat with no other member online.
  const hello = `${SECRET} hello bob`;
  await send(alice.page, hello);
  await alice.page.locator("[data-testid=message][data-mine=true][data-state=sent]").first().waitFor({ timeout: T });
  await alice.page.close();
  await reopen("bob", bob);
  await previewIsText(bob.page);
  await openConversation(bob.page, "PRIVATE_E2EE");
  await shown(bob.page, hello);
  assert.equal(await bob.page.locator("[data-testid=e2ee-unavailable]").count(), 0, "readable without anyone else online");
  for (const [, device] of otherDevices) {
    await openConversation(device.page, "PRIVATE_E2EE");
    await shown(device.page, hello);
  }
  step("five Account A devices and five Account B devices receive desktop message", { accountADevices: 5, accountBDevices: 5, mobileBrowserContexts: 5 });
  step("recipient offline at creation reads without the original sending device", {});
  const reply = `${SECRET} hi alice`;
  await send(bobPhone.page, reply);
  await bobPhone.page.locator("[data-testid=message][data-mine=true][data-state=sent]", { hasText: reply }).waitFor({ timeout: T });
  await shown(bob.page, reply);
  for (const [, device] of otherDevices) await shown(device.page, reply);
  await reopen("alice", alice);
  await openConversation(alice.page, "PRIVATE_E2EE");
  await shown(alice.page, reply);
  step("encrypted send / receive", { messages: 2 });
  const simultaneous = [`${SECRET} concurrent A`, `${SECRET} concurrent B`];
  await Promise.all([send(aliceOther.page, simultaneous[0]), send(bobPhone.page, simultaneous[1])]);
  for (const page of [alice.page, bob.page, ...otherDevices.map(([, d]) => d.page)]) {
    for (const text of simultaneous) await shown(page, text);
  }
  step("ten authorized devices receive simultaneous encrypted sends", { devices: 10 });
  const aliceTab = await alice.ctx.newPage();
  pages.aliceTab = aliceTab;
  await aliceTab.goto(`${BASE}/dashboard/messages`, { waitUntil: "load" });
  await register(aliceTab);
  await openConversation(aliceTab, "PRIVATE_E2EE");
  await shown(aliceTab, reply);
  const tabMessage = `${SECRET} shared browser tab send`;
  await send(aliceTab, tabMessage);
  for (const page of [alice.page, bob.page, ...otherDevices.map(([, d]) => d.page)]) await shown(page, tabMessage);
  await aliceTab.close();
  await send(alice.page, `${SECRET} original tab remains active`);
  await shown(bob.page, `${SECRET} original tab remains active`);
  step("two real application tabs coordinate one cryptographic identity", {});

  // Metadata uses the real server endpoints and encrypted edit/reply payloads.
  const metadataOriginal = `${SECRET} metadata original`;
  const metadataEdited = `${SECRET} metadata revised`;
  const metadataReply = `${SECRET} metadata reply`;
  await send(alice.page, metadataOriginal);
  let metadataRow = alice.page.locator("[data-testid=message][data-message-id]", { hasText: metadataOriginal }).first();
  await metadataRow.locator("[data-testid=message-actions]").waitFor({ timeout: T });
  const metadataMessageId = await metadataRow.getAttribute("data-message-id");
  assert.ok(metadataMessageId, "the metadata target is an accepted server message");
  metadataRow = alice.page.locator(`[data-testid=message][data-message-id="${metadataMessageId}"]`);
  const allDevicePages = [alice.page, bob.page, ...otherDevices.map(([, device]) => device.page)];
  for (const page of allDevicePages) await shown(page, metadataOriginal);
  const phoneOriginal = bobPhone.page.locator(`[data-testid=message][data-message-id="${metadataMessageId}"]`);
  await phoneOriginal.locator("[data-testid=message-actions]").click();
  await bobPhone.page.locator("[data-testid=reply]").click();
  await send(bobPhone.page, metadataReply);
  for (const page of allDevicePages) {
    const replyRow = page.locator("[data-testid=message]", { hasText: metadataReply }).first();
    await replyRow.locator("[data-testid=message-reply]").waitFor({ timeout: T });
  }
  const bobOriginal = bob.page.locator(`[data-testid=message][data-message-id="${metadataMessageId}"]`);
  await bobOriginal.getByRole("button", { name: "React", exact: true }).click();
  await bob.page.getByRole("button", { name: "React \u{1F44D}", exact: true }).click();
  for (const page of allDevicePages) await page.locator(`[data-testid=message][data-message-id="${metadataMessageId}"] [data-testid=reaction]`).waitFor({ timeout: T });
  await metadataRow.locator("[data-testid=message-actions]").click();
  await alice.page.locator("[data-testid=edit]").click();
  await metadataRow.locator("[data-testid=edit-input]").fill(metadataEdited);
  await metadataRow.locator("[data-testid=edit-save]").click();
  for (const page of allDevicePages) await shown(page, metadataEdited);
  await bob.page.bringToFront();
  await metadataRow.getByLabel("Read", { exact: true }).waitFor({ timeout: T });
  await metadataRow.locator("[data-testid=message-actions]").click();
  await alice.page.locator("[data-testid=delete]").click();
  for (const page of allDevicePages) await page.locator(`[data-testid=message][data-message-id="${metadataMessageId}"][data-state=removed]`).waitFor({ timeout: T });
  step("replies reactions encrypted edits read receipts and deletion synchronize across ten devices", { devices: 10 });




  // Fault injection at the untrusted delivery boundary. Services and ciphertext in
  // the database remain real and unchanged; only one browser response is corrupted.
  let damagedOnce = false;
  let retriedClientMessageId = null;
  const captureRetryPublication = request => {
    if (request.method() === "POST" && /\/conversations\/[^/]+\/messages$/.test(request.url())) retriedClientMessageId = request.postDataJSON()?.clientMessageId ?? null;
  };
  alice.page.on("request", captureRetryPublication);
  let orderedPages = 0;
  const deliveryRoute = /\/api\/v2\/messaging\/relay\/v1\/conversations\/[^/]+\/messages\?/;
  const damage = async (route) => {
    if (route.request().method() !== "GET" || !new URL(route.request().url()).searchParams.has("afterSequence")) return route.continue();
    const response = await route.fetch();
    if (response.status() !== 200) return route.fulfill({ response });
    const body = await response.json();
    if (body.messages?.length) {
      orderedPages++;
      for (let i = 1; i < body.messages.length; i++) {
        assert.equal(body.messages[i].sequence, body.messages[i - 1].sequence + 1, "authoritative pages must be contiguous, including tombstones");
      }
    }
    const application = body.messages?.find((m) => m.kind === "APPLICATION" && m.clientMessageId === retriedClientMessageId && m.e2ee?.ciphertext);
    if (!damagedOnce && application) {
      damagedOnce = true;
      application.e2ee.ciphertext = "AQID";
      return route.fulfill({ response, json: body });
    }
    return route.fulfill({ response });
  };
  await bob.page.route(deliveryRoute, damage);
  const retried = `${SECRET} recovered after a failed delivery`;
  await send(alice.page, retried);
  await shown(bob.page, retried);
  alice.page.off("request", captureRetryPublication);
  assert.ok(retriedClientMessageId, "the encrypted publication to corrupt was identified");
  assert.equal(damagedOnce, true, "the integration test must exercise failed decryption");
  assert.ok(orderedPages >= 2, "the failed page is refetched from the authoritative endpoint");
  assert.equal(await bob.page.locator("[data-testid=message-text]").filter({ hasText: retried }).count(), 1, "recovery does not duplicate the message");
  await bob.page.unroute(deliveryRoute, damage);
  step("failed encrypted delivery recovered by authoritative retry", {});


  // ---- 4. Hostile content is shown as text and never executes.
  const payloads = ['<img src=x onerror="window.__xss=1">', "<script>window.__xss=2</script>", "javascript:window.__xss=3", '"><svg onload=window.__xss=4>'];
  for (const p of payloads) await send(alice.page, p);
  for (const p of payloads) {
    const el = bob.page.locator("[data-testid=message-text]").filter({ hasText: p.slice(0, 12) }).last();
    await el.waitFor({ timeout: T });
    assert.equal(await el.textContent(), p);
  }
  assert.equal(await bob.page.evaluate(() => window.__xss ?? null), null);
  // Message text is rendered as text only (no element inside it); the list holds no image,
  // script or foreign SVG (the app's own status icons are Lucide SVGs).
  assert.equal(await bob.page.locator("[data-testid=message-text] *").count(), 0, "no element inside message text");
  assert.equal(await bob.page.locator("[data-testid=messages] img, [data-testid=messages] script, [data-testid=messages] svg:not(.lucide)").count(), 0);
  step("xss payloads inert", { payloads: payloads.length });

  // ---- 5. Reload: the device and its history come back from encrypted storage.
  await bob.page.reload({ waitUntil: "load" });
  await phase(bob.page, "ready");
  await openConversation(bob.page, "PRIVATE_E2EE");
  await shown(bob.page, hello);
  const stored = await inspectStorage(bob.page, SECRET);
  assert.equal(stored.found, 0, "no plaintext in IndexedDB");
  assert.ok(stored.values > 3, `the device stored state: ${JSON.stringify(stored)}`);
  assert.ok(stored.keys.length >= 1 && stored.keys.every((k) => k.extractable === false && k.algorithm === "AES-GCM"), JSON.stringify(stored.keys));
  step("reload from encrypted storage", { values: stored.values, plaintextHits: stored.found, wrappingKeyExtractable: false });

  // ---- 6. Offline delivery: Bob is away while Alice writes; he catches up in order.
  await bob.page.close();
  const away = [1, 2, 3].map((i) => `${SECRET} while away ${i}`);
  for (const m of away) await send(alice.page, m);
  await alice.page.locator("[data-testid=message][data-state=sent]", { hasText: away[2] }).waitFor({ timeout: T });
  bob.page = await bob.ctx.newPage();
  pages.bob = bob.page;
  await bob.page.goto(`${BASE}/dashboard/messages`, { waitUntil: "load" });
  await phase(bob.page, "ready");
  await openConversation(bob.page, "PRIVATE_E2EE");
  for (const m of away) await shown(bob.page, m);
  const order = await bob.page.locator("[data-testid=message-text]").allTextContents();
  const idx = away.map((m) => order.indexOf(m));
  assert.ok(idx.every((v, i) => v >= 0 && (i === 0 || v > idx[i - 1])), `order ${idx}`);
  step("offline delivery in order", { messages: away.length });

  // ---- 7. Network loss and reconnect (cursor resume + REST catch-up).
  await connection(alice.page, "[data-connection=connected]");
  await alice.ctx.setOffline(true);
  await connection(alice.page, ":not([data-connection=connected])");
  const during = `${SECRET} sent during the outage`;
  await send(bob.page, during);
  await alice.ctx.setOffline(false);
  await connection(alice.page, "[data-connection=connected]");
  await shown(alice.page, during);
  step("reconnect", {});

  // ---- 8. Encrypted attachment: encrypted before upload, decrypted on the device.
  const fileBody = Buffer.from(`${SECRET} attachment body ${"x".repeat(5000)}`);
  await alice.page.setInputFiles("[data-testid=file-input]", { name: "plan.txt", mimeType: "text/plain", buffer: fileBody });
  await alice.page.click("[data-testid=attachment-send]");
  const att = bob.page.locator("[data-testid=attachment]", { hasText: "plan.txt" });
  await att.waitFor({ timeout: T });
  const [download] = await Promise.all([bob.page.waitForEvent("download", { timeout: T }), att.click()]);
  assert.equal(download.suggestedFilename(), "plan.txt");
  assert.ok(readFileSync(await download.path()).equals(fileBody), "decrypted bytes equal the original");
  step("encrypted attachment", { bytes: fileBody.length });

  // ---- 9. New chats are private-only: the new-chat dialog offers no MANAGED (server-
  //          readable) choice, so a user cannot create one by accident from the web app.
  await alice.page.click("[data-testid=new-conversation]");
  const dialog = alice.page.locator("[data-testid=new-conversation-form]");
  await dialog.waitFor({ timeout: T });
  await alice.page.locator(`[data-testid=colleague][data-name="${USERS.bob.name}"]`).waitFor({ timeout: T, state: "attached" });
  assert.equal(await dialog.locator("[data-testid=profile-managed], [data-testid=profile-e2ee], input[type=radio]").count(), 0, "no security-profile choice");
  await alice.page.keyboard.press("Escape");
  await dialog.waitFor({ state: "detached", timeout: T });
  step("new chats are private-only", {});

  // A real application archive above the legacy ceiling. Every encrypted send must
  // be accepted and every eligible source must possess it before the new browser joins.
  const bulkPrefix = `${SECRET} large-history block `;
  const bulkHistory = Array.from({ length: 205 }, (_, i) => `${bulkPrefix}${String(i).padStart(3, "0")} ${"x".repeat(4000)}`);
  assert.ok(bulkHistory.reduce((bytes, text) => bytes + Buffer.byteLength(text, "utf8"), 0) > 512 * 1024);
  // Progress timing: the log shows where this step spends its time (sends vs. each source).
  const bulkStarted = Date.now();
  const elapsed = () => `${((Date.now() - bulkStarted) / 1000).toFixed(1)}s`;
  for (const [i, text] of bulkHistory.entries()) {
    await send(alice.page, text);
    await alice.page.locator("[data-testid=message][data-state=sent]", { hasText: text }).waitFor({ timeout: T });
    if ((i + 1) % 25 === 0 || i + 1 === bulkHistory.length) console.log(`BULK sent ${i + 1}/${bulkHistory.length} at ${elapsed()}`);
  }
  const bobSources = [["bob", bob.page], ...otherDevices.filter(([who]) => who === "bob").map(([who, device], n) => [`${who}#${n + 1}`, device.page])];
  for (const [label, page] of bobSources) {
    await shown(page, bulkHistory.at(-1));
    console.log(`BULK ${label} shows newest at ${elapsed()}`);
    await loadAllPrivateHistory(page);
    await shown(page, bulkHistory[0]);
    assert.equal(await page.locator("[data-testid=message-text]", { hasText: bulkPrefix }).count(), bulkHistory.length);
    console.log(`BULK ${label} has full history at ${elapsed()}`);
  }
  step("large encrypted history accepted and available on every source", { messages: bulkHistory.length, plaintextBytes: bulkHistory.reduce((bytes, text) => bytes + Buffer.byteLength(text, "utf8"), 0) });

  // ---- 10. Bob's second device: added to the group by a member device, then his history
  //          is recovered device-to-device after both devices show the same code.
  for (const [, device] of otherDevices) await device.page.close();
  const bob2 = await open("bob");
  pages.bob2 = bob2.page;
  // ADR 0067: signing in is enough. The second browser is never asked to verify itself and
  // Bob's other browser is never asked to approve it; setup is silent.
  // Every other device goes offline before the new browser even has its messaging device:
  // nobody can add it, so it must not wait for anyone.
  await alice.page.close();
  await bob.page.close();
  await phase(bob2.page, "ready");
  assert.equal(await bob2.page.locator("[data-testid=account-link-code], [data-testid=link-email-code], [data-testid=incoming-link]").count(), 0, "no verification step");
  assert.equal(await bob2.page.getByText(/Verify this browser|Securing this chat|Waiting for secure setup|can't send messages from this browser/).count(), 0, "no setup write-up");
  step("second browser trusted by its own sign-in (no verification step)", {});
  await previewIsText(bob2.page);
  await openConversation(bob2.page, "PRIVATE_E2EE");
  // Earlier MLS secrets are not granted to the new browser; authenticated history
  // copies can arrive from Bob's authorized source, never an indefinite waiting claim.
  const older = bob2.page.locator("[data-testid=message][data-state=before-join] [data-testid=message-note]").first();
  await older.waitFor({ timeout: T });
  assert.match(await older.textContent(), /^Sent before this browser joined/);
  // The new device writes first: once Alice reads it, both are in the epoch that
  // contains it (a message encrypted in an earlier epoch is, correctly, unreadable to it).
  const fromNew = `${SECRET} from bob's new device`;
  await send(bob2.page, fromNew);
  await bob2.page.locator("[data-testid=message][data-mine=true][data-state=sent]", { hasText: fromNew }).waitFor({ timeout: T });
  step("new browser joins by itself with every other device offline", {});
  await reopen("alice", alice);
  await reopen("bob", bob);
  await openConversation(alice.page, "PRIVATE_E2EE");
  await shown(alice.page, fromNew);
  const after = `${SECRET} after the new device joined`;
  await send(alice.page, after);
  await shown(bob2.page, after);
  for (const [who, device] of otherDevices) {
    await reopen(who, device);
    await openConversation(device.page, "PRIVATE_E2EE");
    await shown(device.page, hello);
    await shown(device.page, fromNew);
    await shown(device.page, after);
  }
  step("offline registered devices recover across another device joining and epoch changes", { resumedDevices: otherDevices.length });

  // Device controls live behind the "Device & security settings" toggle, as for a user.
  const settings = bob2.page.locator("[data-testid=device-settings-toggle]");
  await settings.click();
  assert.equal(await settings.getAttribute("aria-expanded"), "true", "device settings open");
  await bob2.page.click("[data-testid=recover-history]");
  await bob2.page.locator("[data-testid=transfer-source]").first().click({ timeout: T });
  const targetCode = (await bob2.page.locator("[data-testid=outgoing-code]").textContent({ timeout: T })).trim();
  // Device directories are UUID-paginated, so the first source is not necessarily
  // Bob's oldest desktop. Approve on the actual source displaying this request's SAS.
  const sourcePages = [bob.page, ...otherDevices.filter(([who]) => who === "bob").map(([, device]) => device.page)];
  const incomingTransfer = await Promise.any(sourcePages.map(async (page) => {
    const row = page.locator("[data-testid=incoming-transfer]").filter({ hasText: targetCode });
    await row.waitFor({ timeout: 2 * T });
    return row;
  }));
  assert.equal((await incomingTransfer.locator("[data-testid=incoming-code]").textContent()).trim(), targetCode, "both devices show the same comparison code");
  let interruptedTransferId = null;
  let permitResumedDownload = false;
  let checkpointBeforeInterruption = 0;
  let receivedManifest = null;
  let signalInterruption;
  const interruption = new Promise(resolve => { signalInterruption = resolve; });
  const v2Path = /\/api\/v2\/messaging\/relay\/v1\/messaging\/history-transfers-v2\/([0-9a-f-]+)\/(manifest|checkpoint|chunks\/1)(?:\?|$)/;
  bob2.page.on("request", request => {
    if (request.method() === "POST" && /\/history-transfers-v2\/[^/]+\/checkpoint$/.test(request.url())) checkpointBeforeInterruption = Math.max(checkpointBeforeInterruption, request.postDataJSON()?.nextIndex ?? 0);
  });
  bob2.page.on("response", async response => {
    if (response.request().method() === "GET" && /\/history-transfers-v2\/[^/]+\/manifest$/.test(response.url()) && response.status() === 200) receivedManifest = await response.json().catch(() => null);
  });
  await bob2.page.route(v2Path, async route => {
    const match = v2Path.exec(route.request().url());
    if (route.request().method() === "GET" && match?.[2] === "chunks/1" && !permitResumedDownload) {
      if (!interruptedTransferId) interruptedTransferId = match[1];
      await route.abort("connectionreset");
      signalInterruption();
    } else await route.continue();
  });
  await incomingTransfer.locator("[data-testid=approve-transfer]").click();
  let interruptTimeout;
  try {
    await Promise.race([interruption, new Promise((_, reject) => { interruptTimeout = setTimeout(() => reject(Error("v2 chunk interruption was not reached")), 2 * T); })]);
  } finally { clearTimeout(interruptTimeout); }
  assert.ok(checkpointBeforeInterruption >= 1, "an authenticated first chunk was durably imported before interruption");
  const resumedCompletion = bob2.page.waitForResponse(response => response.request().method() === "POST" && response.url().endsWith(`/history-transfers-v2/${interruptedTransferId}/complete`) && response.status() === 200, { timeout: 2 * T });
  await bob2.page.reload({ waitUntil: "load" });
  permitResumedDownload = true;
  await register(bob2.page);
  await openConversation(bob2.page, "PRIVATE_E2EE");
  await resumedCompletion;
  await shown(bob2.page, bulkHistory.at(-1));
  assert.ok(await bob2.page.locator("[data-testid=load-older]").count(), "transferred history beyond 200 raw records exposes private pagination");
  await loadAllPrivateHistory(bob2.page);
  await shown(bob2.page, hello);
  await shown(bob2.page, bulkHistory[0]);
  await shown(bob2.page, bulkHistory.at(-1));
  assert.equal(await bob2.page.locator("[data-testid=message-text]", { hasText: bulkPrefix }).count(), bulkHistory.length, "every bulk record imports exactly once after interruption");
  assert.ok(receivedManifest?.totalPlaintextBytes > 512 * 1024 && receivedManifest.chunkCount > 2, "actual transferred manifest exceeds 512 KiB and requires multiple chunks");
  assert.equal(await bob2.page.locator(`[data-testid=message][data-message-id="${metadataMessageId}"][data-state=removed]`).count(), 1, "history recovery preserves the deletion tombstone");
  await bob2.page.locator("[data-testid=message]", { hasText: metadataReply }).first().locator("[data-testid=message-reply]").waitFor({ timeout: T });
  const recoveredAttachment = bob2.page.locator("[data-testid=attachment]", { hasText: "plan.txt" });
  await recoveredAttachment.waitFor({ timeout: T });
  const [recoveredDownload] = await Promise.all([bob2.page.waitForEvent("download", { timeout: T }), recoveredAttachment.click()]);
  assert.equal(recoveredDownload.suggestedFilename(), "plan.txt");
  assert.ok(readFileSync(await recoveredDownload.path()).equals(fileBody), "recovered attachment descriptor and key decrypt the original bytes on the new device");
  await bob2.page.unroute(v2Path);
  step("large v2 application history resumes automatically after chunk failure and page restart", { comparisonCode: "#### ####", messages: bulkHistory.length, chunkCount: receivedManifest.chunkCount, plaintextBytes: receivedManifest.totalPlaintextBytes });

  // ---- 11. Removing a device erases its keys and history from that browser.
  if (await settings.getAttribute("aria-expanded") !== "true") await settings.click();
  await bob2.page.click("[data-testid=remove-device]");
  await phase(bob2.page, "revoked");
  const wiped = await inspectStorage(bob2.page, SECRET);
  assert.equal(wiped.found, 0);
  step("device removal erased local state", { remainingValues: wiped.values });

  // ---- 12. Browser security on the real pages.
  for (const p of [alice.page, bob.page]) {
    assert.deepEqual(await violations(p), [], "no CSP / Trusted Types violation while messaging");
    const sink = await p.evaluate(() => {
      try {
        document.body.insertAdjacentHTML("beforeend", "<img src=x onerror=1>");
        return "assigned";
      } catch (e) {
        return e.name;
      }
    });
    assert.equal(sink, "TypeError", "untyped HTML sink refused by Trusted Types");
  }
  const workerHeaders = (await alice.page.request.get(`${BASE}/messaging-crypto/manifest.json`)).headers();
  assert.match(workerHeaders["content-security-policy"], /script-src 'self' 'wasm-unsafe-eval'/);
  assert.match(workerHeaders["content-security-policy"], /default-src 'none'/);
  step("csp + trusted types", {});

  // ---- 13. A tampered WebAssembly module is refused by the worker (pinned SHA-384).
  const tampered = await open("alice", {
    route: (ctx) =>
      ctx.route(/\/messaging-crypto\/.*\.wasm$/, async (route) => {
        const r = await route.fetch();
        const body = Buffer.from(await r.body());
        body[body.length - 100] ^= 0xff;
        await route.fulfill({ response: r, body });
      }),
  });
  await tampered.page.waitForSelector("[data-testid=messaging-error]", { timeout: T });
  assert.match(await tampered.page.locator("[data-testid=messaging-error]").textContent(), /CRYPTO_MODULE_INTEGRITY/);
  await tampered.ctx.close();
  step("tampered crypto module refused", {});

  // ---- 14. An expired / invalid login reaches no Messaging data: a session cookie the
  //          account authority refuses ends the session and sends the browser to sign in.
  const expired = await browser.newContext();
  await expired.addCookies([
    { name: "cfm_session", value: "session", domain: "127.0.0.1", path: "/" },
    { name: "cfm_rt", value: "invalid.token.value", domain: "127.0.0.1", path: "/api/session" },
  ]);
  const ep = await expired.newPage();
  await ep.goto(`${BASE}/dashboard/messages`, { waitUntil: "load" });
  await ep.waitForURL(/\/login/, { timeout: T });
  await expired.close();
  step("expired session", {});

  // Private GROUP path: a registered recipient is offline at creation and catches
  // up through the same authoritative/MLS worker path as the direct conversation.
  await bob.page.close();
  await alice.page.click("[data-testid=new-conversation]");
  await alice.page.click("[data-testid=new-group]");
  await pick(alice.page, USERS.bob.name);
  await alice.page.click("[data-testid=group-next]");
  const groupName = "Reliability private group";
  await alice.page.getByLabel("Group name", { exact: true }).fill(groupName);
  await alice.page.click("[data-testid=create-conversation]");
  await alice.page.locator("[data-testid=conversation-title]", { hasText: groupName }).waitFor({ timeout: T });
  const groupHello = `${SECRET} offline private group`;
  await send(alice.page, groupHello);
  await reopen("bob", bob);
  const groupRow = bob.page.locator("[data-testid=conversation-item]").filter({ hasText: groupName });
  await groupRow.waitFor({ timeout: T });
  await groupRow.click();
  await shown(bob.page, groupHello);
  const groupReply = `${SECRET} group reply`;
  await send(bob.page, groupReply);
  await shown(alice.page, groupReply);
  step("private group offline catch-up and bidirectional delivery", {});

  // ---- 15. Logout revokes this browser's device and erases its messaging data.
  await alice.page.locator("[data-testid=profile-menu-toggle]:visible").first().click();
  await alice.page.locator("[data-testid=logout-open]:visible").first().click();
  await alice.page.locator("[data-testid=logout-confirm]").click();
  await alice.page.waitForFunction(() => !location.pathname.startsWith("/dashboard"), null, { timeout: T });
  const afterLogout = await inspectStorage(alice.page, SECRET);
  assert.equal(afterLogout.values, 0, `nothing of the account remains: ${JSON.stringify(afterLogout)}`);
  step("logout erased the device", { remainingValues: afterLogout.values });

  // Ordinary logout with the selected PRF policy retains only locked ciphertext.
  const protectedBrowser = await open("alice", { authenticator: true });
  await register(protectedBrowser.page);
  await protectedBrowser.page.waitForFunction(async () => {
    const db = await new Promise((resolve, reject) => { const q = indexedDB.open("cfmeet-messaging"); q.onsuccess = () => resolve(q.result); q.onerror = () => reject(q.error); });
    const values = await new Promise((resolve, reject) => { const q = db.transaction("keys").objectStore("keys").getAll(); q.onsuccess = () => resolve(q.result); q.onerror = () => reject(q.error); });
    db.close(); return values.some(value => value?.kind === "LOCAL_AUTHENTICATOR_V1");
  }, null, { timeout: T });
  await openConversation(protectedBrowser.page, "PRIVATE_E2EE");
  const retainedMessage = `${SECRET} authenticator-protected logout`;
  await send(protectedBrowser.page, retainedMessage);
  await shown(protectedBrowser.page, retainedMessage);
  await protectedBrowser.page.locator("[data-testid=message][data-state=sent]", { hasText: retainedMessage }).waitFor({ timeout: T });
  const retainedDeviceId = await protectedBrowser.page.evaluate(async () => {
    const db = await new Promise((resolve, reject) => { const request = indexedDB.open("cfmeet-messaging"); request.onsuccess = () => resolve(request.result); request.onerror = () => reject(request.error); });
    const values = await new Promise((resolve, reject) => { const request = db.transaction("keys").objectStore("keys").getAll(); request.onsuccess = () => resolve(request.result); request.onerror = () => reject(request.error); });
    db.close(); return values.find(value => value?.kind === "LOCAL_AUTHENTICATOR_V1")?.descriptor.messagingDeviceId;
  });
  assert.ok(retainedDeviceId);
  await protectedBrowser.page.locator("[data-testid=profile-menu-toggle]:visible").first().click();
  await protectedBrowser.page.locator("[data-testid=logout-open]:visible").first().click();
  await protectedBrowser.page.locator("[data-testid=logout-confirm]").click();
  await protectedBrowser.page.waitForFunction(() => !location.pathname.startsWith("/dashboard"), null, { timeout: T });
  const lockedStorage = await inspectStorage(protectedBrowser.page, SECRET);
  assert.ok(lockedStorage.values > 0, "ordinary sign-out retains encrypted history");
  assert.equal(lockedStorage.found, 0, "retained values contain no plaintext");
  assert.equal(lockedStorage.keys.length, 0, "no directly usable wrapping key remains");
  await signIn(protectedBrowser.ctx, "alice", protectedBrowser.origin);
  await protectedBrowser.page.goto(`${protectedBrowser.origin}/dashboard/messages`, { waitUntil: "load" });
  await register(protectedBrowser.page);
  await openConversation(protectedBrowser.page, "PRIVATE_E2EE");
  await shown(protectedBrowser.page, retainedMessage);
  const restoredDeviceId = await protectedBrowser.page.evaluate(async () => {
    const db = await new Promise((resolve, reject) => { const request = indexedDB.open("cfmeet-messaging"); request.onsuccess = () => resolve(request.result); request.onerror = () => reject(request.error); });
    const values = await new Promise((resolve, reject) => { const request = db.transaction("keys").objectStore("keys").getAll(); request.onsuccess = () => resolve(request.result); request.onerror = () => reject(request.error); });
    db.close(); return values.find(value => value?.kind === "LOCAL_AUTHENTICATOR_V1")?.descriptor.messagingDeviceId;
  });
  assert.equal(restoredDeviceId, retainedDeviceId, "fresh sign-in reuses the same unrevoked protected Messaging identity");
  await phase(bob.page, "ready");
  step("authenticator-protected logout and fresh sign-in restore history without disconnecting other devices", { plaintextHits: lockedStorage.found });
  await protectedBrowser.ctx.close();

  for (const s of [alice, bob, bob2, ...otherDevices.map(([, device]) => device)]) {
    assert.deepEqual(s.errors.filter((e) => !/ResizeObserver/.test(e)), [], "no page errors");
    await s.ctx.close();
  }
}

