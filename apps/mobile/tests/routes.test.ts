// Client contract test (MTG-001 / MTG-003): every operation the app calls exists in the served
// Meeting Service contract, and API code builds URLs only through ROUTES.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { execFileSync } from "node:child_process";
import { ROUTES, routeTemplate } from "../lib/api/routes.ts";

const here = dirname(fileURLToPath(import.meta.url));
const appRoot = resolve(here, "..");
const contract = JSON.parse(
  readFileSync(resolve(appRoot, "../../packages/contracts/openapi/meeting-service.json"), "utf8")
);

const operations = new Set<string>();
for (const [path, ops] of Object.entries(contract.paths as Record<string, Record<string, unknown>>)) {
  for (const method of Object.keys(ops)) {
    operations.add(`${method.toUpperCase()} ${path.replace(/\{[^}]+\}/g, "{}")}`);
  }
}

test("every mobile route is an operation of the served contract", () => {
  const missing: string[] = [];
  for (const [name, route] of Object.entries(ROUTES)) {
    const key = `${route.method} ${routeTemplate(route as never)}`;
    if (!operations.has(key)) missing.push(`${name}: ${key}`);
  }
  assert.deepEqual(missing, [], `routes not in the contract:\n${missing.join("\n")}`);
});

test("no mobile route uses a retired legacy meeting path", () => {
  for (const [name, route] of Object.entries(ROUTES)) {
    const t = routeTemplate(route as never);
    assert.ok(!t.startsWith("/api/v1/meetings"), `${name} uses legacy ${t}`);
    assert.ok(!t.startsWith("/api/v1/participants"), `${name} uses legacy ${t}`);
    assert.ok(!t.startsWith("/api/v1/users/me"), `${name} uses v1 profile ${t}`);
  }
});

test("path parameters are percent-encoded", () => {
  assert.equal(ROUTES.join.path("a/b?c"), "/conferences/a%2Fb%3Fc/join");
  assert.equal(ROUTES.removeParticipant.path("c 1", "p#2"), "/conferences/c%201/participants/p%232/remove");
});

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir)) {
    if (entry === "node_modules" || entry === "generated" || entry.startsWith(".")) continue;
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) out.push(...sourceFiles(full));
    else if (/\.(ts|tsx)$/.test(entry) && !entry.endsWith(".d.ts")) out.push(full);
  }
  return out;
}

const appSources = ["app", "components", "features", "hooks", "lib", "providers", "store"].flatMap((d) =>
  sourceFiles(join(appRoot, d))
);

test("API code never writes a raw HTTP path (axios verbs with string literals)", () => {
  const offenders: string[] = [];
  for (const file of appSources) {
    const text = readFileSync(file, "utf8");
    if (/\b(api|apiV2|publicApi|publicApiV2|axios)\.(get|post|put|patch|delete)\s*(<[^>]*>)?\(\s*[`"']/.test(text)) {
      offenders.push(file);
    }
  }
  assert.deepEqual(offenders, []);
});

test("ACC-012: no console logging of requests, tokens or credentials", () => {
  const offenders: string[] = [];
  for (const file of appSources) {
    const lines = readFileSync(file, "utf8").split("\n");
    lines.forEach((line, i) => {
      if (/console\.(log|info|debug|trace)\s*\(/.test(line)) offenders.push(`${file}:${i + 1}`);
      if (/console\.\w+\(.*(token|password|refresh|authorization|credential)/i.test(line)) {
        offenders.push(`${file}:${i + 1}`);
      }
    });
  }
  assert.deepEqual(offenders, []);
});

test("generated contract types are up to date", () => {
  execFileSync(process.execPath, [resolve(appRoot, "scripts/generate-contract-types.mjs"), "--check"], {
    stdio: "pipe",
  });
});
