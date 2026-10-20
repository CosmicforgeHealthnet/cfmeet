import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, appendFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const script = new URL("../scripts/verify-build-assets.mjs", import.meta.url).pathname;
function run(mode, dir) {
  return spawnSync(process.execPath, [script, mode, dir], { encoding: "utf8" });
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
