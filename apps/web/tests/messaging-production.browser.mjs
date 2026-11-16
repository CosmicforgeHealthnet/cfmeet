// Production web Messaging journey (messaging-web-verify): the real CF Meet website, real
// accounts, real sign-in through the login page, the real backend. Nothing is injected:
// no tokens, no session cookies, no test hooks. It proves the user journey
//
//   sign in → Messages is ready on its own (no "Turn on") → choose a contact → start a chat
//   → send → receive, with live updates connected
//
// plus: an offline recipient, a network interruption, a contact who has not set up
// messaging yet (pending setup, then delivered automatically), a second browser that must
// be verified by a trusted one, and a returning browser.
//
//   PROD_WEB_URL   https://www.cfmeet.com
//   PROD_ACCOUNTS  path of the JSON written by the VPS step (never printed):
//                  [{email,password,cfMeetId,labels:{<cfMeetId>: label}}, ...] (a, b, c)
//   PROD_EVIDENCE  path of the evidence JSON (no secrets, no message text)
//
//   node --test tests/messaging-production.browser.mjs
import assert from "node:assert/strict";
import test, { after } from "node:test";
import { readFileSync, writeFileSync } from "node:fs";
import { randomUUID } from "node:crypto";
import { chromium } from "playwright";

const WEB = (process.env.PROD_WEB_URL ?? "https://www.cfmeet.com").replace(/\/+$/, "");
const ACCOUNTS = JSON.parse(readFileSync(process.env.PROD_ACCOUNTS, "utf8"));
const EVIDENCE = process.env.PROD_EVIDENCE;
const T = 90_000;
const RUN = randomUUID().slice(0, 8);
const evidence = { web: WEB, startedAt: new Date().toISOString(), checks: {}, timings: {} };
let browser;

const [A, B, C] = ACCOUNTS;
const label = (viewer, of) => viewer.labels[of.cfMeetId];

function pass(name, detail = {}) {
  evidence.checks[name] = { result: "PASS", ...detail };
  console.log(`PASS ${name}`);
}

after(async () => {
  evidence.finishedAt = new Date().toISOString();
  if (EVIDENCE) writeFileSync(EVIDENCE, JSON.stringify(evidence, null, 2));
  await browser?.close();
});

/** A fresh browser profile signed in through the real login page, landing on Messages. */
async function signIn(who, name) {
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e).slice(0, 200)));
  // Failed API calls (method, path with ids redacted, status, error code): never bodies.
  page.on("response", async (r) => {
    if (r.status() < 400 || !/api\.cfmeet\.com/.test(r.url())) return;
    let code = null;
    try { code = (await r.json())?.error?.code ?? null; } catch { /* not JSON */ }
    const path = new URL(r.url()).pathname.replace(/[0-9a-f]{8}-[0-9a-f-]{27}/g, "{id}");
    (evidence.apiFailures ??= []).push({ who: name, method: r.request().method(), path, status: r.status(), code });
  });
  await page.goto(`${WEB}/login?next=%2Fdashboard%2Fmessages`, { waitUntil: "load" });
  await page.fill('input[placeholder="you@example.com"]', who.email);
  await page.fill('input[placeholder="Enter your password"]', who.password);
  const started = Date.now();
  await page.click('button[type="submit"]');
  await page.waitForURL(/\/dashboard\/messages/, { timeout: T });
  return { ctx, page, errors, name, started };
}

async function phase(page, p, timeout = T) {
  try {
    await page.waitForSelector(`[data-testid=device-state][data-phase=${p}]`, { timeout, state: "attached" });
  } catch (e) {
    const text = await page.locator("[data-testid=messages-page]").innerText().catch(() => "(no page)");
    throw new Error(`waiting for phase ${p}; the page shows:\n${text.slice(0, 1500)}`, { cause: e });
  }
}
const connected = (page, timeout = T) => page.locator("[data-testid=connection-state][data-connection=connected]").first().waitFor({ timeout, state: "attached" });

async function noManualActivation(page) {
  assert.equal(await page.locator("[data-testid=register-device]").count(), 0, "no activation button");
  const text = await page.locator("[data-testid=messages-page]").innerText();
  assert.ok(!/Turn on private messaging/i.test(text), "no 'Turn on private messaging' panel");
  assert.ok(!/Live updates unavailable/i.test(text), "no persistent live-updates warning");
}

async function startChat(page, contactLabel) {
  await page.click("[data-testid=new-conversation]");
  const box = page.locator(`[data-testid=colleague][data-name="${contactLabel}"]`);
  await box.waitFor({ timeout: T, state: "attached" });
  await page.locator("label", { has: box }).click();
  await page.click("[data-testid=create-conversation]");
  const dialog = page.locator("[data-testid=new-conversation-form]");
  try {
    await dialog.waitFor({ state: "detached", timeout: T });
  } catch (e) {
    const text = (await dialog.innerText().catch(() => "")).replace(/\s+/g, " ").slice(0, 400);
    throw new Error(`the new chat was not created; the dialog shows: ${text}; API failures: ${JSON.stringify(evidence.apiFailures ?? [])}`, { cause: e });
  }
  await page.waitForSelector("[data-testid=conversation][data-profile=PRIVATE_E2EE]", { timeout: T });
}

async function openChat(page, nth = 0) {
  const item = page.locator("[data-testid=conversation-item][data-profile=PRIVATE_E2EE]").nth(nth);
  await item.waitFor({ timeout: T });
  await item.click();
  await page.waitForSelector("[data-testid=conversation][data-profile=PRIVATE_E2EE]", { timeout: T });
}

async function openChatWith(page, contactLabel) {
  const item = page.locator("[data-testid=conversation-item][data-profile=PRIVATE_E2EE]", { hasText: contactLabel }).first();
  await item.waitFor({ timeout: T });
  await item.click();
  await page.waitForSelector("[data-testid=conversation][data-profile=PRIVATE_E2EE]", { timeout: T });
}

async function send(page, text) {
  await page.fill("[data-testid=composer]", text);
  await page.click("[data-testid=send]");
}
const shown = (page, text, timeout = T) => page.locator("[data-testid=message-text]", { hasText: text }).first().waitFor({ timeout });
const mine = (page, text, state) => page.locator(`[data-testid=message][data-mine=true][data-state=${state}]`, { hasText: text }).first();

test("production: sign in, choose a contact, chat — encryption automatic and invisible", { timeout: 30 * 60_000 }, async () => {
  assert.ok(A && B && C, "three accounts");
  browser = await chromium.launch();

  // 1. Fresh accounts: Messaging is ready after sign-in, on its own; live updates connect.
  const a1 = await signIn(A, "a1");
  await phase(a1.page, "ready");
  evidence.timings.signInToReadyMs = Date.now() - a1.started;
  await connected(a1.page);
  await noManualActivation(a1.page);
  const b1 = await signIn(B, "b1");
  await phase(b1.page, "ready");
  await connected(b1.page);
  await noManualActivation(b1.page);
  pass("fresh_accounts_ready_automatically_web_only", { signInToReadyMs: evidence.timings.signInToReadyMs });
  pass("live_updates_connected_authenticated_websocket");

  // 2. Choose a contact → start chat → send → receive (both directions, realtime).
  await startChat(a1.page, label(A, B));
  const hello = `journey ${RUN} hello`;
  let t0 = Date.now();
  await send(a1.page, hello);
  await mine(a1.page, hello, "sent").waitFor({ timeout: T });
  await openChatWith(b1.page, label(B, A)).catch(() => openChat(b1.page));
  await shown(b1.page, hello);
  evidence.timings.sendToReceiveMs = Date.now() - t0;
  const reply = `journey ${RUN} reply`;
  t0 = Date.now();
  await send(b1.page, reply);
  await shown(a1.page, reply);
  evidence.timings.replyToReceiveMs = Date.now() - t0;
  pass("contact_chat_send_receive_encrypted", { sendToReceiveMs: evidence.timings.sendToReceiveMs, replyMs: evidence.timings.replyToReceiveMs });

  // 3. Offline recipient: B is away; A's messages are encrypted and queued; B catches up.
  await b1.page.close();
  const away = [1, 2].map((i) => `journey ${RUN} while away ${i}`);
  for (const m of away) await send(a1.page, m);
  await mine(a1.page, away[1], "sent").waitFor({ timeout: T });
  b1.page = await b1.ctx.newPage();
  await b1.page.goto(`${WEB}/dashboard/messages`, { waitUntil: "load" });
  await phase(b1.page, "ready");
  await connected(b1.page);
  await openChatWith(b1.page, label(B, A)).catch(() => openChat(b1.page));
  for (const m of away) await shown(b1.page, m);
  pass("offline_recipient_receives_queued_messages", { messages: away.length });
  pass("returning_trusted_browser_starts_automatically");

  // 4. Network interruption: live updates drop and resume on their own.
  await a1.ctx.setOffline(true);
  await a1.page.locator("[data-testid=connection-state]:not([data-connection=connected])").first().waitFor({ timeout: T, state: "attached" });
  const during = `journey ${RUN} during outage`;
  await send(b1.page, during);
  await a1.ctx.setOffline(false);
  t0 = Date.now();
  await connected(a1.page);
  await shown(a1.page, during);
  evidence.timings.reconnectToMessageMs = Date.now() - t0;
  pass("reconnect_after_network_interruption", { reconnectToMessageMs: evidence.timings.reconnectToMessageMs });

  // 5. A contact who has not set up messaging yet: the chat starts, the message waits
  //    (sealed, never shown as sent), and is delivered automatically once C arrives.
  await startChat(a1.page, label(A, C));
  const early = `journey ${RUN} before setup`;
  await send(a1.page, early);
  await mine(a1.page, early, "pending-setup").waitFor({ timeout: T });
  assert.equal(await mine(a1.page, early, "sent").count(), 0, "never reported sent before it can be decrypted");
  pass("chat_with_unenrolled_contact_is_pending_not_failed");
  const c1 = await signIn(C, "c1");
  await phase(c1.page, "ready");
  await connected(c1.page);
  await noManualActivation(c1.page);
  t0 = Date.now();
  await mine(a1.page, early, "sent").waitFor({ timeout: 3 * T });
  await openChatWith(c1.page, label(C, A)).catch(() => openChat(c1.page));
  await shown(c1.page, early, 2 * T);
  evidence.timings.setupToDeliveryMs = Date.now() - t0;
  pass("pending_message_delivered_after_recipient_setup", { setupToDeliveryMs: evidence.timings.setupToDeliveryMs });

  // 6. A second browser of A: verification is required (code typed on the trusted browser),
  //    no encryption activation; afterwards both of A's browsers receive the same messages.
  const a2 = await signIn(A, "a2");
  await a2.page.locator("[data-testid=device-setup][data-linking=true]").first().waitFor({ timeout: T });
  await noManualActivation(a2.page);
  const code = (await a2.page.locator("[data-testid=account-link-code]:visible").first().textContent({ timeout: T })).trim();
  assert.match(code, /^\d{6}$/);
  await a1.page.locator("[data-testid=incoming-link]").waitFor({ timeout: 2 * T });
  await a1.page.fill("[data-testid=incoming-link-code]", code);
  await a1.page.click("[data-testid=approve-link]");
  await phase(a2.page, "ready");
  await connected(a2.page);
  pass("new_untrusted_browser_verification_only");
  await openChatWith(a2.page, label(A, B)).catch(() => openChat(a2.page));
  await a2.page.locator("[data-testid=e2ee-unavailable]").waitFor({ state: "detached", timeout: 3 * T });
  // A's first browser still shows the chat with C (step 5): open the chat with B there too.
  await openChatWith(a1.page, label(A, B));
  const sync = `journey ${RUN} to both browsers`;
  await send(b1.page, sync);
  await shown(a1.page, sync);
  await shown(a2.page, sync, 2 * T);
  pass("two_trusted_browsers_synchronize");

  // 7. No exposed encryption errors in normal operation.
  for (const s of [a1, b1, c1, a2]) {
    const text = await s.page.locator("[data-testid=messages-page]").innerText();
    assert.ok(!/could not be decrypted|CRYPTO_|DEVICE_REQUIRED|NOT_IN_GROUP|E2EE_NOT_AVAILABLE/.test(text), `${s.name}: no encryption error shown`);
    assert.deepEqual(s.errors, [], `${s.name}: no page errors`);
  }
  pass("no_exposed_encryption_errors");

  // 8. N0-11: the shipped browser registers its Calling endpoint automatically and
  //    must revoke that exact endpoint while its session is still authenticated.
  const endpointPrefix = "cfmeet.calling.endpoint-id.";
  await a2.page.waitForFunction((prefix) => Object.keys(localStorage).some((k) => k.startsWith(prefix)), endpointPrefix, { timeout: T });
  const endpointId = await a2.page.evaluate((prefix) => {
    const key = Object.keys(localStorage).find((k) => k.startsWith(prefix));
    return key ? localStorage.getItem(key) : null;
  }, endpointPrefix);
  assert.match(endpointId ?? "", /^[0-9a-f]{8}-[0-9a-f-]{27}$/i, "this browser registered a Calling endpoint");
  pass("calling_endpoint_registered_by_web_app");
  await a2.page.locator("[data-testid=profile-menu-toggle]:visible").first().click();
  await a2.page.click("[data-testid=logout-open]");
  const revoked = a2.page.waitForResponse((r) => new URL(r.url()).origin === new URL(WEB).origin
    && new URL(r.url()).pathname === `/api/calling/v1/calling/endpoints/${endpointId}`
    && r.request().method() === "DELETE", { timeout: T });
  await a2.page.click("[data-testid=logout-confirm]");
  const revokeResponse = await revoked;
  assert.equal(revokeResponse.status(), 200, "logout authenticated and revoked its Calling endpoint");
  const revokedEndpoint = await revokeResponse.json();
  assert.equal(revokedEndpoint.endpointId, endpointId);
  assert.equal(revokedEndpoint.status, "REVOKED");
  await a2.page.waitForURL(/\/login(?:[?#]|$)/, { timeout: T });
  assert.equal(await a2.page.evaluate((prefix) => Object.keys(localStorage).some((k) => k.startsWith(prefix)), endpointPrefix), false, "logout forgot the Calling endpoint id");
  pass("web_logout_revokes_registered_calling_endpoint");
  evidence.verdict = "PASS";
});
