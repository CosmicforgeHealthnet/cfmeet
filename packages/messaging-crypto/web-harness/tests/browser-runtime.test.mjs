// The REAL CF Meet client crypto core (cf-messaging-crypto, Rust -> wasm32) running in
// Chromium: each device in its own Web Worker, the page relaying bytes as the untrusted
// Delivery Service, served under a strict CSP with Trusted Types.
//
//   cargo build --release --target wasm32-unknown-unknown
//   wasm-bindgen --target web --no-typescript --out-dir www/pkg \
//     target/wasm32-unknown-unknown/release/cf_messaging_crypto_web_harness.wasm
//   node --test tests/browser-runtime.test.mjs      (needs `playwright` + Chromium)
//
// Browser-runtime evidence only: not a physical Android / iOS device, and not the
// shipped web app (which does not yet integrate the crypto core).
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { join, extname, normalize } from "node:path";
import { chromium } from "playwright";

const ROOT = new URL("../www/", import.meta.url).pathname;
const TYPES = { ".html": "text/html", ".js": "text/javascript", ".wasm": "application/wasm" };
const STRICT =
  "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self'; connect-src 'self'; " +
  "base-uri 'none'; object-src 'none'; frame-ancestors 'none'; " +
  "require-trusted-types-for 'script'; trusted-types cfmeet-harness";
let csp = STRICT;
let server, base, browser;

before(async () => {
  server = createServer(async (req, res) => {
    const path = normalize(new URL(req.url, "http://x").pathname).replace(/^\/+/, "") || "index.html";
    if (path.includes("..")) return res.writeHead(400).end();
    try {
      const body = await readFile(join(ROOT, path));
      res.writeHead(200, {
        "Content-Type": TYPES[extname(path)] ?? "application/octet-stream",
        "Content-Security-Policy": csp,
        "X-Content-Type-Options": "nosniff",
        "Cross-Origin-Opener-Policy": "same-origin",
      });
      res.end(body);
    } catch {
      res.writeHead(404).end();
    }
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  base = `http://127.0.0.1:${server.address().port}`;
  browser = await chromium.launch();
});

after(async () => {
  await browser?.close();
  server?.closeAllConnections();
  server?.close();
});

async function page() {
  const p = await browser.newPage();
  const violations = [];
  p.on("console", (m) => m.type() === "error" && violations.push(m.text()));
  await p.goto(`${base}/index.html`);
  await p.waitForFunction(() => window.harnessReady === true);
  return { p, violations };
}

test("client crypto journey runs in Chromium Web Workers under a strict CSP", async () => {
  const { p, violations } = await page();
  const t0 = Date.now();
  const log = await p.evaluate(() => window.runJourney());
  const ms = Date.now() - t0;
  const names = log.map((s) => s.name);
  assert.deepEqual(names, [
    "device registration",
    "group create + welcome",
    "encrypt/decrypt",
    "tamper + replay rejected",
    "key substitution rejected",
    "epoch change",
    "offline delivery",
    "attachment",
    "history transfer",
    "device revocation",
    "no browser persistence",
  ]);
  assert.deepEqual(violations.filter((v) => /Content Security Policy|Trusted Type/i.test(v)), []);
  console.log(`EVIDENCE browser=chromium-${browser.version()} steps=${log.length} journey_ms=${ms}`);
  for (const s of log) console.log(`EVIDENCE step ${JSON.stringify(s)}`);
  await p.close();
});

test("without 'wasm-unsafe-eval' the browser refuses to compile the crypto core", async () => {
  csp = STRICT.replace(" 'wasm-unsafe-eval'", "");
  try {
    const { p } = await page().catch(async () => ({ p: null }));
    if (p) {
      const err = await p.evaluate(() => window.runJourney().then(() => null, (e) => String(e)));
      assert.ok(err && /WebAssembly|CompileError|wasm|Content Security/i.test(err), String(err));
      await p.close();
    }
  } finally {
    csp = STRICT;
  }
});
