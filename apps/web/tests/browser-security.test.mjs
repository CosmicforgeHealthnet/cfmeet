// Browser security of the built web app (Part V remediation M10), in real Chromium:
// every document carries a fresh-nonce CSP with Trusted Types and the static security
// headers; the app hydrates with NO policy violation; and injected script, untyped DOM
// XSS sinks, eval and framing are refused by the browser.
//
//   npm run build && node --test tests/browser-security.test.mjs
//
// Needs a Playwright Chromium (CI: `npx playwright install --with-deps chromium`).
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { chromium } from "playwright";

const PORT = 4300 + Math.floor(Math.random() * 500);
const BASE = `http://127.0.0.1:${PORT}`;
const PAGES = ["/", "/login", "/signup", "/join", "/forgot-password"];
let server;
let browser;

before(async () => {
  server = spawn("npx", ["next", "start", "-p", String(PORT), "-H", "127.0.0.1"], {
    env: { ...process.env, NODE_ENV: "production", NEXT_TELEMETRY_DISABLED: "1" },
    stdio: "ignore",
    // Its own process group: `npx` starts `next` as a child; both are stopped together.
    detached: true,
  });
  for (let i = 0; i < 120; i++) {
    try {
      const r = await fetch(`${BASE}/login`, { signal: AbortSignal.timeout(5000) });
      if (r.status < 500) break;
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  browser = await chromium.launch();
});

after(async () => {
  await browser?.close();
  if (server) process.kill(-server.pid, "SIGTERM");
});

async function open(path) {
  const page = await browser.newPage();
  await page.addInitScript(() => {
    window.__violations = [];
    document.addEventListener("securitypolicyviolation", (e) =>
      window.__violations.push(`${e.effectiveDirective} ${e.blockedURI} ${e.sample}`),
    );
  });
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  const response = await page.goto(`${BASE}${path}`, { waitUntil: "load", timeout: 30000 });
  // Let client components hydrate and run their effects.
  await page.waitForTimeout(1500);
  return { page, response, errors };
}

test("every page sends a nonce CSP with Trusted Types and hydrates without violations", async () => {
  const nonces = new Set();
  for (const path of PAGES) {
    const { page, response, errors } = await open(path);
    const h = response.headers();
    const csp = h["content-security-policy"];
    assert.ok(csp, `${path}: CSP header`);
    const nonce = /'nonce-([^']+)'/.exec(csp)?.[1];
    assert.ok(nonce, `${path}: nonce`);
    nonces.add(nonce);
    for (const d of [
      "object-src 'none'",
      "base-uri 'none'",
      "frame-ancestors 'none'",
      "require-trusted-types-for 'script'",
      "'strict-dynamic'",
    ]) {
      assert.ok(csp.includes(d), `${path}: ${d} in ${csp}`);
    }
    const scriptSrc = csp.split(";").find((d) => d.trim().startsWith("script-src"));
    assert.ok(!scriptSrc.includes("'unsafe-inline'") && !scriptSrc.includes("'unsafe-eval'"), scriptSrc);
    assert.equal(h["x-content-type-options"], "nosniff");
    assert.equal(h["x-frame-options"], "DENY");
    assert.equal(h["cross-origin-opener-policy"], "same-origin");
    assert.ok(h["permissions-policy"].includes("geolocation=()"));
    assert.ok(h["strict-transport-security"]);
    assert.equal(h["x-powered-by"], undefined);
    // The framework's own nonced scripts ran (React Server Components payload).
    assert.equal(await page.evaluate(() => Array.isArray(self.__next_f)), true, `${path}: hydrated`);
    const violations = await page.evaluate(() => window.__violations);
    assert.deepEqual(violations, [], `${path}: CSP / Trusted Types violations`);
    assert.deepEqual(
      errors.filter((e) => /TrustedHTML|TrustedScript|Content Security Policy|unsafe-eval/i.test(e)),
      [],
      `${path}: policy errors`,
    );
    await page.close();
  }
  assert.equal(nonces.size, PAGES.length, "a fresh nonce per document");
});

test("injected script, untyped DOM sinks, eval and framing are refused", async () => {
  const { page } = await open("/login");
  const outcome = await page.evaluate(() => {
    const r = {};
    try {
      const s = document.createElement("script");
      s.textContent = "window.__pwned = 1";
      document.body.appendChild(s);
      r.script = "appended";
    } catch (e) {
      r.script = e.name;
    }
    try {
      document.body.insertAdjacentHTML("beforeend", '<img src=x onerror="window.__pwned2 = 1">');
      r.html = "inserted";
    } catch (e) {
      r.html = e.name;
    }
    return r;
  });
  await page.waitForTimeout(300);
  assert.equal(outcome.script, "TypeError", "Trusted Types refuse an untyped script body");
  assert.equal(outcome.html, "TypeError", "Trusted Types refuse untyped HTML");
  assert.equal(await page.evaluate(() => window.__pwned ?? window.__pwned2 ?? null), null);
  // Markup injected into the served document, under the server's own CSP header: an
  // inline script without the nonce never runs, and even a script carrying the nonce
  // cannot eval. (Code inserted through DevTools evaluation is exempt from CSP, so this
  // is checked on parsed markup.)
  const injected = await browser.newPage();
  await injected.route("**/login", async (route) => {
    const r = await route.fetch();
    const nonce = /'nonce-([^']+)'/.exec(r.headers()["content-security-policy"])[1];
    const html = (await r.text()).replace(
      "</body>",
      `<script>window.__inline = 1</script>` +
        `<script nonce="${nonce}">try { window.__eval = String(eval("1 + 1")); } catch (e) { window.__eval = e.name; }` +
        ` try { window.__fn = String(new Function("return 3")()); } catch (e) { window.__fn = e.name; }</script></body>`,
    );
    await route.fulfill({ response: r, body: html });
  });
  await injected.goto(`${BASE}/login`, { waitUntil: "load" });
  await injected.waitForTimeout(500);
  const ran = await injected.evaluate(() => ({ inline: window.__inline ?? null, eval: window.__eval, fn: window.__fn }));
  assert.deepEqual(ran, { inline: null, eval: "EvalError", fn: "EvalError" }, "CSP blocks injected inline script and eval");
  await injected.close();
  // Framing: a page that frames the app gets nothing (frame-ancestors / X-Frame-Options).
  const host = await browser.newPage();
  await host.setContent(`<iframe id="f" src="${BASE}/login"></iframe>`);
  await host.waitForTimeout(1500);
  const framed = host.frames().find((f) => f.url().includes("/login"));
  const framedBody = framed ? await framed.evaluate(() => document.body?.innerText ?? "").catch(() => "") : "";
  assert.equal(framedBody.trim(), "", "the app does not render inside a foreign frame");
  await host.close();
  await page.close();
});

test("client navigation loads route chunks under Trusted Types; the default policy admits nothing else", async () => {
  const { page, errors } = await open("/login");
  const consoleErrors = [];
  page.on("console", (m) => m.type() === "error" && consoleErrors.push(m.text()));
  await page.evaluate(() => {
    window.__spa = "still-the-same-document";
  });
  await page.click('a[href="/signup"]');
  await page.waitForFunction(() => window.location.pathname === "/signup", null, { timeout: 30000 });
  await page.waitForTimeout(1000);
  assert.equal(await page.evaluate(() => window.__spa ?? null), "still-the-same-document", "client-side navigation, no full reload");
  assert.deepEqual(await page.evaluate(() => window.__violations), [], "no CSP / Trusted Types violation");
  assert.deepEqual(consoleErrors.filter((e) => /Trusted|RSC payload/i.test(e)), [], "no Trusted Types fallback");
  const r = await page.evaluate(() => {
    const out = {};
    const tryUrl = (name, url) => {
      try {
        document.createElement("script").src = url;
        out[name] = "assigned";
      } catch (e) {
        out[name] = e.name;
      }
    };
    tryUrl("crossOrigin", "https://evil.example/x.js");
    tryUrl("dataUrl", "data:text/javascript,alert(1)");
    tryUrl("outsideStatic", "/api/x.js");
    tryUrl("traversal", "/_next/static/../../evil.js");
    tryUrl("notJs", "/_next/static/chunks/x.css");
    tryUrl("ownChunk", "/_next/static/chunks/route.js");
    try {
      document.body.innerHTML += "<b>x</b>";
      out.html = "assigned";
    } catch (e) {
      out.html = e.name;
    }
    return out;
  });
  assert.deepEqual(r, {
    crossOrigin: "TypeError",
    dataUrl: "TypeError",
    outsideStatic: "TypeError",
    traversal: "TypeError",
    notJs: "TypeError",
    ownChunk: "assigned",
    html: "TypeError",
  });
  assert.deepEqual(errors.filter((e) => /Trusted/i.test(e)), []);
  await page.close();
});
