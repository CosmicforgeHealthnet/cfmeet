import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import vm from "node:vm";

test("encrypted link cards accept bounded inline JPEGs and refuse remote tracking or executable images", () => {
  const worker = readFileSync(new URL("../messaging-crypto/worker.src.js", import.meta.url), "utf8");
  const section = worker.slice(worker.indexOf("const str ="), worker.indexOf("function coordinate("));
  const validate = vm.runInNewContext(section + ";linkPreview", { URL, fail: (code) => { throw new Error(code); }, fetch: () => { throw new Error("A preview fetched a resource"); } });
  const card = { url: "https://example.com/article", title: "Article", description: "A description", siteName: "example.com" };
  const image = "data:image/jpeg;base64,/9j/" + "A".repeat(32);
  assert.equal(validate({ ...card, imageDataUrl: image }).imageDataUrl, image);
  for (const imageDataUrl of ["https://tracker.example/pixel", "data:image/svg+xml,<svg/>", "data:text/html;base64,AAAA", image + "A".repeat(24576)]) {
    assert.throws(() => validate({ ...card, imageDataUrl }), /PAYLOAD_INVALID/);
  }
  assert.throws(() => validate({ ...card, url: "javascript:alert(1)" }), /PAYLOAD_INVALID/);
  assert.throws(() => validate({ ...card, title: "x".repeat(201) }), /PAYLOAD_INVALID/);
  assert.equal(validate(card).title, "Article");
});
