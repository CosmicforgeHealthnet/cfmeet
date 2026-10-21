// The CSP builder (src/lib/security/csp.ts) and a static guard against DOM XSS sinks in
// the web app's source.  node --experimental-strip-types --test tests/csp.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { buildCsp, MESSAGING_CRYPTO_PATH, MESSAGING_CRYPTO_WORKER_CSP, SECURITY_HEADERS } from "../src/lib/security/csp.ts";

const directive = (csp, name) =>
  csp.split(";").map((d) => d.trim()).find((d) => d === name || d.startsWith(`${name} `));

test("production policy: nonce scripts only, Trusted Types, no plugins / framing / base", () => {
  const csp = buildCsp({ nonce: "AAAAAAAAAAAAAAAAAAAAAAAA", dev: false, secure: true, apiUrl: "https://api.cfmeet.com/v1" });
  assert.equal(directive(csp, "script-src"), "script-src 'self' 'nonce-AAAAAAAAAAAAAAAAAAAAAAAA' 'strict-dynamic'");
  assert.ok(directive(csp, "connect-src").includes("https://api.cfmeet.com"), "API origin only, not its path");
  assert.ok(!directive(csp, "connect-src").includes("/v1"));
  for (const d of ["object-src 'none'", "base-uri 'none'", "frame-ancestors 'none'", "require-trusted-types-for 'script'", "upgrade-insecure-requests"]) {
    assert.ok(csp.includes(d), d);
  }
  assert.ok(!csp.includes("'unsafe-eval'"));
});

test("pages never compile WebAssembly; only the crypto worker's own policy allows it", () => {
  const nonce = "AAAAAAAAAAAAAAAAAAAAAAAA";
  for (const csp of [buildCsp({ nonce, dev: false }), buildCsp({ nonce, dev: false, secure: true })]) {
    assert.ok(!csp.includes("wasm-unsafe-eval"), csp);
  }
  const w = MESSAGING_CRYPTO_WORKER_CSP;
  assert.equal(directive(w, "default-src"), "default-src 'none'");
  assert.equal(directive(w, "script-src"), "script-src 'self' 'wasm-unsafe-eval'");
  assert.equal(directive(w, "connect-src"), "connect-src 'self'");
  assert.equal(directive(w, "worker-src"), "worker-src 'none'");
  assert.ok(!w.includes("'unsafe-eval'") && !w.includes("'unsafe-inline'") && !w.includes("*"), w);
  assert.equal(MESSAGING_CRYPTO_PATH, "/messaging-crypto");
});

test("messaging and storage origins: HTTPS, or loopback HTTP only; origin, never a path", () => {
  const nonce = "DDDDDDDDDDDDDDDDDDDDDDDD";
  const csp = buildCsp({ nonce, dev: false, messagingUrl: "https://msg.cfmeet.com/", storageUrl: "https://files.cfmeet.com/bucket?x=1" });
  const c = directive(csp, "connect-src");
  assert.ok(c.includes("https://msg.cfmeet.com") && c.includes("wss://msg.cfmeet.com"), c);
  assert.ok(c.includes("https://files.cfmeet.com") && !c.includes("/bucket") && !c.includes("wss://files"), c);
  const local = directive(buildCsp({ nonce, dev: false, messagingUrl: "http://127.0.0.1:18090", storageUrl: "http://127.0.0.1:9000" }), "connect-src");
  assert.ok(local.includes("http://127.0.0.1:18090") && local.includes("ws://127.0.0.1:18090") && local.includes("http://127.0.0.1:9000"), local);
  const insecure = directive(buildCsp({ nonce, dev: false, messagingUrl: "http://msg.example.com", storageUrl: "javascript:alert(1)" }), "connect-src");
  assert.ok(!insecure.includes("msg.example.com") && !insecure.includes("javascript"), insecure);
});

test("development adds only what next dev needs; plain HTTP is not upgraded", () => {
  const csp = buildCsp({ nonce: "BBBBBBBBBBBBBBBBBBBBBBBB", dev: true });
  assert.ok(directive(csp, "script-src").includes("'unsafe-eval'"));
  assert.ok(!csp.includes("upgrade-insecure-requests"));
  assert.ok(!buildCsp({ nonce: "BBBBBBBBBBBBBBBBBBBBBBBB", dev: false, secure: false }).includes("upgrade-insecure-requests"));
});

test("a weak nonce and injected sources are refused", () => {
  assert.throws(() => buildCsp({ nonce: "x", dev: false }));
  assert.throws(() => buildCsp({ nonce: "'unsafe-inline' AAAAAAAAAAAAAAAA", dev: false }));
  const csp = buildCsp({ nonce: "CCCCCCCCCCCCCCCCCCCCCCCC", dev: false, extraConnectSrc: "https://rtc.example.com 'unsafe-inline' javascript:alert(1) wss://media.example.com" });
  assert.ok(directive(csp, "connect-src").includes("https://rtc.example.com"));
  assert.ok(directive(csp, "connect-src").includes("wss://media.example.com"));
  assert.ok(!csp.includes("javascript:") && !directive(csp, "connect-src").includes("'unsafe-inline'"));
  assert.ok(SECURITY_HEADERS.some((h) => h.key === "X-Frame-Options" && h.value === "DENY"));
});

test("no DOM XSS sinks in the app source", () => {
  const files = [];
  const walk = (d) => {
    for (const f of readdirSync(d)) {
      const p = join(d, f);
      if (statSync(p).isDirectory()) walk(p);
      else if (/\.(tsx?|jsx?|mjs)$/.test(f)) files.push(p);
    }
  };
  walk(fileURLToPath(new URL("../src", import.meta.url)));
  const sinks = [/dangerouslySetInnerHTML/, /\.innerHTML\s*=/, /\.outerHTML\s*=/, /insertAdjacentHTML/, /document\.write/, /\beval\s*\(/, /new Function\s*\(/];
  const hits = [];
  for (const f of files) {
    const src = readFileSync(f, "utf8");
    for (const re of sinks) if (re.test(src)) hits.push(`${f}: ${re}`);
  }
  assert.deepEqual(hits, [], "render untrusted text through React (escaped), never through an HTML sink");
});
