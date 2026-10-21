import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, appendFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const script = new URL("../scripts/verify-build-assets.mjs", import.meta.url).pathname;
function run(mode, dir, publicDir = join(dir, "no-public")) {
  return spawnSync(process.execPath, [script, mode, dir], { encoding: "utf8", env: { ...process.env, CFMEET_PUBLIC_DIR: publicDir } });
}

test("release asset manifest detects mutation, addition and deletion", async () => {
  const dir = await mkdtemp(join(tmpdir(), "cfmeet-assets-"));
  const chunks = join(dir, "static", "chunks");
  try {
    await mkdir(chunks, { recursive: true });
    const chunk = join(chunks, "abc.js");
    await writeFile(chunk, "console.log('release')");
    await writeFile(join(chunks, "style.css"), "body {color: black}");
    assert.notEqual(run("--verify", dir).status, 0, "unrecorded build refused");
    assert.equal(run("--write", dir).status, 0);
    assert.notEqual(run("--write", dir).status, 0, "cannot overwrite original manifest");
    assert.equal(run("--verify", dir).status, 0);
    await appendFile(chunk, "\nconsole.log('tampered')");
    assert.notEqual(run("--verify", dir).status, 0);
    await writeFile(chunk, "console.log('release')");
    await writeFile(join(chunks, "injected.js"), "console.log('new')");
    assert.notEqual(run("--verify", dir).status, 0);
    await rm(join(chunks, "injected.js"));
    await rm(chunk);
    assert.notEqual(run("--verify", dir).status, 0);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("an empty Next static build cannot be signed", async () => {
  const dir = await mkdtemp(join(tmpdir(), "cfmeet-assets-"));
  try {
    await mkdir(join(dir, "static"));
    assert.notEqual(run("--write", dir).status, 0);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test("the served crypto worker artifact and Trusted Types policy are pinned too", async () => {
  const dir = await mkdtemp(join(tmpdir(), "cfmeet-assets-"));
  const pub = join(dir, "public");
  try {
    await mkdir(join(dir, "static", "chunks"), { recursive: true });
    await writeFile(join(dir, "static", "chunks", "a.js"), "1");
    await mkdir(join(pub, "messaging-crypto"), { recursive: true });
    await mkdir(join(pub, "security"), { recursive: true });
    const wasm = join(pub, "messaging-crypto", "cf-messaging-crypto.0123.wasm");
    await writeFile(wasm, Buffer.from([0, 97, 115, 109]));
    await writeFile(join(pub, "messaging-crypto", "crypto-worker.0123.js"), "worker");
    await writeFile(join(pub, "security", "trusted-types-default.js"), "policy");
    assert.equal(run("--write", dir, pub).status, 0);
    assert.equal(run("--verify", dir, pub).status, 0);
    await appendFile(wasm, Buffer.from([1]));
    assert.notEqual(run("--verify", dir, pub).status, 0, "a tampered crypto module is detected");
    await writeFile(wasm, Buffer.from([0, 97, 115, 109]));
    await writeFile(join(pub, "security", "trusted-types-default.js"), "policy-changed");
    assert.notEqual(run("--verify", dir, pub).status, 0, "a changed Trusted Types policy is detected");
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
