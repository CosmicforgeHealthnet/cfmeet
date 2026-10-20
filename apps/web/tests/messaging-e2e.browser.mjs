// The REAL CF Meet web Messaging client in Chromium, against real services: the production
// Next build of apps/web, the Meeting Service (identity bridge + client relay), Platform
// Access and Messaging processes, PostgreSQL, Redis and S3-compatible storage.
//
// Driven by services/meeting-service/tests/unified_messaging_e2e.rs
// (`web_messaging_client_in_chromium_across_real_services`), which starts the services,
// seeds the accounts and passes them here:
//
//   E2E_WEB_PORT    the port `next start` listens on (the build baked the service URLs)
//   E2E_USERS       {"alice":{"token","name"},"bob":{...},"mallory":{...}}
//   E2E_SECRET      a unique phrase: it must never appear outside the devices
//   E2E_EVIDENCE    where to write the step log (JSON)
//
// Steps: device registration; an end-to-end encrypted conversation created in the UI;
// encrypted send / receive; XSS payloads rendered as text; reload (encrypted local
// state); offline delivery; network loss and reconnect; an encrypted attachment;
// a MANAGED conversation, labelled as server-readable; a second device added to the
// group, its history recovered device-to-device with comparison codes; device removal
// erasing local state; CSP / Trusted Types / worker integrity; expired session.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { chromium } from "playwright";

const PORT = Number(process.env.E2E_WEB_PORT ?? 3100);
const BASE = `http://127.0.0.1:${PORT}`;
const USERS = JSON.parse(process.env.E2E_USERS ?? "{}");
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

async function open(who, { route } = {}) {
  const ctx = await browser.newContext({ acceptDownloads: true });
  await ctx.addCookies([
    { name: "access_token", value: USERS[who].token, url: BASE },
    { name: "refresh_token", value: "not-a-refresh-token", url: BASE },
  ]);
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
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  if (process.env.E2E_DEBUG) {
    page.on("console", (m) => console.log(`[${who} console.${m.type()}] ${m.text()}`));
    page.on("requestfailed", (r) => console.log(`[${who} failed] ${r.method()} ${r.url()} ${r.failure()?.errorText}`));
    page.on("response", (r) => r.status() >= 400 && console.log(`[${who} ${r.status()}] ${r.request().method()} ${r.url()}`));
  }
  await page.goto(`${BASE}/dashboard/messages`, { waitUntil: "load" });
  return { ctx, page, errors };
}

async function phase(page, p) {
  try {
    await page.waitForSelector(`[data-testid=device-state][data-phase=${p}]`, { timeout: T });
  } catch (e) {
    const text = await page.locator("[data-testid=messages-page]").innerText().catch(() => "(no page)");
    throw new Error(`waiting for phase ${p}; the page shows:\n${text}`, { cause: e });
  }
}
const violations = (page) => page.evaluate(() => window.__violations);

async function register(page) {
  await phase(page, "needs-device");
  await page.click("[data-testid=register-device]");
  await phase(page, "ready");
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
      console.log(`--- ${who} page at failure ---\n${text}`);
    }
    throw e;
  }
});

async function journey() {
  assert.ok(USERS.alice && USERS.bob, "E2E_USERS must name alice and bob");

  // ---- 1. Device registration in the signed-in app (keys generated in the worker).
  const alice = await open("alice");
  const bob = await open("bob");
  pages.alice = alice.page;
  pages.bob = bob.page;
  await register(alice.page);
  await register(bob.page);
  const pageCsp = (await alice.page.request.get(`${BASE}/dashboard/messages`)).headers()["content-security-policy"];
  assert.ok(pageCsp && !pageCsp.includes("wasm-unsafe-eval") && !pageCsp.includes("'unsafe-eval'"), pageCsp);
  step("device registration", { devices: 2, pageCspWithoutWasmEval: true });

  // ---- 2. Alice starts an end-to-end encrypted conversation with Bob in the UI.
  await alice.page.click("[data-testid=new-conversation]");
  await alice.page.locator(`[data-testid=colleague][data-name="${USERS.bob.name}"]`).check({ timeout: T });
  await alice.page.check("[data-testid=profile-e2ee]");
  await alice.page.click("[data-testid=create-conversation]");
  await alice.page.waitForSelector("[data-testid=conversation][data-profile=PRIVATE_E2EE]", { timeout: T });
  assert.equal(await alice.page.locator("[data-testid=profile-badge]").first().getAttribute("data-profile"), "PRIVATE_E2EE");
  step("encrypted conversation created", {});

  // ---- 3. Encrypted send and receive, both directions.
  const hello = `${SECRET} hello bob`;
  await send(alice.page, hello);
  await alice.page.locator("[data-testid=message][data-mine=true][data-state=sent]").first().waitFor({ timeout: T });
  await openConversation(bob.page, "PRIVATE_E2EE");
  await shown(bob.page, hello);
  const reply = `${SECRET} hi alice`;
  await send(bob.page, reply);
  await shown(alice.page, reply);
  step("encrypted send / receive", { messages: 2 });

  // ---- 4. Hostile content is shown as text and never executes.
  const payloads = ['<img src=x onerror="window.__xss=1">', "<script>window.__xss=2</script>", "javascript:window.__xss=3", '"><svg onload=window.__xss=4>'];
  for (const p of payloads) await send(alice.page, p);
  for (const p of payloads) {
    const el = bob.page.locator("[data-testid=message-text]").filter({ hasText: p.slice(0, 12) }).last();
    await el.waitFor({ timeout: T });
    assert.equal(await el.textContent(), p);
  }
  assert.equal(await bob.page.evaluate(() => window.__xss ?? null), null);
  assert.equal(await bob.page.locator("[data-testid=messages] img, [data-testid=messages] script, [data-testid=messages] svg").count(), 0);
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
  await alice.page.locator("[data-testid=connection-state][data-connection=connected]").waitFor({ timeout: T });
  await alice.ctx.setOffline(true);
  await alice.page.locator("[data-testid=connection-state]:not([data-connection=connected])").waitFor({ timeout: T });
  const during = `${SECRET} sent during the outage`;
  await send(bob.page, during);
  await alice.ctx.setOffline(false);
  await alice.page.locator("[data-testid=connection-state][data-connection=connected]").waitFor({ timeout: T });
  await shown(alice.page, during);
  step("reconnect", {});

  // ---- 8. Encrypted attachment: encrypted before upload, decrypted on the device.
  const fileBody = Buffer.from(`${SECRET} attachment body ${"x".repeat(5000)}`);
  await alice.page.setInputFiles("[data-testid=file-input]", { name: "plan.txt", mimeType: "text/plain", buffer: fileBody });
  const att = bob.page.locator("[data-testid=attachment]", { hasText: "plan.txt" });
  await att.waitFor({ timeout: T });
  const [download] = await Promise.all([bob.page.waitForEvent("download", { timeout: T }), att.click()]);
  assert.equal(download.suggestedFilename(), "plan.txt");
  assert.ok(readFileSync(await download.path()).equals(fileBody), "decrypted bytes equal the original");
  step("encrypted attachment", { bytes: fileBody.length });

  // ---- 9. A MANAGED conversation is created only on purpose and says it is server-readable.
  await alice.page.click("[data-testid=new-conversation]");
  await alice.page.locator(`[data-testid=colleague][data-name="${USERS.bob.name}"]`).check({ timeout: T });
  await alice.page.check("[data-testid=profile-managed]");
  await alice.page.click("[data-testid=create-conversation]");
  await alice.page.waitForSelector("[data-testid=conversation][data-profile=MANAGED]", { timeout: T });
  const managedText = "managed hello (server-readable)";
  await send(alice.page, managedText);
  await openConversation(bob.page, "MANAGED");
  await shown(bob.page, managedText);
  assert.match(await bob.page.locator("[data-testid=profile-badge]").first().textContent(), /readable by CF Meet servers/);
  step("managed conversation labelled", {});

  // ---- 10. Bob's second device: added to the group by a member device, then his history
  //          is recovered device-to-device after both devices show the same code.
  const bob2 = await open("bob");
  pages.bob2 = bob2.page;
  await register(bob2.page);
  await openConversation(bob2.page, "PRIVATE_E2EE");
  await bob2.page.locator("[data-testid=e2ee-unavailable]").waitFor({ state: "detached", timeout: 3 * T });
  await bob2.page.locator("[data-testid=message][data-state=before-join]").first().waitFor({ timeout: T });
  // The new device writes first: once Alice reads it, both are in the epoch that
  // contains it (a message encrypted in an earlier epoch is, correctly, unreadable to it).
  const fromNew = `${SECRET} from bob's new device`;
  await send(bob2.page, fromNew);
  await openConversation(alice.page, "PRIVATE_E2EE");
  await shown(alice.page, fromNew);
  const after = `${SECRET} after the new device joined`;
  await send(alice.page, after);
  await shown(bob2.page, after);
  await bob2.page.click("[data-testid=recover-history]");
  await bob2.page.locator("[data-testid=transfer-source]").first().click({ timeout: T });
  const targetCode = (await bob2.page.locator("[data-testid=outgoing-code]").textContent({ timeout: T })).trim();
  const incoming = bob.page.locator("[data-testid=incoming-code]");
  await incoming.waitFor({ timeout: 2 * T });
  assert.equal((await incoming.textContent()).trim(), targetCode, "both devices show the same comparison code");
  await bob.page.click("[data-testid=approve-transfer]");
  await bob2.page.locator("[data-testid=outgoing-transfer][data-state^=received]").waitFor({ timeout: 2 * T });
  await shown(bob2.page, hello);
  step("second device + history transfer", { comparisonCode: "#### ####" });

  // ---- 11. Removing a device erases its keys and history from that browser.
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

  // ---- 14. An expired / invalid login reaches no Messaging data.
  const expired = await browser.newContext();
  await expired.addCookies([{ name: "access_token", value: "invalid.token.value", url: BASE }]);
  const ep = await expired.newPage();
  await ep.goto(`${BASE}/dashboard/messages`, { waitUntil: "load" });
  await ep.waitForURL(/\/login/, { timeout: T });
  await expired.close();
  step("expired session", {});

  // ---- 15. Logout revokes this browser's device and erases its messaging data.
  await alice.page.locator("[data-testid=profile-menu-toggle]:visible").first().click();
  await alice.page.locator("[data-testid=logout-open]:visible").first().click();
  await alice.page.locator("[data-testid=logout-confirm]").click();
  await alice.page.waitForFunction(() => !location.pathname.startsWith("/dashboard"), null, { timeout: T });
  const afterLogout = await inspectStorage(alice.page, SECRET);
  assert.equal(afterLogout.values, 0, `nothing of the account remains: ${JSON.stringify(afterLogout)}`);
  step("logout erased the device", { remainingValues: afterLogout.values });

  for (const s of [alice, bob, bob2]) {
    assert.deepEqual(s.errors.filter((e) => !/ResizeObserver/.test(e)), [], "no page errors");
    await s.ctx.close();
  }
}

