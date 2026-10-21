#!/usr/bin/env node
// Hosted-staging capacity runner for Unified Messaging (Part K gate 16).
//
// Drives a DEPLOYED Messaging service over its public API exactly as an external
// application does (API credential + acting principal, device-bound client tokens,
// single-use realtime tickets, presigned object-storage uploads) and writes one JSON
// report. It asserts CORRECTNESS only (every message accepted, delivered exactly once and
// in order, every attachment byte-identical); it asserts NO performance threshold of its
// own. Owner-approved thresholds can be supplied with --thresholds (see report.mjs).
//
//   node scripts/staging/capacity-runner.mjs \
//     --messaging-url https://<staging messaging host> \
//     --credential-file <file with the SANDBOX API credential, mode 600> \
//     --principals-file <JSON array of principal ids of that environment> \
//     --authorization <owner approval reference> --out report.json
//
// Prerequisites (owner): a dedicated SANDBOX environment of a test application, a
// credential with the scopes below, enough principals for --members, and the staging
// host in CFMEET_STAGING_ALLOWED_HOSTS. Never a production credential or host.
//
// Side effects on the target (all inside that SANDBOX environment): it registers one
// device per member (revoked at the end), creates conversations, messages and
// attachments (left for the environment's retention), and temporarily widens the
// environment policy (GROUP MANAGED, attachments on); the previous policy is restored.

import { readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import os from "node:os";
import {
  MessagingClient,
  SafetyError,
  allowedHostsFromEnv,
  assertStagingTarget,
  b64,
  noisePng,
  openRealtime,
  parseArgs,
  percentiles,
  pool,
  readSecretFile,
  sha256hex,
  sleep,
  uuid,
} from "./lib.mjs";

export const REQUIRED_SCOPES = [
  "messages.send",
  "messages.read",
  "conversations.create",
  "conversations.manage",
  "attachments.write",
  "attachments.read",
];
export const WORKLOADS = ["managed-send", "history-read", "fanout", "reconnect-storm", "search", "attachments"];

const spec = {
  defaults: {
    "messaging-url": undefined,
    "credential-file": undefined,
    "principals-file": undefined,
    authorization: undefined,
    out: "capacity-report.json",
    label: "",
    workloads: WORKLOADS.join(","),
    members: "20",
    "messages-per-sender": "20",
    "fanout-messages": "30",
    "fanout-interval-ms": "100",
    searches: "20",
    attachments: "10",
    "attachment-bytes": "262144",
    concurrency: "16",
    "delivery-timeout-ms": "30000",
    "crypto-dir": undefined,
  },
  flags: ["help"],
};

const here = dirname(fileURLToPath(import.meta.url));

/** Loads the pinned browser crypto core (verified against its manifest) to make devices. */
async function loadCrypto(dir) {
  const manifest = JSON.parse(readFileSync(join(dir, "manifest.json"), "utf8"));
  const pick = (role) => Object.entries(manifest.files).find(([, f]) => f.role === role);
  const verified = (role) => {
    const [name, f] = pick(role);
    const bytes = readFileSync(join(dir, name));
    const got = `sha384-${createHash("sha384").update(bytes).digest("base64")}`;
    if (got !== f.integrity) throw new Error(`${name} does not match its pinned integrity`);
    return { name, bytes };
  };
  const wasm = verified("wasm");
  const glue = verified("glue");
  const mod = await import(pathToFileURL(join(dir, glue.name)).href);
  mod.initSync({ module: wasm.bytes });
  return mod;
}

class Run {
  constructor(opts, client, crypto) {
    this.o = opts;
    this.m = client;
    this.crypto = crypto;
    this.devices = new Map(); // principal → deviceId
    this.report = { workloads: {}, errors: [] };
  }

  async device(principal) {
    if (this.devices.has(principal)) return this.devices.get(principal);
    const ch = (await this.m.expect(201, "POST", "/v1/messaging/devices/registration-challenges", { acting: principal, body: {} })).json;
    const dev = new this.crypto.WebDevice(ch.deviceId);
    const kp = dev.registrationKeyPackage(ch.challengeId, Buffer.from(ch.nonce, "hex"));
    await this.m.expect(201, "POST", "/v1/messaging/devices", {
      acting: principal,
      body: { challengeId: ch.challengeId, keyPackage: b64(kp) },
    });
    dev.free?.();
    this.devices.set(principal, ch.deviceId);
    return ch.deviceId;
  }

  async ticket(principal) {
    const deviceId = await this.device(principal);
    const ct = (await this.m.expect(201, "POST", "/v1/messaging/client-tokens", { acting: principal, body: { deviceId } })).json;
    const tk = await this.m.expect(201, "POST", "/v1/realtime/tickets", { auth: `CF-Client-Token ${ct.token}` });
    return tk.json.ticket;
  }

  async group(members) {
    const c = (await this.m.expect(201, "POST", "/v1/conversations", {
      acting: members[0],
      key: `capacity-${uuid()}`,
      body: { contextClass: "GROUP", securityProfile: "MANAGED" },
    })).json;
    await pool(members.slice(1), this.o.concurrency, (p) =>
      this.m.expect(201, "POST", `/v1/conversations/${c.conversationId}/members`, { acting: members[0], body: { principalId: p } }),
    );
    return c.conversationId;
  }

  async send(conv, from, text, extra = {}) {
    const clientMessageId = uuid();
    const r = await this.m.call("POST", `/v1/conversations/${conv}/messages`, {
      acting: from,
      body: { clientMessageId, text, ...extra },
    });
    return { ...r, clientMessageId };
  }

  /** Every message of `conv` (paged), as read by `reader`, with each page's latency. */
  async history(conv, reader) {
    const out = [];
    const lat = [];
    let after = 0;
    for (;;) {
      const r = await this.m.expect(200, "GET", `/v1/conversations/${conv}/messages?afterSequence=${after}&limit=500`, { acting: reader });
      lat.push(r.ms);
      out.push(...(r.json.messages ?? []));
      if (r.json.nextAfterSequence == null) break;
      after = r.json.nextAfterSequence;
    }
    return { messages: out, lat };
  }

  // ---- workloads -------------------------------------------------------------------

  async managedSend(members) {
    const conv = await this.group(members);
    const per = this.o.messagesPerSender;
    const lat = [];
    const status = {};
    const sent = new Set();
    const started = performance.now();
    await Promise.all(
      members.map(async (p) => {
        for (let i = 0; i < per; i++) {
          const r = await this.send(conv, p, `capacity ${i} from ${p}`);
          status[r.status] = (status[r.status] ?? 0) + 1;
          if (r.status === 201) {
            lat.push(r.ms);
            sent.add(r.clientMessageId);
          }
        }
      }),
    );
    const elapsed = (performance.now() - started) / 1000;
    const { messages, lat: readLat } = await this.history(conv, members[0]);
    const seen = new Map();
    let ordered = true;
    for (let i = 0; i < messages.length; i++) {
      seen.set(messages[i].clientMessageId, (seen.get(messages[i].clientMessageId) ?? 0) + 1);
      if (i > 0 && !(messages[i].sequence > messages[i - 1].sequence)) ordered = false;
    }
    const missing = [...sent].filter((id) => !seen.has(id)).length;
    const duplicated = [...seen.values()].filter((n) => n > 1).length;
    this.lastConversation = { conv, members };
    return {
      senders: members.length,
      attempted: members.length * per,
      accepted: sent.size,
      statuses: status,
      elapsed_s: Math.round(elapsed * 1000) / 1000,
      accepted_per_s: Math.round((sent.size / elapsed) * 100) / 100,
      send_latency: percentiles(lat),
      verification: { stored: messages.length, missing, duplicated, strictly_ordered: ordered },
      correct: missing === 0 && duplicated === 0 && ordered && sent.size === members.length * per,
      _readLat: readLat,
    };
  }

  async historyRead(members) {
    if (!this.lastConversation) throw new Error("history-read needs managed-send first");
    const { conv } = this.lastConversation;
    const lat = [];
    let pages = 0;
    await pool(members, this.o.concurrency, async (p) => {
      const h = await this.history(conv, p);
      lat.push(...h.lat);
      pages += h.lat.length;
    });
    return { readers: members.length, pages, page_size: 500, page_latency: percentiles(lat), correct: true };
  }

  async connect(principal, conv, onEvent) {
    const ticket = await this.ticket(principal);
    let resolveSub;
    const subscribed = new Promise((r) => (resolveSub = r));
    const started = performance.now();
    const ws = await openRealtime(this.o.messagingUrl, ticket, (f) => {
      if (f.type === "subscribed" || (f.type === "error" && f.requestId === "s1")) resolveSub(f);
      else if (f.type === "event") onEvent(f.event, performance.now());
    });
    ws.send(JSON.stringify({ type: "subscribe", conversationId: conv, requestId: "s1" }));
    const sub = await Promise.race([subscribed, sleep(15000).then(() => ({ type: "timeout" }))]);
    if (sub.type !== "subscribed") throw new Error(`subscribe failed: ${sub.type} ${sub.code ?? ""}`);
    return { ws, ms: performance.now() - started };
  }

  async fanout(members) {
    const conv = await this.group(members);
    const [sender, ...receivers] = members;
    const received = new Map(receivers.map((r) => [r, []]));
    const sockets = await pool(receivers, this.o.concurrency, (r) =>
      this.connect(r, conv, (ev, at) => {
        if (ev?.type === "message.created") received.get(r).push({ ev: JSON.stringify(ev), at });
      }),
    );
    const sent = [];
    for (let i = 0; i < this.o.fanoutMessages; i++) {
      const t0 = performance.now();
      const r = await this.send(conv, sender, `fanout ${i}`);
      if (r.status !== 201) throw new Error(`fanout send ${i} → ${r.status}`);
      sent.push({ id: r.json.messageId, t0 });
      await sleep(this.o.fanoutIntervalMs);
    }
    const deadline = performance.now() + this.o.deliveryTimeoutMs;
    const complete = () => [...received.values()].every((l) => l.length >= sent.length);
    while (!complete() && performance.now() < deadline) await sleep(100);
    const lat = [];
    let missing = 0;
    let duplicated = 0;
    let outOfOrder = 0;
    for (const list of received.values()) {
      let last = -1;
      for (let i = 0; i < sent.length; i++) {
        const hits = list.filter((x) => x.ev.includes(sent[i].id));
        if (hits.length === 0) {
          missing++;
          continue;
        }
        if (hits.length > 1) duplicated++;
        const idx = list.indexOf(hits[0]);
        if (idx < last) outOfOrder++;
        last = idx;
        lat.push(hits[0].at - sent[i].t0);
      }
    }
    this.fanoutState = { conv, sender, receivers, sockets };
    return {
      receivers: receivers.length,
      messages: sent.length,
      deliveries_expected: receivers.length * sent.length,
      deliveries: lat.length,
      connect_and_subscribe: percentiles(sockets.map((s) => s.ms)),
      send_to_receive_latency: percentiles(lat),
      verification: { missing, duplicated, out_of_order: outOfOrder },
      correct: missing === 0 && duplicated === 0 && outOfOrder === 0,
    };
  }

  async reconnectStorm() {
    if (!this.fanoutState) throw new Error("reconnect-storm needs fanout first");
    const { conv, sender, receivers, sockets } = this.fanoutState;
    for (const s of sockets) s.ws.close();
    await sleep(500);
    const got = new Map(receivers.map((r) => [r, 0]));
    let marker;
    const started = performance.now();
    // Every receiver reconnects at once (new client token, new ticket, new socket).
    const again = await Promise.all(
      receivers.map((r) =>
        this.connect(r, conv, (ev) => {
          if (ev?.type === "message.created" && marker && JSON.stringify(ev).includes(marker)) got.set(r, got.get(r) + 1);
        }),
      ),
    );
    const allSubscribed = performance.now() - started;
    const r = await this.send(conv, sender, "after the reconnection storm");
    if (r.status !== 201) throw new Error(`post-storm send → ${r.status}`);
    marker = r.json.messageId;
    const deadline = performance.now() + this.o.deliveryTimeoutMs;
    while ([...got.values()].some((n) => n < 1) && performance.now() < deadline) await sleep(100);
    for (const s of again) s.ws.close();
    const missing = [...got.values()].filter((n) => n === 0).length;
    const duplicated = [...got.values()].filter((n) => n > 1).length;
    return {
      clients: receivers.length,
      all_resubscribed_ms: Math.round(allSubscribed),
      per_client_reconnect: percentiles(again.map((s) => s.ms)),
      verification: { missing, duplicated },
      correct: missing === 0 && duplicated === 0,
    };
  }

  async search(members) {
    if (!this.lastConversation) throw new Error("search needs managed-send first");
    const { conv } = this.lastConversation;
    const needle = `needle${uuid().replaceAll("-", "").slice(0, 12)}`;
    const r = await this.send(conv, members[0], `capacity search ${needle}`);
    if (r.status !== 201) throw new Error(`search seed → ${r.status}`);
    const lat = [];
    let found = 0;
    const statuses = {};
    // Spread over members: search is rate limited per principal.
    const queries = Array.from({ length: this.o.searches }, (_, i) => members[i % members.length]);
    await pool(queries, this.o.concurrency, async (p) => {
      const q = await this.m.call("GET", `/v1/conversations/${conv}/search?q=${needle}&limit=20`, { acting: p });
      statuses[q.status] = (statuses[q.status] ?? 0) + 1;
      if (q.status === 200) {
        lat.push(q.ms);
        if (JSON.stringify(q.json).includes(r.json.messageId)) found++;
      }
    });
    return { queries: queries.length, statuses, latency: percentiles(lat), found, correct: found === queries.length };
  }

  async attachments(members) {
    if (!this.lastConversation) throw new Error("attachments needs managed-send first");
    const { conv } = this.lastConversation;
    const stage = { intent: [], upload: [], finalize: [], ready: [], download: [] };
    let identical = 0;
    const failures = {};
    const items = Array.from({ length: this.o.attachments }, (_, i) => members[i % members.length]);
    const reader = members[1] ?? members[0];
    await pool(items, this.o.concurrency, async (p) => {
      try {
        const png = noisePng(this.o.attachmentBytes);
        const sha = sha256hex(png);
        const intent = await this.m.expect(201, "POST", `/v1/conversations/${conv}/attachments`, {
          acting: p,
          body: { kind: "IMAGE", size: png.length, sha256: sha, mediaType: "image/png", fileName: "capacity.png" },
        });
        stage.intent.push(intent.ms);
        const aid = intent.json.attachmentId;
        const up = intent.json.upload;
        const headers = Object.fromEntries((up.headers ?? []).map((h) => [h.name, h.value]));
        let t = performance.now();
        const put = await fetch(up.url, { method: up.method ?? "PUT", headers, body: png });
        if (!put.ok) throw new Error(`upload ${put.status}`);
        stage.upload.push(performance.now() - t);
        const fin = await this.m.expect(200, "POST", `/v1/conversations/${conv}/attachments/${aid}/finalize`, { acting: p, body: {} });
        stage.finalize.push(fin.ms);
        t = performance.now();
        let state = fin.json.state;
        while (state !== "READY") {
          if (["REJECTED", "BLOCKED", "FAILED", "DELETED"].includes(state)) throw new Error(`attachment ${state}`);
          if (performance.now() - t > this.o.deliveryTimeoutMs) throw new Error(`attachment stayed ${state}`);
          await sleep(250);
          state = (await this.m.expect(200, "GET", `/v1/conversations/${conv}/attachments/${aid}`, { acting: p })).json.state;
        }
        stage.ready.push(performance.now() - t);
        const sent = await this.send(conv, p, "capacity attachment", { attachmentIds: [aid] });
        if (sent.status !== 201) throw new Error(`attach send ${sent.status}`);
        const grant = await this.m.expect(200, "POST", `/v1/conversations/${conv}/attachments/${aid}/download-grants`, { acting: reader, body: {} });
        t = performance.now();
        const got = Buffer.from(await (await fetch(grant.json.url)).arrayBuffer());
        stage.download.push(performance.now() - t);
        if (sha256hex(got) === sha) identical++;
      } catch (e) {
        failures[e.message] = (failures[e.message] ?? 0) + 1;
      }
    });
    return {
      attachments: items.length,
      bytes_each: this.o.attachmentBytes,
      byte_identical: identical,
      failures,
      intent: percentiles(stage.intent),
      upload: percentiles(stage.upload),
      finalize: percentiles(stage.finalize),
      finalize_to_ready: percentiles(stage.ready),
      download: percentiles(stage.download),
      correct: identical === items.length,
    };
  }

  async cleanup() {
    for (const [principal, deviceId] of this.devices) {
      const r = await this.m.call("POST", `/v1/messaging/devices/${deviceId}/revoke`, { acting: principal, body: { reason: "USER_REQUESTED" } });
      if (r.status >= 300) this.report.errors.push(`device revoke ${deviceId} → ${r.status}`);
    }
  }
}

export async function main(argv) {
  const a = parseArgs(argv, spec);
  if (a.help) {
    console.log(readFileSync(fileURLToPath(import.meta.url), "utf8").split("\n").filter((l) => l.startsWith("//")).join("\n"));
    return 0;
  }
  for (const k of ["messaging-url", "credential-file", "principals-file"]) if (!a[k]) throw new Error(`--${k} is required`);
  const target = assertStagingTarget(a["messaging-url"], allowedHostsFromEnv());
  const loopback = ["localhost", "127.0.0.1", "::1", "[::1]"].includes(target.hostname);
  if (!loopback && !a.authorization) throw new SafetyError("--authorization <owner approval reference> is required for a hosted target");
  const n = (k) => {
    const v = Number(a[k]);
    if (!Number.isInteger(v) || v < 1) throw new Error(`--${k} must be a positive integer`);
    return v;
  };
  const o = {
    messagingUrl: target.href.replace(/\/$/, ""),
    members: n("members"),
    messagesPerSender: n("messages-per-sender"),
    fanoutMessages: n("fanout-messages"),
    fanoutIntervalMs: Number(a["fanout-interval-ms"]),
    searches: n("searches"),
    attachments: n("attachments"),
    attachmentBytes: n("attachment-bytes"),
    concurrency: n("concurrency"),
    deliveryTimeoutMs: n("delivery-timeout-ms"),
  };
  const workloads = a.workloads.split(",").map((s) => s.trim()).filter(Boolean);
  for (const w of workloads) if (!WORKLOADS.includes(w)) throw new Error(`unknown workload ${w}`);
  const principals = JSON.parse(readFileSync(a["principals-file"], "utf8"));
  if (!Array.isArray(principals) || principals.length < o.members) {
    throw new Error(`--principals-file must list at least --members (${o.members}) principal ids`);
  }
  const members = principals.slice(0, o.members);
  const credential = readSecretFile(a["credential-file"], "API credential");
  const crypto = await loadCrypto(a["crypto-dir"] ?? join(here, "../../apps/web/public/messaging-crypto"));
  const client = new MessagingClient(o.messagingUrl, credential);
  const run = new Run(o, client, crypto);

  const startedAt = new Date().toISOString();
  const policy = (await client.expect(200, "GET", "/v1/messaging/policy")).json;
  let restored = false;
  const widen = structuredClone(policy.policy ?? {});
  widen.contexts = { ...(widen.contexts ?? {}) };
  const group = widen.contexts.GROUP ?? {};
  const allowed = new Set([...(group.allowedProfiles ?? []), "MANAGED"]);
  widen.contexts.GROUP = { ...group, allowedProfiles: [...allowed], defaultProfile: group.defaultProfile ?? "MANAGED" };
  widen.attachments = { ...(widen.attachments ?? {}), enabled: true };
  const put = (await client.expect(200, "PUT", "/v1/messaging/policy", { body: { expectedVersion: policy.version, policy: widen } })).json;

  const runners = {
    "managed-send": () => run.managedSend(members),
    "history-read": () => run.historyRead(members),
    fanout: () => run.fanout(members),
    "reconnect-storm": () => run.reconnectStorm(),
    search: () => run.search(members),
    attachments: () => run.attachments(members),
  };
  try {
    for (const w of WORKLOADS.filter((x) => workloads.includes(x))) {
      process.stderr.write(`capacity: ${w}...\n`);
      try {
        const r = await runners[w]();
        delete r._readLat;
        run.report.workloads[w] = r;
      } catch (e) {
        run.report.workloads[w] = { correct: false, error: e.message };
      }
    }
  } finally {
    for (const s of run.fanoutState?.sockets ?? []) s.ws.close();
    await run.cleanup().catch((e) => run.report.errors.push(`cleanup: ${e.message}`));
    const r = await client.call("PUT", "/v1/messaging/policy", { body: { expectedVersion: put.version, policy: policy.policy ?? {} } });
    restored = r.status === 200;
    if (!restored) run.report.errors.push(`policy restore → ${r.status}`);
  }

  const report = {
    kind: "cfmeet.staging.capacity",
    version: 1,
    label: a.label,
    authorization: a.authorization ?? null,
    target: { messaging: o.messagingUrl, loopback },
    startedAt,
    finishedAt: new Date().toISOString(),
    runner: { node: process.version, platform: `${os.platform()} ${os.release()}`, cpus: os.cpus().length, host: os.hostname() },
    parameters: { ...o, workloads },
    policyRestored: restored,
    devicesRegisteredAndRevoked: run.devices.size,
    workloads: run.report.workloads,
    errors: run.report.errors,
    correct: Object.values(run.report.workloads).every((w) => w.correct) && run.report.errors.length === 0 && restored,
  };
  writeFileSync(a.out, `${JSON.stringify(report, null, 2)}\n`);
  process.stderr.write(`capacity: report written to ${a.out} (correct: ${report.correct})\n`);
  return report.correct ? 0 : 1;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  main(process.argv.slice(2)).then(
    (code) => process.exit(code),
    (e) => {
      console.error(e instanceof SafetyError ? `REFUSED: ${e.message}` : `capacity: ${e.message}`);
      process.exit(e instanceof SafetyError ? 3 : 2);
    },
  );
}
