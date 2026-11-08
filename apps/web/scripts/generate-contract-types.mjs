#!/usr/bin/env node
// Contract parity (Blueprint v1.0 Section 17, FE-015): TypeScript types generated from the
// canonical served contract `packages/contracts/openapi/meeting-service.json`.
//
//   node scripts/generate-contract-types.mjs           write src/types/generated/meeting-service.ts
//   node scripts/generate-contract-types.mjs --check   exit 1 when the committed file is stale
//
// Dependency-free on purpose: the contract uses a small OpenAPI 3.0 subset ($ref, allOf,
// enum, nullable, arrays, objects, additionalProperties), so a ~150-line generator is more
// robust here than a third-party tool and needs no package-lock change. Unsupported schema
// keywords fail the run loudly instead of producing a wrong type.
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
export const CONTRACT_PATH = join(here, "..", "..", "..", "packages", "contracts", "openapi", "meeting-service.json");
export const OUTPUT_PATH = join(here, "..", "src", "types", "generated", "meeting-service.ts");

const SUPPORTED = new Set([
  "type", "format", "description", "nullable", "required", "properties", "items", "enum",
  "allOf", "oneOf", "anyOf", "$ref", "additionalProperties", "example", "minimum", "maximum",
  "minLength", "maxLength", "pattern", "default", "deprecated", "readOnly", "writeOnly",
  "title", "minItems", "maxItems", "uniqueItems", "exclusiveMinimum", "exclusiveMaximum", "multipleOf",
]);

const IDENT = /^[A-Za-z_$][A-Za-z0-9_$]*$/;
const key = (name) => (IDENT.test(name) ? name : JSON.stringify(name));

function comment(text, indent) {
  if (!text) return "";
  const lines = String(text).replace(/\*\//g, "*\\/").split(/\r?\n/).map((l) => l.trimEnd());
  if (lines.length === 1) return `${indent}/** ${lines[0]} */\n`;
  return `${indent}/**\n${lines.map((l) => `${indent} *${l ? ` ${l}` : ""}`).join("\n")}\n${indent} */\n`;
}

function refName(ref, where) {
  const m = /^#\/components\/schemas\/([A-Za-z0-9_.-]+)$/.exec(ref);
  if (!m) throw new Error(`${where}: unsupported $ref ${ref}`);
  return m[1].replace(/[^A-Za-z0-9_]/g, "_");
}

/** TypeScript type expression of an OpenAPI 3.0 schema. */
export function typeOf(schema, where, indent = "") {
  if (schema === true || schema === undefined || schema === null) return "unknown";
  if (typeof schema !== "object") throw new Error(`${where}: schema must be an object`);
  for (const k of Object.keys(schema)) {
    if (!SUPPORTED.has(k) && !k.startsWith("x-")) throw new Error(`${where}: unsupported keyword ${k}`);
  }
  const nullable = (t) => (schema.nullable ? `${t} | null` : t);
  if (schema.$ref) return nullable(refName(schema.$ref, where));
  if (schema.allOf) {
    const parts = schema.allOf.map((s, i) => typeOf(s, `${where}.allOf[${i}]`, indent));
    return nullable(parts.length === 1 ? parts[0] : `(${parts.join(" & ")})`);
  }
  for (const union of ["oneOf", "anyOf"]) {
    if (schema[union]) {
      const parts = schema[union].map((s, i) => typeOf(s, `${where}.${union}[${i}]`, indent));
      return nullable(`(${parts.join(" | ")})`);
    }
  }
  if (schema.enum) return nullable(schema.enum.map((v) => JSON.stringify(v)).join(" | "));
  switch (schema.type) {
    case "string":
      return nullable("string");
    case "integer":
    case "number":
      return nullable("number");
    case "boolean":
      return nullable("boolean");
    case "array":
      return nullable(`Array<${typeOf(schema.items, `${where}.items`, indent)}>`);
    case "object":
    case undefined: {
      const props = schema.properties ?? {};
      const extra = schema.additionalProperties;
      const names = Object.keys(props);
      if (names.length === 0 && schema.type === undefined && extra === undefined) return nullable("unknown");
      let record = null;
      if (extra === true || (extra && typeof extra === "object" && Object.keys(extra).length === 0)) {
        record = "Record<string, unknown>";
      } else if (extra && typeof extra === "object") {
        record = `Record<string, ${typeOf(extra, `${where}.additionalProperties`, indent)}>`;
      }
      if (names.length === 0) return nullable(record ?? "Record<string, unknown>");
      const required = new Set(schema.required ?? []);
      const inner = indent + "  ";
      const body = names
        .map((n) => {
          const p = props[n];
          return `${comment(p && p.description, inner)}${inner}${key(n)}${required.has(n) ? "" : "?"}: ${typeOf(p, `${where}.${n}`, inner)};`;
        })
        .join("\n");
      const object = `{\n${body}\n${indent}}`;
      return nullable(record ? `(${object} & ${record})` : object);
    }
    default:
      throw new Error(`${where}: unsupported type ${schema.type}`);
  }
}

/** The generated module for a parsed contract document. */
export function generate(contract) {
  const schemas = contract.components?.schemas ?? {};
  const out = [
    "// GENERATED FILE — do not edit by hand.",
    "// Source: packages/contracts/openapi/meeting-service.json",
    "// Regenerate: npm run contracts:generate (apps/web). CI fails when this file is stale.",
    "/* eslint-disable */",
    "",
    `export const CONTRACT_TITLE = ${JSON.stringify(contract.info?.title ?? "")};`,
    `export const CONTRACT_VERSION = ${JSON.stringify(contract.info?.version ?? "")};`,
    "",
  ];
  for (const name of Object.keys(schemas).sort()) {
    const schema = schemas[name];
    const id = name.replace(/[^A-Za-z0-9_]/g, "_");
    const t = typeOf(schema, name);
    out.push(comment(schema.description, "").trimEnd());
    if (t.startsWith("{") && !schema.nullable) out.push(`export interface ${id} ${t}`);
    else out.push(`export type ${id} = ${t};`);
    out.push("");
  }
  out.push("/** Every served operation: `METHOD /path` → operationId. */");
  out.push("export const CONTRACT_OPERATIONS = {");
  const methods = ["get", "put", "post", "delete", "options", "head", "patch", "trace"];
  const ops = [];
  for (const [path, item] of Object.entries(contract.paths ?? {})) {
    for (const m of methods) {
      if (item[m]) ops.push([`${m.toUpperCase()} ${path}`, item[m].operationId ?? "", !!item[m].deprecated]);
    }
  }
  ops.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
  for (const [k, id] of ops) out.push(`  ${JSON.stringify(k)}: ${JSON.stringify(id)},`);
  out.push("} as const;");
  out.push("");
  out.push("export type ContractOperation = keyof typeof CONTRACT_OPERATIONS;");
  out.push("");
  out.push("/** Operations the contract marks `deprecated: true`. */");
  out.push("export const DEPRECATED_OPERATIONS: readonly ContractOperation[] = [");
  for (const [k, , deprecated] of ops) if (deprecated) out.push(`  ${JSON.stringify(k)},`);
  out.push("];");
  out.push("");
  return out.join("\n");
}

function main() {
  const contract = JSON.parse(readFileSync(CONTRACT_PATH, "utf8"));
  const text = generate(contract);
  if (process.argv.includes("--check")) {
    let current = "";
    try {
      current = readFileSync(OUTPUT_PATH, "utf8").replace(/\r\n/g, "\n");
    } catch {
      current = "";
    }
    if (current !== text) {
      console.error(
        "src/types/generated/meeting-service.ts is stale relative to packages/contracts/openapi/meeting-service.json.\n" +
          "Run `npm run contracts:generate` in apps/web and commit the result.",
      );
      process.exit(1);
    }
    console.log(`contract types are fresh (${contract.info?.title} ${contract.info?.version})`);
    return;
  }
  mkdirSync(dirname(OUTPUT_PATH), { recursive: true });
  writeFileSync(OUTPUT_PATH, text);
  console.log(`wrote ${OUTPUT_PATH}`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) main();
