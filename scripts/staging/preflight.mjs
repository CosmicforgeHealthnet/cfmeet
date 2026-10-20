#!/usr/bin/env node
// Hosted-staging preflight and smoke check (Part K gates 16 and 30). Read-mostly: it
// changes nothing unless --smoke is given, and then only inside the SANDBOX environment
// the credential belongs to (one two-person conversation and two messages).
//
//   node scripts/staging/preflight.mjs --messaging-url <url> [--service name=url ...] \
//     --credential-file F --principals-file F [--members 20] [--smoke] [--out preflight.json]
//
// Checks: every URL is an allowed staging target (never a production host); each service
// answers /ready; the API credential is accepted and can read the environment policy;
// the principals file lists enough well-formed ids; with --smoke a MANAGED conversation
// can be created, written and read back in order.

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  MessagingClient,
  SafetyError,
  allowedHostsFromEnv,
  assertStagingTarget,
  readSecretFile,
  uuid,
} from "./lib.mjs";

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

function parse(argv) {
  const o = { services: {}, members: 20, smoke: false, out: undefined };
  for (let i = 0; i < argv.length; i++) {
    const k = argv[i];
    const v = () => {
      const x = argv[++i];
      if (x === undefined) throw new Error(`${k} needs a value`);
      return x;
    };
    if (k === "--messaging-url") o.messaging = v();
    else if (k === "--service") {
      const [name, url] = v().split("=", 2);
      if (!name || !url) throw new Error("--service takes name=url");
      o.services[name] = url;
    } else if (k === "--credential-file") o.credential = v();
    else if (k === "--principals-file") o.principals = v();
    else if (k === "--members") o.members = Number(v());
    else if (k === "--smoke") o.smoke = true;
    else if (k === "--out") o.out = v();
    else throw new Error(`unknown option ${k}`);
  }
  if (!o.messaging) throw new Error("--messaging-url is required");
  return o;
}

async function ready(url) {
  try {
    const r = await fetch(`${url.replace(/\/$/, "")}/ready`, { signal: AbortSignal.timeout(10000) });
    return { ok: r.ok, status: r.status };
  } catch (e) {
    return { ok: false, status: 0, error: e.cause?.code ?? e.name };
  }
}

export async function preflight(o) {
  const allowed = allowedHostsFromEnv();
  const checks = [];
  const add = (name, ok, detail = "") => checks.push({ name, ok, detail });
  const targets = { messaging: o.messaging, ...o.services };
  for (const url of Object.values(targets)) {
    assertStagingTarget(url, allowed); // throws SafetyError: nothing is contacted
  }
  for (const [name, url] of Object.entries(targets)) {
    const r = await ready(url);
    add(`${name} /ready`, r.ok, r.error ?? `HTTP ${r.status}`);
  }
  if (o.credential) {
    const m = new MessagingClient(o.messaging, readSecretFile(o.credential, "API credential"));
    const p = await m.call("GET", "/v1/messaging/policy");
    add("credential accepted (GET /v1/messaging/policy)", p.status === 200, `HTTP ${p.status}`);
    if (o.principals) {
      const list = JSON.parse(readFileSync(o.principals, "utf8"));
      const good = Array.isArray(list) && list.every((x) => UUID.test(x)) && new Set(list).size === list.length;
      add(`principals file (${Array.isArray(list) ? list.length : 0} ids, need ${o.members})`, good && list.length >= o.members);
      if (o.smoke && good && list.length >= 2) {
        const [a, b] = list;
        const c = await m.call("POST", "/v1/conversations", {
          acting: a,
          key: `preflight-${uuid()}`,
          body: { contextClass: "GROUP", securityProfile: "MANAGED" },
        });
        add("smoke: create MANAGED group", c.status === 201, `HTTP ${c.status} ${c.json?.error?.code ?? ""}`);
        if (c.status === 201) {
          const id = c.json.conversationId;
          const add2 = await m.call("POST", `/v1/conversations/${id}/members`, { acting: a, body: { principalId: b } });
          add("smoke: add member", add2.status === 201, `HTTP ${add2.status}`);
          const texts = ["preflight one", "preflight two"];
          for (const [i, text] of texts.entries()) {
            const s = await m.call("POST", `/v1/conversations/${id}/messages`, { acting: i ? b : a, body: { clientMessageId: uuid(), text } });
            add(`smoke: send ${i + 1}`, s.status === 201, `HTTP ${s.status}`);
          }
          const h = await m.call("GET", `/v1/conversations/${id}/messages?limit=10`, { acting: b });
          const got = (h.json?.messages ?? []).map((x) => x.managed?.text);
          add("smoke: read back in order", h.status === 200 && JSON.stringify(got) === JSON.stringify(texts), JSON.stringify(got));
        }
      }
    }
  }
  return { kind: "cfmeet.staging.preflight", at: new Date().toISOString(), targets, checks, ok: checks.every((c) => c.ok) };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  (async () => {
    const o = parse(process.argv.slice(2));
    const r = await preflight(o);
    for (const c of r.checks) console.log(`${c.ok ? "ok  " : "FAIL"} ${c.name} ${c.detail}`);
    if (o.out) writeFileSync(o.out, `${JSON.stringify(r, null, 2)}\n`);
    process.exit(r.ok ? 0 : 1);
  })().catch((e) => {
    console.error(e instanceof SafetyError ? `REFUSED: ${e.message}` : `preflight: ${e.message}`);
    process.exit(e instanceof SafetyError ? 3 : 2);
  });
}
