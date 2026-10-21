// Unit tests of the hosted-staging kit's safety rules and report logic (no network).
//   node --test scripts/staging/test/
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, chmodSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { inflateSync } from "node:zlib";
import { PRODUCTION_HOSTS, SafetyError, assertStagingTarget, noisePng, percentiles, readSecretFile, b64 } from "../lib.mjs";
import { evaluate, markdown } from "../report.mjs";

test("production hosts are always refused, even when allowlisted", () => {
  for (const h of PRODUCTION_HOSTS) {
    assert.throws(() => assertStagingTarget(`https://${h}`, [h]), SafetyError);
    assert.throws(() => assertStagingTarget(`https://${h.toUpperCase()}/v1`, []), SafetyError);
  }
  assert.throws(() => assertStagingTarget("https://staging.example.test", ["staging.example.test", "api.cfmeet.com"]), SafetyError, "an allowlist naming production is refused");
});

test("hosted targets need https and the explicit allowlist; loopback is a rehearsal", () => {
  assert.throws(() => assertStagingTarget("http://staging.example.test", ["staging.example.test"]), SafetyError);
  assert.throws(() => assertStagingTarget("https://other.example.test", ["staging.example.test"]), SafetyError);
  assert.throws(() => assertStagingTarget("not a url", []), SafetyError);
  assert.equal(assertStagingTarget("https://staging.example.test", ["staging.example.test"]).hostname, "staging.example.test");
  assert.equal(assertStagingTarget("http://127.0.0.1:8080", []).port, "8080");
  assert.equal(assertStagingTarget("http://localhost:1", []).hostname, "localhost");
});

test("credential files must be private and non-empty", () => {
  const dir = mkdtempSync(join(tmpdir(), "cf-staging-kit-"));
  try {
    const f = join(dir, "cred");
    writeFileSync(f, "  secret-value \n");
    chmodSync(f, 0o644);
    assert.throws(() => readSecretFile(f, "credential"), SafetyError);
    chmodSync(f, 0o600);
    assert.equal(readSecretFile(f, "credential"), "secret-value");
    writeFileSync(f, "\n");
    assert.throws(() => readSecretFile(f, "credential"), SafetyError);
    assert.throws(() => readSecretFile(undefined, "credential"), SafetyError);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("percentiles, encoding and generated media", () => {
  assert.deepEqual(percentiles([]), { samples: 0 });
  const p = percentiles([5, 1, 4, 2, 3]);
  assert.equal(p.p50_ms, 3);
  assert.equal(p.max_ms, 5);
  assert.equal(b64([0xfb, 0xff]), "-_8");
  const png = noisePng(10000);
  assert.deepEqual([...png.subarray(0, 8)], [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  assert.ok(png.length >= 10000, "random pixels do not compress");
  const idatLen = png.readUInt32BE(33);
  assert.equal(png.subarray(37, 41).toString(), "IDAT");
  const raw = inflateSync(png.subarray(41, 41 + idatLen));
  assert.equal(raw.length, (1 + 256 * 3) * png.readUInt32BE(20));
});

test("reports never invent a threshold; owner thresholds and correctness are enforced", () => {
  const report = {
    label: "t",
    target: { messaging: "http://127.0.0.1:1", loopback: true },
    authorization: null,
    startedAt: "a",
    finishedAt: "b",
    runner: { node: "v22", platform: "linux", cpus: 2 },
    parameters: { members: 2 },
    policyRestored: true,
    devicesRegisteredAndRevoked: 1,
    errors: [],
    correct: true,
    workloads: { fanout: { correct: true, send_to_receive_latency: { samples: 2, p50_ms: 3, p95_ms: 9, p99_ms: 9, max_ms: 9 } } },
  };
  assert.match(evaluate(report).verdict, /^MEASURED/);
  assert.equal(evaluate(report, { fanout: { "send_to_receive_latency.p95_ms": { max: 10 } } }).verdict, "PASS");
  assert.equal(evaluate(report, { fanout: { "send_to_receive_latency.p95_ms": { max: 5 } } }).verdict, "FAIL");
  assert.equal(evaluate(report, { fanout: { "no.such.path": { max: 5 } } }).verdict, "FAIL", "a missing measurement fails");
  const broken = { ...report, correct: false, workloads: { fanout: { correct: false, error: "x" } } };
  assert.equal(evaluate(broken).verdict, "FAIL");
  const md = markdown(report, evaluate(report));
  assert.match(md, /loopback rehearsal, not hosted staging/);
  assert.match(md, /not a production capacity claim/);
});
