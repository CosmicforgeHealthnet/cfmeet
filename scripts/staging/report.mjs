#!/usr/bin/env node
// Turns a capacity-runner JSON report into Markdown evidence.
//
//   node scripts/staging/report.mjs --in report.json [--thresholds owner-thresholds.json] [--out report.md]
//
// Without --thresholds every measurement is reported as MEASURED: the kit never invents a
// performance target. A thresholds file is written and approved by the owner, e.g.
//   { "fanout": { "send_to_receive_latency.p95_ms": { "max": <owner value> } } }
// and each listed check becomes PASS or FAIL. Correctness (every message accepted,
// delivered once and in order; attachments byte-identical) is always a hard check.

import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { parseArgs } from "./lib.mjs";

const get = (obj, path) => path.split(".").reduce((o, k) => (o == null ? undefined : o[k]), obj);

export function evaluate(report, thresholds = {}) {
  const checks = [];
  for (const [w, r] of Object.entries(report.workloads ?? {})) {
    checks.push({ workload: w, check: "correctness", value: r.correct, outcome: r.correct ? "PASS" : "FAIL", detail: r.error ?? "" });
    for (const [path, rule] of Object.entries(thresholds[w] ?? {})) {
      const value = get(r, path);
      let outcome = "PASS";
      if (typeof value !== "number") outcome = "FAIL";
      else if (rule.max !== undefined && value > rule.max) outcome = "FAIL";
      else if (rule.min !== undefined && value < rule.min) outcome = "FAIL";
      checks.push({ workload: w, check: path, value, outcome, detail: JSON.stringify(rule) });
    }
  }
  const failed = checks.some((c) => c.outcome === "FAIL") || !report.correct;
  const thresholded = Object.keys(thresholds).length > 0;
  return { checks, verdict: failed ? "FAIL" : thresholded ? "PASS" : "MEASURED (no owner thresholds supplied)" };
}

function fmt(p) {
  if (!p || typeof p !== "object" || !("samples" in p)) return String(p);
  if (!p.samples) return "no samples";
  return `p50 ${p.p50_ms} / p95 ${p.p95_ms} / p99 ${p.p99_ms} / max ${p.max_ms} ms (n=${p.samples})`;
}

export function markdown(report, evaluation) {
  const lines = [];
  lines.push(`# Hosted staging capacity — ${report.label || report.startedAt}`, "");
  lines.push(`| Field | Value |`, `|---|---|`);
  lines.push(`| Target | ${report.target.messaging}${report.target.loopback ? " (loopback rehearsal, not hosted staging)" : ""} |`);
  lines.push(`| Owner authorization | ${report.authorization ?? "— (loopback)"} |`);
  lines.push(`| Window | ${report.startedAt} → ${report.finishedAt} |`);
  lines.push(`| Runner | ${report.runner.node}, ${report.runner.platform}, ${report.runner.cpus} CPUs |`);
  lines.push(`| Parameters | ${Object.entries(report.parameters).filter(([k]) => k !== "messagingUrl").map(([k, v]) => `${k}=${v}`).join(", ")} |`);
  lines.push(`| Policy restored / devices revoked | ${report.policyRestored} / ${report.devicesRegisteredAndRevoked} |`);
  lines.push(`| Verdict | **${evaluation.verdict}** |`, "");
  for (const [w, r] of Object.entries(report.workloads)) {
    lines.push(`## ${w}`, "", `| Measurement | Value |`, `|---|---|`);
    for (const [k, v] of Object.entries(r)) lines.push(`| ${k} | ${typeof v === "object" ? (v && "samples" in v ? fmt(v) : `\`${JSON.stringify(v)}\``) : v} |`);
    lines.push("");
  }
  lines.push(`## Checks`, "", `| Workload | Check | Value | Rule | Outcome |`, `|---|---|---|---|---|`);
  for (const c of evaluation.checks) lines.push(`| ${c.workload} | ${c.check} | ${c.value} | ${c.detail} | ${c.outcome} |`);
  if (report.errors?.length) lines.push("", `Errors: ${report.errors.join("; ")}`);
  lines.push("", "These numbers describe this target and runner only; they are not a production capacity claim.");
  return `${lines.join("\n")}\n`;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const a = parseArgs(process.argv.slice(2), { defaults: { in: undefined, thresholds: undefined, out: undefined } });
  const report = JSON.parse(readFileSync(a.in, "utf8"));
  const thresholds = a.thresholds ? JSON.parse(readFileSync(a.thresholds, "utf8")) : {};
  const ev = evaluate(report, thresholds);
  const md = markdown(report, ev);
  if (a.out) writeFileSync(a.out, md);
  else process.stdout.write(md);
  process.exit(ev.verdict === "FAIL" ? 1 : 0);
}
