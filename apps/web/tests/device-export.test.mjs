import assert from "node:assert/strict";
import test from "node:test";
import vm from "node:vm";
import { readFileSync } from "node:fs";
import { collectDeviceHistory } from "../src/lib/messaging/device-export-data.ts";

test("device history traverses all local pages and rejects a repeated continuation", async () => {
  const cursor = { conversationId: "11111111-1111-4111-8111-111111111111", sequence: 100 };
  const seen = [];
  const result = await collectDeviceHistory(async (after) => {
    seen.push(after);
    return after ? { records: ["last"], next: null } : { records: ["first"], next: cursor };
  });
  assert.deepEqual(result, { records: ["first", "last"], complete: true, next: null });
  assert.deepEqual(seen, [null, cursor]);
  await assert.rejects(() => collectDeviceHistory(async () => ({ records: [], next: cursor })), /did not advance/);
});

test("local plaintext export excludes device secrets and file decryption descriptors", () => {
  const source = readFileSync(new URL("../messaging-crypto/worker.src.js", import.meta.url), "utf8");
  const section = source.slice(source.indexOf("function view(r)"), source.indexOf("async function loadRecord("));
  const exportRecord = vm.runInNewContext(section + ";exportRecord");
  const result = exportRecord("conversation", {
    sequence: 3, status: "DECRYPTED", payload: { t: "file", d: "secret-content-key", caption: "photo" },
    attachmentId: "attachment", privateKey: "private", state: "epoch-secret", wrappingKey: "key",
  });
  assert.equal(result.content.caption, "photo");
  assert.equal(result.content.attachmentId, "attachment");
  for (const secret of ["secret-content-key", "privateKey", "wrappingKey", "epoch-secret"]) assert.ok(!JSON.stringify(result).includes(secret));
  assert.equal(exportRecord("conversation", { sequence: 3, status: "REDACTED", payload: { t: "text", text: "removed" } }).content, null);
});
