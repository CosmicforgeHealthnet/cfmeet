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
//   E2E_SECRET      a unique phrase used in messages
//   E2E_EVIDENCE    where to write the step log (JSON)
//
// Chats are server-stored (MANAGED, 26 November 2026): signing in is all a user needs.
// Steps: sign-in with no device setup on several browsers at once; a chat started in the
// UI; send / receive both ways in real time; a burst whose per-message time must not grow
// with the chat's length; a new browser that has the full history as soon as it signs in;
// two tabs of one browser working together; reload; network loss and reconnect; a file;
// XSS payloads rendered as text. Every user-visible wait is timed (TIMING lines) so a slow
// path is measured, not guessed.
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
// Where screenshots of the real chat UI go (uploaded by CI for review); unset: none.
const SCREENS = process.env.E2E_SCREENSHOTS;
async function shot(page, name) {
  if (!SCREENS) return;
  const { mkdirSync } = await import("node:fs");
  mkdirSync(SCREENS, { recursive: true });
  await page.screenshot({ path: `${SCREENS}/${name}.png` }).catch(() => {});
}
const T = 30_000;
const BURST = 60;
const log = [];
const timings = {};
const step = (name, detail = {}) => {
  log.push({ name, ...detail, at: new Date().toISOString() });
  console.log(`STEP ${name} ${JSON.stringify(detail)}`);
};
/** Times one user-visible action; every sample is kept for the evidence log. */
async function timed(name, fn) {
  const started = performance.now();
  const result = await fn();
  const ms = Math.round(performance.now() - started);
  (timings[name] ??= []).push(ms);
  console.log(`TIMING ${name} ${ms}ms`);
  return result;
}
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
  const summary = Object.fromEntries(Object.entries(timings).map(([k, v]) => {
    const sorted = [...v].sort((a, b) => a - b);
    return [k, { n: v.length, medianMs: sorted[Math.floor(sorted.length / 2)], maxMs: sorted.at(-1) }];
  }));
  console.log(`TIMING SUMMARY ${JSON.stringify(summary)}`);
  if (EVIDENCE) writeFileSync(EVIDENCE, JSON.stringify({ steps: log, timings: summary }, null, 2));
  await browser?.close();
  if (server) process.kill(-server.pid, "SIGTERM");
});

/**
 * Signs a browser context in as the passwordless sign-in does (ACC-019): a one-time code
 * verified with the account authority (fixed-code test accounts), then the app's own
 * `POST /api/session` stores the refresh token in its httpOnly cookie. One login per
 * context, so no two contexts share a single-use refresh token.
 */
async function signIn(ctx, who) {
  const login = await fetch(`${API}/auth/otp/verify`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify({ email: USERS[who].email, code: USERS[who].code }),
  });
  const body = await login.json().catch(() => null);
  assert.equal(login.status, 200, `one-time-code sign-in for ${who}: ${JSON.stringify(body)}`);
  assert.ok(body?.refresh_token, `the login for ${who} returned a refresh token`);
  const session = await ctx.request.post(`${BASE}/api/session`, {
    headers: { "X-CF-Session": "1", Origin: BASE },
    data: { refresh_token: body.refresh_token, remember: false },
  });
  assert.equal(session.status(), 200, `session cookie for ${who}: ${await session.text()}`);
}

async function open(who) {
  const ctx = await browser.newContext({ acceptDownloads: true });
  await signIn(ctx, who);
  // What the login flow stores for a signed-in account (the dashboard requires it).
  await ctx.addInitScript((u) => {
    if (!localStorage.getItem("cosmic_user_info")) localStorage.setItem("cosmic_user_info", JSON.stringify(u));
  }, { id: USERS[who].id, email: USERS[who].email, username: USERS[who].name });
  // Every rate-limited request on any page of this browser is named in the log.
  ctx.on("response", (r) => {
    if (r.status() !== 429) return;
    const u = new URL(r.url());
    console.log(`[${who} 429] ${r.request().method()} ${u.pathname}`);
  });
  const page = await newPage(ctx, who);
  return { ctx, page };
}

async function newPage(ctx, who) {
  const page = await ctx.newPage();
  page.on("pageerror", (e) => console.log(`[${who} pageerror] ${e}`));
  if (process.env.E2E_DEBUG) page.on("console", (m) => console.log(`[${who} console.${m.type()}] ${m.text()}`));
  await page.goto(`${BASE}/dashboard/messages`, { waitUntil: "load" });
  return page;
}

/** Signed in and usable: phase ready with NO encryption device, and nothing to set up. */
async function ready(page) {
  try {
    await page.waitForSelector("[data-testid=device-state][data-phase=ready]", { timeout: T, state: "attached" });
  } catch (e) {
    const text = await page.locator("[data-testid=messages-page]").innerText().catch(() => "(no page)");
    throw new Error(`waiting for the chat to be ready; the page shows:\n${text}`, { cause: e });
  }
  assert.equal(await page.locator("[data-testid=device-setup], [data-testid=new-chat-setup], [data-testid=private-locked]").count(), 0, "no device setup, unlock or tab lock");
  assert.equal(await page.locator("[data-testid=incoming-link]").count(), 0, "no device approval");
}

const connected = (page) => page.locator("[data-testid=connection-state][data-connection=connected]").waitFor({ timeout: T, state: "attached" });

async function openChat(page) {
  const item = page.locator("[data-testid=conversation-item][data-profile=MANAGED]").first();
  await item.waitFor({ timeout: T });
  await item.click();
  await page.waitForSelector("[data-testid=conversation][data-profile=MANAGED]", { timeout: T });
}

async function type(page, text) {
  await page.fill("[data-testid=composer]", text);
  await page.click("[data-testid=send]");
}

const sentOn = (page, text) => page.locator("[data-testid=message][data-state=sent], [data-testid=message][data-state=delivered]", { hasText: text }).first().waitFor({ timeout: T });
const shown = (page, text) => page.locator("[data-testid=message-text]", { hasText: text }).first().waitFor({ timeout: T });

/** Sends from one page and times both the sender's confirmation and the live arrival. */
async function exchange(from, to, text) {
  const started = performance.now();
  await type(from, text);
  await Promise.all([
    timed("send-confirmed", () => sentOn(from, text)),
    shown(to, text).then(() => {
      const ms = Math.round(performance.now() - started);
      (timings["live-delivery"] ??= []).push(ms);
      console.log(`TIMING live-delivery ${ms}ms`);
    }),
  ]);
}

async function textsIn(page) {
  return page.locator("[data-testid=message-text]").allTextContents();
}

const pages = {};
test("the CF Meet web app runs real server-stored messaging", { timeout: 15 * 60_000 }, async () => {
  try {
    await journey();
  } catch (e) {
    for (const [who, p] of Object.entries(pages)) {
      await shot(p, `failure-${who}`);
      const dialogs = await p.evaluate(() => [...document.querySelectorAll("[role=dialog], [data-slot=dialog-overlay]")]
        .map((d) => `${d.getAttribute("data-slot") ?? d.getAttribute("role")} [${d.getAttribute("data-testid") ?? ""}] ${(d.textContent ?? "").slice(0, 200)}`)).catch(() => []);
      if (dialogs.length) console.log(`--- ${who} open dialogs ---\n${dialogs.join("\n")}`);
      const text = await p.locator("[data-testid=messages-page]").innerText().catch(() => "(closed)");
      console.log(`--- ${who} page at failure ---\n${text.length > 12000 ? text.slice(-12000) : text}`);
    }
    throw e;
  }
});

async function journey() {
  assert.ok(USERS.alice && USERS.bob, "E2E_USERS must name alice and bob");

  // ---- 1. Signing in is all it takes: no device setup, no unlock, no approval.
  const alice = await open("alice");
  const bob = await open("bob");
  pages.alice = alice.page;
  pages.bob = bob.page;
  await timed("ready-after-load", () => ready(alice.page));
  await timed("ready-after-load", () => ready(bob.page));
  step("sign-in is enough", { browsers: 2 });

  // ---- 2. Alice starts a chat with Bob in the UI; it is server-stored.
  await alice.page.click("[data-testid=new-conversation]");
  const row = alice.page.locator(`[data-testid=colleague][data-name="${USERS.bob.name}"]`);
  await row.waitFor({ timeout: T, state: "attached" });
  await timed("create-chat", async () => {
    await row.click();
    await alice.page.waitForSelector("[data-testid=conversation][data-profile=MANAGED]", { timeout: T });
  });
  const hello = `${SECRET} hello bob`;
  await type(alice.page, hello);
  await sentOn(alice.page, hello);
  await timed("open-chat", () => openChat(bob.page));
  await shown(bob.page, hello);
  step("chat started in the UI", { profile: "MANAGED" });

  // ---- 3. Real-time send and receive, both directions.
  await connected(alice.page);
  await connected(bob.page);
  for (let i = 0; i < 3; i++) {
    await exchange(bob.page, alice.page, `${SECRET} bob says ${i}`);
    await exchange(alice.page, bob.page, `${SECRET} alice says ${i}`);
  }
  step("real-time send and receive", { messages: 6 });

  // ---- 3b. Ticks as on WhatsApp: ✓✓ as soon as Bob's browser has the message, wherever
  //          he is in CF Meet (his chat closed), and read only once he opens the chat.
  await bob.page.goto(`${BASE}/dashboard`, { waitUntil: "load" });
  await bob.page.waitForTimeout(1500);
  const away = `${SECRET} while bob is on another page`;
  await type(alice.page, away);
  const awayBubble = alice.page.locator("[data-testid=message]", { hasText: away }).first();
  await timed("delivered-tick", () => alice.page.locator("[data-testid=message][data-receipt=delivered]", { hasText: away }).first().waitFor({ timeout: T }));
  assert.equal(await awayBubble.getAttribute("data-receipt"), "delivered", "delivered, not read, while Bob has not opened the chat");
  await bob.page.goto(`${BASE}/dashboard/messages`, { waitUntil: "load" });
  await ready(bob.page);
  await openChat(bob.page);
  await shown(bob.page, away);
  await timed("read-tick", () => alice.page.locator("[data-testid=message][data-receipt=read]", { hasText: away }).first().waitFor({ timeout: T }));
  step("ticks: delivered away from the chat, read when opened", {});

  // ---- 4. A burst: written quickly, delivered in order, and each message no slower
  //         late in the chat than early on.
  const burst = Array.from({ length: BURST }, (_, i) => `${SECRET} burst ${String(i).padStart(3, "0")}`);
  const perMessage = [];
  for (const text of burst) {
    const started = performance.now();
    await type(alice.page, text);
    await sentOn(alice.page, text);
    perMessage.push(performance.now() - started);
  }
  await shown(bob.page, burst.at(-1));
  const order = await textsIn(bob.page);
  const idx = burst.map((m) => order.indexOf(m));
  assert.ok(idx.every((v, i) => v >= 0 && (i === 0 || v > idx[i - 1])), `burst order ${idx}`);
  const avg = (a) => Math.round(a.reduce((s, v) => s + v, 0) / a.length);
  const early = avg(perMessage.slice(0, 10)), late = avg(perMessage.slice(-10));
  console.log(`TIMING burst-first10-avg ${early}ms burst-last10-avg ${late}ms`);
  timings["burst-send"] = perMessage.map(Math.round);
  assert.ok(late <= Math.max(3 * early, early + 1500), `sending must not slow down as the chat grows: first 10 avg ${early} ms, last 10 avg ${late} ms`);
  step("burst in order without slowdown", { messages: BURST, first10AvgMs: early, last10AvgMs: late });

  // ---- 5. A new browser for Bob: the full history is there as soon as he signs in.
  const bob2 = await open("bob");
  pages.bob2 = bob2.page;
  await ready(bob2.page);
  await timed("new-device-history", async () => {
    await openChat(bob2.page);
    await shown(bob2.page, burst.at(-1));
  });
  // Older messages are reachable too (paging back if the chat shows a window).
  for (let i = 0; i < 10 && !(await bob2.page.locator("[data-testid=message-text]", { hasText: hello }).count()); i++) {
    const more = bob2.page.locator("[data-testid=load-older]");
    if (!(await more.count())) break;
    await timed("load-older", () => more.click());
    await bob2.page.waitForTimeout(250);
  }
  await shown(bob2.page, hello);
  step("new browser has the full history", { messages: BURST + 7 });

  // ---- 6. Both of Bob's browsers live at once: a message reaches both.
  await connected(bob2.page);
  const both = `${SECRET} to both of bob's browsers`;
  await type(alice.page, both);
  await Promise.all([shown(bob.page, both), shown(bob2.page, both)]);
  const fromSecond = `${SECRET} sent from bob's second browser`;
  await exchange(bob2.page, alice.page, fromSecond);
  await shown(bob.page, fromSecond);
  step("two browsers of one account in sync", {});

  // ---- 7. Two tabs of one browser: no one-tab lock.
  const aliceTab = await newPage(alice.ctx, "alice");
  pages.aliceTab = aliceTab;
  await ready(aliceTab);
  await openChat(aliceTab);
  await shown(aliceTab, fromSecond);
  const fromTab = `${SECRET} from alice's second tab`;
  await type(aliceTab, fromTab);
  await Promise.all([shown(alice.page, fromTab), shown(bob.page, fromTab)]);
  await aliceTab.close();
  delete pages.aliceTab;
  step("two tabs work together", {});

  // ---- 8. Reload: the chat comes back from the server.
  await timed("reload-to-history", async () => {
    await bob.page.reload({ waitUntil: "load" });
    await ready(bob.page);
    await openChat(bob.page);
    await shown(bob.page, fromTab);
  });
  step("reload", {});

  // ---- 9. Network loss and reconnect.
  await connected(alice.page);
  await alice.ctx.setOffline(true);
  await alice.page.locator("[data-testid=connection-state]:not([data-connection=connected])").waitFor({ timeout: T, state: "attached" });
  const during = `${SECRET} sent during the outage`;
  await type(bob.page, during);
  await sentOn(bob.page, during);
  await alice.ctx.setOffline(false);
  await timed("reconnect-catch-up", async () => {
    await connected(alice.page);
    await shown(alice.page, during);
  });
  step("reconnect", {});

  // ---- 10. A file: sent, received live, downloaded byte for byte.
  const fileBody = Buffer.from(`${SECRET} attachment body ${"x".repeat(200_000)}`);
  await timed("file-delivered", async () => {
    await alice.page.setInputFiles("[data-testid=file-input]", { name: "plan.txt", mimeType: "text/plain", buffer: fileBody });
    // A picked file is previewed first (with an optional caption) and sent with Send.
    await alice.page.locator("[data-testid=attachment-preview]").waitFor({ timeout: T });
    await alice.page.click("[data-testid=attachment-send]");
    await bob.page.locator("[data-testid=attachment]", { hasText: "plan.txt" }).waitFor({ timeout: 2 * T });
  });
  const att = bob.page.locator("[data-testid=attachment]", { hasText: "plan.txt" });
  const [download] = await timed("file-download", () => Promise.all([bob.page.waitForEvent("download", { timeout: T }), att.click()]));
  assert.equal(download.suggestedFilename(), "plan.txt");
  assert.ok(readFileSync(await download.path()).equals(fileBody), "downloaded bytes equal the original");
  step("file", { bytes: fileBody.length });

  // ---- 11. Hostile text renders as text.
  const payloads = [`<img src=x onerror="window.__xss=1">`, `<script>window.__xss=2</script>`, `"><svg onload=window.__xss=4>`];
  for (const p of payloads) await type(alice.page, p);
  for (const p of payloads) await shown(bob.page, p);
  assert.equal(await bob.page.evaluate(() => window.__xss ?? null), null, "no payload executed");
  step("xss payloads inert", { payloads: payloads.length });
  await shot(alice.page, "01-direct-chat-alice");
  await shot(bob.page, "02-direct-chat-bob");

  // ---- 12. A group, as on WhatsApp: named at creation, the name everywhere, members under
  //          the name, and the sender's name in the chat list preview.
  await alice.page.click("[data-testid=new-conversation]");
  await alice.page.click("[data-testid=new-group]");
  await alice.page.locator(`[data-testid=colleague][data-name="${USERS.bob.name}"]`).click();
  await alice.page.click("[data-testid=group-next]");
  await alice.page.fill("input[aria-label='Group name']", "Weekend plans");
  await alice.page.click("[data-testid=create-conversation]");
  // The New chat dialog closes once the group exists, and the group opens.
  await alice.page.locator("[data-testid=new-conversation-form]").waitFor({ state: "detached", timeout: T });
  await alice.page.locator("[data-testid=conversation] [data-testid=group-members]").waitFor({ timeout: T });
  const groupHello = `${SECRET} hello group`;
  await type(alice.page, groupHello);
  const groupItem = bob.page.locator("[data-testid=conversation-item]", { hasText: "Weekend plans" }).first();
  await groupItem.waitFor({ timeout: T });
  await groupItem.click();
  await shown(bob.page, groupHello);
  assert.ok((await bob.page.locator("[data-testid=group-members]").innerText()).includes("You"), "the group header lists its members");
  const fromBob = `${SECRET} bob in the group`;
  await type(bob.page, fromBob);
  await shown(alice.page, fromBob);
  const aliceItem = alice.page.locator("[data-testid=conversation-item]", { hasText: "Weekend plans" }).first();
  await aliceItem.waitFor({ timeout: T });
  const firstName = USERS.bob.name.split(" ")[0];
  await alice.page.waitForFunction((name) => [...document.querySelectorAll("[data-testid=conversation-item]")]
    .some((el) => el.textContent.includes("Weekend plans") && el.textContent.includes(`${name}: `)), firstName, { timeout: T });
  await shot(alice.page, "03-group-alice");
  await shot(bob.page, "04-group-bob");
  step("group: named, members in the header, sender in the preview", {});

  // ---- 13. Every texting action, live on the other side (WhatsApp behaviour).
  const bubble = (page, text) => page.locator("[data-testid=message]", { hasText: text }).first();
  const menu = async (page, text, item) => {
    await bubble(page, text).hover();
    await bubble(page, text).locator("[data-testid=message-actions]").click();
    await page.click(`[data-testid=message-menu] [data-testid=${item}]`);
  };
  // Reply: Bob's quote shows on Alice's side.
  const replyText = `${SECRET} replying to bob`;
  await menu(alice.page, fromBob, "reply");
  await alice.page.locator("[data-testid=reply-banner]").waitFor({ timeout: T });
  await type(alice.page, replyText);
  await bob.page.locator("[data-testid=message]", { hasText: replyText }).locator("[data-testid=message-reply]").waitFor({ timeout: T });
  // Reaction: Bob reacts, Alice sees it; Bob removes it, Alice sees it go.
  await bubble(bob.page, replyText).hover();
  await bubble(bob.page, replyText).getByRole("button", { name: "React", exact: true }).click();
  await bob.page.getByRole("button", { name: "React 👍" }).first().click();
  await timed("reaction-live", () => bubble(alice.page, replyText).locator("[data-testid=reaction]", { hasText: "👍" }).waitFor({ timeout: T }));
  await bubble(bob.page, replyText).locator("[data-testid=reaction]", { hasText: "👍" }).click();
  await bubble(alice.page, replyText).locator("[data-testid=reaction]").waitFor({ state: "detached", timeout: T });
  // Edit: the new text replaces the old one on Bob's side.
  const edited = `${SECRET} edited reply`;
  await menu(alice.page, replyText, "edit");
  await alice.page.fill("[data-testid=edit-input]", edited);
  await alice.page.click("[data-testid=edit-save]");
  await timed("edit-live", () => shown(bob.page, edited));
  // Delete for everyone: gone from Bob's chat.
  await menu(alice.page, edited, "delete");
  await bob.page.locator("[data-testid=message-text]", { hasText: edited }).waitFor({ state: "detached", timeout: T });
  // Typing: Alice sees Bob typing, and it stops when he clears the box.
  await bob.page.fill("[data-testid=composer]", "typing…");
  await bob.page.type("[data-testid=composer]", " x");
  await timed("typing-indicator", () => alice.page.getByText(/is typing…/).first().waitFor({ timeout: T }));
  await bob.page.fill("[data-testid=composer]", "");
  step("reply, reaction, edit, delete for everyone, typing — live on the other side", {});

  // Forward (WhatsApp): quick ➦ arrow, tick a chat, add a message, send once.
  const forwardNote = `${SECRET} see this`;
  await bubble(alice.page, fromBob).hover();
  await bubble(alice.page, fromBob).locator("[data-testid=quick-forward]").click();
  await alice.page.locator("[data-testid=forward-dialog]").waitFor({ timeout: T });
  await alice.page.locator(`[data-testid=forward-target][data-name="${USERS.bob.name}"]`).first().click();
  await alice.page.fill("[data-testid=forward-note]", forwardNote);
  await alice.page.click("[data-testid=forward-send]");
  await alice.page.locator("[data-testid=forward-dialog]").waitFor({ state: "detached", timeout: T });
  await bob.page.locator("[data-testid=conversation-item]", { hasText: USERS.alice.name }).first().click();
  await bob.page.locator("[data-testid=message]", { hasText: fromBob }).locator("[data-testid=message-forwarded]").last().waitFor({ timeout: T });
  await shown(bob.page, forwardNote);
  // A file forwards too (re-sent: the server doesn't copy attachments).
  await alice.page.locator("[data-testid=conversation-item]", { hasText: USERS.bob.name }).first().click();
  await bubble(alice.page, "plan.txt").hover();
  await bubble(alice.page, "plan.txt").locator("[data-testid=quick-forward]").click();
  await alice.page.locator('[data-testid=forward-target][data-name="Weekend plans"]').first().click();
  await alice.page.click("[data-testid=forward-send]");
  await alice.page.locator("[data-testid=forward-dialog]").waitFor({ state: "detached", timeout: T });
  await bob.page.locator("[data-testid=conversation-item]", { hasText: "Weekend plans" }).first().click();
  await bob.page.locator("[data-testid=attachment]", { hasText: "plan.txt" }).waitFor({ timeout: 2 * T });
  await shot(alice.page, "05-forward-alice");
  step("forward: text with a note, and a file, to chats picked in the dialog", {});

  // Unread badge: a message to a chat Alice isn't looking at shows a badge; opening clears it.
  const unreadText = `${SECRET} while alice looks elsewhere`;
  await type(bob.page, unreadText);
  const groupRow = alice.page.locator("[data-testid=conversation-item]", { hasText: "Weekend plans" }).first();
  await timed("unread-badge", () => groupRow.locator("[data-testid=unread-badge]").waitFor({ timeout: T }));
  await groupRow.click();
  await shown(alice.page, unreadText);
  await groupRow.locator("[data-testid=unread-badge]").waitFor({ state: "detached", timeout: T });
  // Search in chat finds an older message.
  await alice.page.click("[data-testid=conversation-search-toggle]");
  await alice.page.fill("[data-testid=conversation-message-search] input", "hello group");
  await alice.page.keyboard.press("Enter");
  await timed("search", () => alice.page.locator("[data-testid=search-results]", { hasText: "hello group" }).waitFor({ timeout: T }));
  // Message info lists Bob as having read Alice's message.
  await alice.page.keyboard.press("Escape");
  await menu(alice.page, groupHello, "message-info");
  await alice.page.locator("[data-testid=info-read]").waitFor({ timeout: T });
  await alice.page.keyboard.press("Escape");
  step("unread badge, search in chat, message info", {});

  // ---- 14. Group info, as on WhatsApp.
  await alice.page.click("[data-testid=conversation-members-toggle]");
  const info = alice.page.locator("[data-testid=members]");
  await info.locator("[data-testid=info-title]", { hasText: "Weekend plans" }).waitFor({ timeout: T });
  assert.match(await info.locator("[data-testid=info-subtitle]").innerText(), /Group · 2 members/);
  await info.locator("[data-testid=admin-badge]").first().waitFor({ timeout: T });
  await info.getByRole("button", { name: "Search members" }).first().click();
  await info.locator("[data-testid=member-search]").fill(USERS.bob.name.slice(0, 4));
  assert.equal(await info.locator("[data-testid=member-row]").count(), 1, "member search filters the list");
  await info.locator("[data-testid=member-search]").fill("");
  await info.locator("[data-testid=invite-link]").click();
  await timed("invite-link", () => info.locator("[data-testid=invite-url]").waitFor({ timeout: T }));
  assert.match(await info.locator("[data-testid=invite-url]").innerText(), /\/dashboard\/messages\/join\?token=/);
  const [exported] = await Promise.all([alice.page.waitForEvent("download", { timeout: T }), info.locator("[data-testid=export-chat]").click()]);
  const exportedText = readFileSync(await exported.path(), "utf8");
  assert.ok(exportedText.includes("hello group") && exportedText.includes("Weekend plans"), "the export holds the chat");
  await shot(alice.page, "06-group-info");
  await info.getByRole("button", { name: "Close chat info" }).click();
  step("group info: title, members, admin badge, member search, invite link, export", {});
}

/**
 * Phone-sized tour (screenshots only, never fails the run): the main screens at an iPhone
 * viewport with touch, after the journey has filled the chats, so mobile layouts can be
 * reviewed (and compared between versions) from CI artifacts.
 */
test("phone-sized screens (screenshots)", { timeout: 5 * 60_000 }, async () => {
  if (!SCREENS || !USERS.bob) return;
  const ctx = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 2, isMobile: true, hasTouch: true });
  try {
    await signIn(ctx, "bob");
    await ctx.addInitScript((u) => {
      if (!localStorage.getItem("cosmic_user_info")) localStorage.setItem("cosmic_user_info", JSON.stringify(u));
    }, { id: USERS.bob.id, email: USERS.bob.email, username: USERS.bob.name });
    const page = await ctx.newPage();
    const settle = () => page.waitForTimeout(3000);
    const visit = async (path, name) => {
      await page.goto(`${BASE}${path}`, { waitUntil: "load" }).catch(() => {});
      await settle();
      await shot(page, name);
    };
    await visit("/dashboard", "m01-home");
    await visit("/dashboard/messages", "m02-chats");
    const rows = page.locator("[data-testid=conversation-item]");
    if (await rows.count().catch(() => 0)) {
      await rows.first().click().catch(() => {});
      await settle();
      await shot(page, "m03-chat-open");
      await page.locator("[data-testid=conversation-members-toggle]").click().catch(() => {});
      await settle();
      await shot(page, "m04-chat-info");
      await page.keyboard.press("Escape").catch(() => {});
      await page.goto(`${BASE}/dashboard/messages`, { waitUntil: "load" }).catch(() => {});
      await settle();
      const group = page.locator("[data-testid=conversation-item]", { hasText: "Weekend plans" }).first();
      if (await group.count().catch(() => 0)) {
        await group.click().catch(() => {});
        await settle();
        await shot(page, "m05-group-open");
      }
    }
    await visit("/dashboard/contacts", "m06-contacts");
    await visit("/dashboard/calls", "m07-calls");
    await visit("/dashboard/meetings", "m08-meetings");
    await visit("/dashboard/calendar", "m09-calendar");
    await visit("/dashboard/organizations", "m10-organizations");
    await visit("/dashboard/settings", "m11-settings");
    // The app menu (mobile sidebar).
    await page.goto(`${BASE}/dashboard`, { waitUntil: "load" }).catch(() => {});
    await settle();
    await page.getByRole("button", { name: /menu/i }).first().click({ timeout: 3000 }).catch(() => {});
    await page.waitForTimeout(800);
    await shot(page, "m12-menu");
  } catch (e) {
    console.log(`phone tour stopped: ${e}`);
  } finally {
    await ctx.close().catch(() => {});
  }
});
