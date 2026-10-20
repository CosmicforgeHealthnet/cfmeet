// Part V remediation M12: the "Could not resolve reference: Failed to fetch" defect,
// checked the way a reader meets it — in Swagger UI, in Chromium.
//
// Every bundled OpenAPI document (packages/contracts/bundled/**) is rendered by the SAME
// Swagger UI the Meeting Service serves (version and SRI digests read from
// services/meeting-service/src/swagger/mod.rs, so the two cannot drift). Requests to the
// schema naming authority (schemas.cfmeet.com) are BLOCKED. Every operation and every
// component schema is resolved; the test fails on any resolver error, any blocked or
// foreign request, or an operation that does not render.
//
//   npm ci && npx playwright install chromium && npm test     (needs network for the CDN)
//
// The negative control (a document with an external $ref) is resolved the same way and
// MUST report the error, so a silent resolver cannot pass the check.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { chromium } from "playwright";

const ROOT = new URL("../../../", import.meta.url).pathname;
const BUNDLED = join(ROOT, "packages/contracts/bundled");
const swaggerRs = readFileSync(join(ROOT, "services/meeting-service/src/swagger/mod.rs"), "utf8");
const constant = (name) => {
  const m = new RegExp(`const ${name}: &str =\\s*"([^"]+)"`).exec(swaggerRs);
  assert.ok(m, `${name} not found in swagger/mod.rs`);
  return m[1];
};
const VERSION = constant("SWAGGER_UI_VERSION");
const CSS_SRI = constant("SWAGGER_UI_CSS_SRI");
const JS_SRI = constant("SWAGGER_UI_JS_SRI");
const CDN = `https://cdn.jsdelivr.net/npm/swagger-ui-dist@${VERSION}`;

const docs = [];
(function walk(dir) {
  for (const f of readdirSync(dir)) {
    const p = join(dir, f);
    if (statSync(p).isDirectory()) walk(p);
    else if (f.endsWith(".openapi.json")) docs.push(relative(BUNDLED, p));
  }
})(BUNDLED);

const page = `<!doctype html><html><head><meta charset="utf-8"><title>CF Meet APIs</title>
<link rel="stylesheet" href="${CDN}/swagger-ui.css" integrity="${CSS_SRI}" crossorigin="anonymous">
</head><body><div id="swagger-ui"></div>
<script src="${CDN}/swagger-ui-bundle.js" integrity="${JS_SRI}" crossorigin="anonymous"></script>
<script>window.ui = SwaggerUIBundle({ dom_id: "#swagger-ui", url: location.hash.slice(1) || "about:blank" });</script>
</body></html>`;

let server, base, browser;
before(async () => {
  server = createServer((req, res) => {
    const path = decodeURIComponent(new URL(req.url, "http://x").pathname);
    if (path === "/") return res.writeHead(200, { "Content-Type": "text/html" }).end(page);
    const rel = path.replace(/^\/docs\//, "");
    if (!docs.includes(rel)) return res.writeHead(404).end();
    res.writeHead(200, { "Content-Type": "application/json" }).end(readFileSync(join(BUNDLED, rel)));
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

test("bundles exist for every served contract", () => {
  assert.ok(docs.length >= 11, `found ${docs.length}`);
  for (const m of swaggerRs.matchAll(/packages\/contracts\/bundled\/([^"]+\.openapi\.json)/g)) {
    assert.ok(docs.includes(m[1]), `the Meeting Service serves ${m[1]}, which is not bundled`);
  }
});

for (const doc of docs) {
  test(`Swagger UI renders ${doc} with every reference resolved and the schema host blocked`, async () => {
    const spec = JSON.parse(readFileSync(join(BUNDLED, doc), "utf8"));
    const ops = [];
    for (const [p, item] of Object.entries(spec.paths ?? {}))
      for (const m of ["get", "put", "post", "delete", "patch", "head", "options"]) if (item[m]) ops.push([p, m]);
    const schemas = Object.keys(spec.components?.schemas ?? {});
    const ctx = await browser.newContext();
    const blocked = [], foreign = [];
    await ctx.route("**/*", (route) => {
      const u = new URL(route.request().url());
      if (u.hostname === "schemas.cfmeet.com") { blocked.push(u.href); return route.abort(); }
      if (u.origin !== base && !u.href.startsWith(CDN)) { foreign.push(u.href); return route.abort(); }
      return route.continue();
    });
    const p = await ctx.newPage();
    const pageErrors = [];
    p.on("pageerror", (e) => pageErrors.push(String(e)));
    await p.goto(`${base}/#${base}/docs/${doc}`);
    await p.waitForFunction(() => window.ui?.specSelectors?.loadingStatus?.() === "success", null, { timeout: 30_000 });
    // Swagger UI's own resolver over the WHOLE document (what expanding every operation
    // and model does, in one pass), plus the errors the UI itself collected.
    const result = await p.evaluate(async (url) => {
      const sys = window.ui;
      const { errors } = await sys.fn.resolveSubtree(sys.specSelectors.specJson().toJS(), [], { baseDoc: url });
      return {
        errors: [...(errors || []), ...sys.errSelectors.allErrors().toJS()].map(
          (e) => `${e.source ?? "resolver"}: ${e.message} @ ${(e.fullPath || e.path || []).join("/")}`,
        ),
        rendered: document.querySelectorAll(".opblock").length,
        text: document.body.innerText.includes("Could not resolve reference"),
      };
    }, `${base}/docs/${doc}`);
    assert.deepEqual(result.errors, [], "resolver errors");
    assert.equal(result.text, false, "the reference-resolution error is shown");
    assert.equal(result.rendered, ops.length, "every operation rendered");
    assert.deepEqual(blocked, [], "requests to the schema host");
    assert.deepEqual(foreign, [], "requests to other hosts");
    assert.deepEqual(pageErrors, []);
    console.log(`EVIDENCE swagger ${doc} operations=${ops.length} schemas=${schemas.length} resolver_errors=0`);
    await ctx.close();
  });
}

test("negative control: an external reference is reported (the check can fail)", async () => {
  const ctx = await browser.newContext();
  await ctx.route("**/schemas.cfmeet.com/**", (r) => r.abort());
  const p = await ctx.newPage();
  await p.goto(`${base}/`);
  await p.waitForFunction(() => typeof window.ui?.fn?.resolveSubtree === "function");
  const errors = await p.evaluate(async () => {
    const spec = { openapi: "3.1.0", info: { title: "x", version: "1" }, paths: { "/x": { get: { responses: { 200: {
      description: "ok", content: { "application/json": { schema: { $ref: "https://schemas.cfmeet.com/platform/common/v1.schema.json#/$defs/Uuid" } } } } } } } } };
    const { errors } = await window.ui.fn.resolveSubtree(spec, [], { baseDoc: location.href });
    return (errors || []).map((e) => e.message);
  });
  assert.ok(errors.some((m) => /Could not resolve reference/i.test(m)), JSON.stringify(errors));
  await ctx.close();
});
