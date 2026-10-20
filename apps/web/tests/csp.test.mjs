// The CSP builder (src/lib/security/csp.ts) and a static guard against DOM XSS sinks in
// the web app's source.  node --experimental-strip-types --test tests/csp.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { buildCsp, SECURITY_HEADERS } from "../src/lib/security/csp.ts";

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

test("MLS WebAssembly permission is opt-in and never enables JavaScript eval", () => {
  const nonce = "AAAAAAAAAAAAAAAAAAAAAAAA";
  const secure = buildCsp({ nonce, dev: false, allowMessagingWasm: true });
  const scripts = directive(secure, "script-src");
  assert.ok(scripts.includes("'wasm-unsafe-eval'"), scripts);
  assert.ok(!scripts.includes("'unsafe-eval'"), scripts);
  assert.ok(!scripts.includes("'unsafe-inline'"), scripts);
  assert.ok(secure.includes("require-trusted-types-for 'script'"));
  const ordinary = buildCsp({ nonce, dev: false });
  assert.ok(!ordinary.includes("'wasm-unsafe-eval'"), ordinary);
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
  walk(new URL("../src", import.meta.url).pathname);
  const sinks = [/dangerouslySetInnerHTML/, /\.innerHTML\s*=/, /\.outerHTML\s*=/, /insertAdjacentHTML/, /document\.write/, /\beval\s*\(/, /new Function\s*\(/];
  const hits = [];
  for (const f of files) {
    const src = readFileSync(f, "utf8");
    for (const re of sinks) if (re.test(src)) hits.push(`${f}: ${re}`);
  }
  assert.deepEqual(hits, [], "render untrusted text through React (escaped), never through an HTML sink");
});
