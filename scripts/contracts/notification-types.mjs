#!/usr/bin/env node
// FE1 (Notification Service roadmap, step after N1): TypeScript types generated from the
// frozen Notification Service contracts, for the web and mobile apps.
//
//   node scripts/contracts/notification-types.mjs           write both generated files
//   node scripts/contracts/notification-types.mjs --check   exit 1 when either is stale
//
// Sources: packages/contracts/notifications/common/v1.schema.json ($defs) and the U1–U9
// operations of packages/contracts/notifications/http/v1.openapi.json (request / response
// shapes named after their operationId). Outputs:
//   apps/web/src/types/generated/notifications.ts
//   apps/mobile/lib/api/generated/notifications.ts
// Types only: no runtime code, no UI. The U1–U9 routes are not served until N8; until then
// nothing imports these types. Dependency-free; unsupported schema keywords fail loudly.
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
const COMMON_REL = "packages/contracts/notifications/common/v1.schema.json";
const HTTP_REL = "packages/contracts/notifications/http/v1.openapi.json";
const COMMON_ID = "https://schemas.cfmeet.com/notifications/common/v1.schema.json";
export const OUTPUTS = ["apps/web/src/types/generated/notifications.ts", "apps/mobile/lib/api/generated/notifications.ts"];

// Validation-only / annotation keywords: they narrow values but do not change the type.
const IGNORED = new Set([
  "description", "format", "pattern", "minLength", "maxLength", "minimum", "maximum", "minItems",
  "maxItems", "uniqueItems", "minProperties", "maxProperties", "default", "title", "examples",
]);
const STRUCTURAL = new Set(["type", "enum", "const", "$ref", "anyOf", "oneOf", "allOf", "properties",
  "required", "additionalProperties", "propertyNames", "items", "not"]);

// User-facing definitions only: the envelope vocabulary (producers, routes, sealed parts,
// lanes, ingest outcomes) is internal to services and never reaches a client.
const FRONTEND_DEFS = new Set([
  "Uuid", "Category", "PreferenceCategory", "Channel", "ActionKind", "NotificationItem",
  "NotificationLevel", "ChannelSwitches", "QuietHours", "ConversationPreference", "ContactPreference",
  "NotificationPreferences", "NotificationPreferencesUpdate", "PushPlatform", "ErrorCode", "Error",
]);

const IDENT = /^[A-Za-z_$][A-Za-z0-9_$]*$/;
const key = (n) => (IDENT.test(n) ? n : JSON.stringify(n));
const pascal = (s) => s.replace(/(^|[^A-Za-z0-9])([a-z])/g, (_, __, c) => c.toUpperCase()).replace(/[^A-Za-z0-9]/g, "");

function refName(ref, where) {
  const m = /^(?:#|https:\/\/schemas\.cfmeet\.com\/notifications\/common\/v1\.schema\.json#)\/\$defs\/([A-Za-z0-9_]+)$/.exec(ref);
  if (!m) throw new Error(`${where}: unsupported $ref ${ref}`);
  return m[1];
}

function typeOf(s, where, indent) {
  if (s === true || s === undefined) return "unknown";
  if (typeof s !== "object" || s === null) throw new Error(`${where}: schema must be an object`);
  for (const k of Object.keys(s)) {
    if (!IGNORED.has(k) && !STRUCTURAL.has(k) && !k.startsWith("x-")) throw new Error(`${where}: unsupported keyword ${k}`);
  }
  if (s.$ref) return refName(s.$ref, where);
  if (s.const !== undefined) return JSON.stringify(s.const);
  if (s.enum) return s.enum.map((v) => JSON.stringify(v)).join(" | ");
  for (const u of ["anyOf", "oneOf"]) {
    // A oneOf that only constrains which properties are present (no own types) refines an
    // object type declared alongside it: the object type is used.
    if (s[u] && !(s.properties && s[u].every((b) => !b.type && !b.$ref && !b.properties))) {
      return `(${s[u].map((b, i) => typeOf(b, `${where}.${u}[${i}]`, indent)).join(" | ")})`;
    }
  }
  if (s.allOf) return `(${s.allOf.map((b, i) => typeOf(b, `${where}.allOf[${i}]`, indent)).join(" & ")})`;
  const types = Array.isArray(s.type) ? s.type : [s.type];
  return types.map((t) => {
    switch (t) {
      case "string": return "string";
      case "integer": case "number": return "number";
      case "boolean": return "boolean";
      case "null": return "null";
      case "array": return `Array<${typeOf(s.items, `${where}.items`, indent)}>`;
      case "object": case undefined: return objectType(s, where, indent);
      default: throw new Error(`${where}: unsupported type ${t}`);
    }
  }).join(" | ");
}

function objectType(s, where, indent) {
  const props = s.properties ?? {};
  const required = new Set(s.required ?? []);
  const names = Object.keys(props);
  const extra = s.additionalProperties;
  if (names.length === 0 && extra && typeof extra === "object") {
    const value = typeOf(extra, `${where}.additionalProperties`, indent);
    const keys = s.propertyNames?.enum;
    return keys ? `Partial<Record<${keys.map((k) => JSON.stringify(k)).join(" | ")}, ${value}>>` : `Record<string, ${value}>`;
  }
  if (names.length === 0) return extra === false ? "Record<string, never>" : "Record<string, unknown>";
  const inner = indent + "  ";
  const lines = names.map((n) => {
    const p = props[n];
    const doc = p && p.description ? `${inner}/** ${String(p.description).replace(/\*\//g, "*\\/").split("\n")[0]} */\n` : "";
    return `${doc}${inner}${key(n)}${required.has(n) ? "" : "?"}: ${typeOf(p, `${where}.${n}`, inner)};`;
  });
  return `{\n${lines.join("\n")}\n${indent}}`;
}

export function generate() {
  const common = JSON.parse(readFileSync(join(root, COMMON_REL), "utf8"));
  const http = JSON.parse(readFileSync(join(root, HTTP_REL), "utf8"));
  if (common.$id !== COMMON_ID) throw new Error("unexpected common schema $id");
  const out = [
    "// GENERATED by scripts/contracts/notification-types.mjs from",
    `// ${COMMON_REL} and ${HTTP_REL}.`,
    "// Do not edit. Notification Service contracts are frozen by roadmap N1 (experimental):",
    "// the U1-U9 routes are not served until N8, so nothing imports these types yet.",
    "/* eslint-disable */",
    "",
  ];
  for (const [name, s] of Object.entries(common.$defs)) {
    if (!FRONTEND_DEFS.has(name)) continue;
    if (s.description) out.push(`/** ${String(s.description).replace(/\*\//g, "*\\/").split("\n")[0]} */`);
    out.push(`export type ${name} = ${typeOf(s, name, "")};`, "");
  }
  for (const [path, item] of Object.entries(http.paths)) {
    if (!path.startsWith("/api/v2/notifications")) continue;
    for (const [method, op] of Object.entries(item)) {
      const base = pascal(op.operationId);
      const req = op.requestBody?.content?.["application/json"]?.schema;
      if (req) out.push(`/** ${method.toUpperCase()} ${path} request body. */`, `export type ${base}Request = ${typeOf(req, `${base}Request`, "")};`, "");
      const res = op.responses?.["200"]?.content?.["application/json"]?.schema;
      if (res) out.push(`/** ${method.toUpperCase()} ${path} 200 response. */`, `export type ${base}Response = ${typeOf(res, `${base}Response`, "")};`, "");
    }
  }
  return out.join("\n");
}

const isMain = process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1];
if (isMain) {
  const text = generate();
  const check = process.argv.includes("--check");
  let stale = false;
  for (const rel of OUTPUTS) {
    const path = join(root, rel);
    let current = null;
    try { current = readFileSync(path, "utf8"); } catch { /* missing */ }
    if (check) {
      if (current !== text) { console.error(`stale: ${rel} (run node scripts/contracts/notification-types.mjs)`); stale = true; }
    } else {
      mkdirSync(dirname(path), { recursive: true });
      writeFileSync(path, text);
      console.log(`wrote ${rel}`);
    }
  }
  if (stale) process.exit(1);
  if (check) console.log("notification contract types are fresh");
}
