// Gap audit A22 (web): the chosen profile photo is shrunk in the browser to fit the
// Meeting Service's 512 KiB avatar limit (decode with EXIF orientation, longest side
// <= 512 px, JPEG at decreasing quality), with clear errors for non-images and HEIC.
//   node --experimental-strip-types --test tests/avatar-image.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import {
  AVATAR_ERROR_MESSAGES,
  AVATAR_MAX_BYTES,
  AVATAR_QUALITIES,
  AVATAR_TARGET_BYTES,
  browserCodec,
  fitWithin,
  looksLikeImage,
  prepareAvatarImage,
} from "../src/lib/account/avatar-image.ts";

const KB = 1024;
const file = (size, type, name = "photo.jpg") => new File([new Uint8Array(size)], name, { type });

/** A fake codec: decodes to `width` x `height`; encoded size = f(width, height, quality). */
function fakeCodec({ width = 4032, height = 3024, size = (w, h, q) => Math.round(w * h * q), decodeFails = false, encodeType = "image/jpeg" } = {}) {
  const calls = { decode: 0, encode: [], closed: 0 };
  return {
    calls,
    decode: async () => {
      calls.decode += 1;
      if (decodeFails) throw new Error("cannot decode");
      return { width, height, source: {}, close: () => { calls.closed += 1; } };
    },
    encode: async (_img, w, h, type, quality) => {
      calls.encode.push({ w, h, type, quality });
      return new Blob([new Uint8Array(size(w, h, quality))], { type: encodeType });
    },
  };
}

test("the target leaves a safety margin under the server limit (account_avatar.rs MAX_AVATAR_BYTES)", () => {
  assert.equal(AVATAR_MAX_BYTES, 512 * KB);
  assert.ok(AVATAR_TARGET_BYTES < AVATAR_MAX_BYTES);
  const rust = readFileSync(new URL("../../../services/meeting-service/src/services/account_avatar.rs", import.meta.url), "utf8");
  assert.match(rust, /MAX_AVATAR_BYTES: usize = 512 \* 1024;/);
});

test("fitWithin keeps the aspect ratio, caps the longest side and never enlarges", () => {
  assert.deepEqual(fitWithin(4032, 3024, 512), { width: 512, height: 384 });
  assert.deepEqual(fitWithin(3024, 4032, 512), { width: 384, height: 512 });
  assert.deepEqual(fitWithin(4000, 4000, 512), { width: 512, height: 512 });
  assert.deepEqual(fitWithin(300, 200, 512), { width: 300, height: 200 });
  assert.deepEqual(fitWithin(10000, 1, 512), { width: 512, height: 1 });
});

test("a large phone photo is shrunk to a JPEG of at most 512 px under the target", async () => {
  const codec = fakeCodec(); // 4032x3024 -> 512x384; q0.9 -> ~177 KB
  const r = await prepareAvatarImage(file(4 * 1024 * KB, "image/jpeg", "IMG_0001.JPG"), codec);
  assert.equal(r.ok, true);
  assert.equal(r.file.type, "image/jpeg");
  assert.equal(r.file.name, "IMG_0001.jpg");
  assert.ok(r.file.size <= AVATAR_TARGET_BYTES);
  assert.deepEqual(codec.calls.encode, [{ w: 512, h: 384, type: "image/jpeg", quality: 0.9 }]);
  assert.equal(codec.calls.closed, 1);
});

test("quality is lowered step by step until the result fits", async () => {
  // Bytes = w*h*q*4: 512x384 needs q <= 0.61 to fit 480 KB.
  const codec = fakeCodec({ size: (w, h, q) => Math.round(w * h * q * 4) });
  const r = await prepareAvatarImage(file(3 * 1024 * KB, "image/png", "scan.png"), codec);
  assert.equal(r.ok, true);
  assert.deepEqual(codec.calls.encode.map((c) => c.quality), [0.9, 0.82, 0.72, 0.6]);
  assert.ok(r.file.size <= AVATAR_TARGET_BYTES);
  assert.equal(r.file.name, "scan.jpg");
});

test("when no quality fits at 512 px, smaller sizes are tried", async () => {
  const codec = fakeCodec({ size: (w, h, q) => (w === 512 ? AVATAR_MAX_BYTES : Math.round(w * h * q)) });
  const r = await prepareAvatarImage(file(2 * 1024 * KB, "image/webp"), codec);
  assert.equal(r.ok, true);
  assert.equal(codec.calls.encode.length, AVATAR_QUALITIES.length + 1);
  assert.deepEqual(codec.calls.encode.at(-1), { w: 384, h: 288, type: "image/jpeg", quality: 0.9 });
});

test("the existing 'too big' message is shown only if it can never be made small enough", async () => {
  const codec = fakeCodec({ size: () => AVATAR_MAX_BYTES });
  const r = await prepareAvatarImage(file(2 * 1024 * KB, "image/jpeg"), codec);
  assert.deepEqual(r, { ok: false, error: "too-big" });
  assert.equal(AVATAR_ERROR_MESSAGES["too-big"], "That photo is too big. Use one under 512 KB.");
  assert.equal(codec.calls.closed, 1);
});

test("small PNG / JPEG / WebP files are sent unchanged", async () => {
  for (const type of ["image/png", "image/jpeg", "image/webp"]) {
    const codec = fakeCodec();
    const f = file(200 * KB, type);
    const r = await prepareAvatarImage(f, codec);
    assert.equal(r.ok, true);
    assert.equal(r.file, f);
    assert.equal(codec.calls.decode, 0);
  }
});

test("files just over the target (but under the limit) are still re-encoded for the margin", async () => {
  const codec = fakeCodec();
  const r = await prepareAvatarImage(file(500 * KB, "image/jpeg"), codec);
  assert.equal(r.ok, true);
  assert.equal(codec.calls.decode, 1);
});

test("small images of other types (GIF, BMP, AVIF) are converted to JPEG", async () => {
  const codec = fakeCodec({ width: 200, height: 100 });
  const r = await prepareAvatarImage(file(10 * KB, "image/gif", "a.gif"), codec);
  assert.equal(r.ok, true);
  assert.equal(r.file.type, "image/jpeg");
  assert.deepEqual(codec.calls.encode[0], { w: 200, h: 100, type: "image/jpeg", quality: 0.9 });
});

test("a file that is not an image is refused with the existing message, without decoding", async () => {
  const codec = fakeCodec();
  assert.deepEqual(await prepareAvatarImage(file(10 * KB, "application/pdf", "cv.pdf"), codec), { ok: false, error: "not-image" });
  assert.deepEqual(await prepareAvatarImage(file(10 * KB, "", "notes.txt"), codec), { ok: false, error: "not-image" });
  assert.equal(codec.calls.decode, 0);
  assert.equal(AVATAR_ERROR_MESSAGES["not-image"], "Use a PNG, JPEG or WebP image.");
  assert.equal(looksLikeImage(file(1, "", "IMG_1.HEIC")), true);
});

test("a HEIC photo the browser cannot decode asks for a JPEG or PNG", async () => {
  const codec = fakeCodec({ decodeFails: true });
  assert.deepEqual(await prepareAvatarImage(file(2 * 1024 * KB, "image/heic", "IMG_2.HEIC"), codec), { ok: false, error: "unsupported-heic" });
  assert.deepEqual(await prepareAvatarImage(file(2 * 1024 * KB, "", "IMG_3.heif"), codec), { ok: false, error: "unsupported-heic" });
  assert.match(AVATAR_ERROR_MESSAGES["unsupported-heic"], /JPEG or PNG/);
});

test("a HEIC photo the browser can decode (Safari) is converted to JPEG", async () => {
  const codec = fakeCodec();
  const r = await prepareAvatarImage(file(2 * 1024 * KB, "image/heic", "IMG_4.HEIC"), codec);
  assert.equal(r.ok, true);
  assert.equal(r.file.type, "image/jpeg");
  assert.equal(r.file.name, "IMG_4.jpg");
});

test("a corrupt image or a canvas that cannot encode JPEG reports an unreadable photo", async () => {
  assert.deepEqual(await prepareAvatarImage(file(2 * 1024 * KB, "image/jpeg"), fakeCodec({ decodeFails: true })), { ok: false, error: "undecodable" });
  assert.deepEqual(await prepareAvatarImage(file(2 * 1024 * KB, "image/jpeg"), fakeCodec({ width: 0, height: 0 })), { ok: false, error: "undecodable" });
  assert.deepEqual(await prepareAvatarImage(file(2 * 1024 * KB, "image/jpeg"), fakeCodec({ encodeType: "image/png" })), { ok: false, error: "undecodable" });
});

test("the browser codec decodes with EXIF orientation and draws on a white canvas", async (t) => {
  const seen = { bitmapOptions: [], draws: [], fills: [], blobs: [], closed: 0 };
  const saved = { createImageBitmap: globalThis.createImageBitmap, OffscreenCanvas: globalThis.OffscreenCanvas };
  t.after(() => Object.assign(globalThis, saved));
  globalThis.createImageBitmap = async (_blob, options) => {
    seen.bitmapOptions.push(options);
    return { width: 3024, height: 4032, close: () => { seen.closed += 1; } };
  };
  globalThis.OffscreenCanvas = class {
    constructor(w, h) { this.w = w; this.h = h; }
    getContext() {
      return {
        fillStyle: "",
        fillRect(x, y, w, h) { seen.fills.push([this.fillStyle, x, y, w, h]); },
        drawImage(_src, x, y, w, h) { seen.draws.push([x, y, w, h]); },
      };
    }
    async convertToBlob({ type, quality }) {
      seen.blobs.push({ type, quality, w: this.w, h: this.h });
      return new Blob([new Uint8Array(Math.round(this.w * this.h * quality))], { type });
    }
  };
  const r = await prepareAvatarImage(file(5 * 1024 * KB, "image/jpeg"), browserCodec());
  assert.equal(r.ok, true);
  assert.deepEqual(seen.bitmapOptions, [{ imageOrientation: "from-image" }]);
  assert.deepEqual(seen.blobs, [{ type: "image/jpeg", quality: 0.9, w: 384, h: 512 }]);
  assert.deepEqual(seen.fills, [["#ffffff", 0, 0, 384, 512]]);
  assert.deepEqual(seen.draws, [[0, 0, 384, 512]]);
  assert.equal(seen.closed, 1);
});

test("the browser codec falls back when imageOrientation is not supported", async (t) => {
  const seen = [];
  const saved = { createImageBitmap: globalThis.createImageBitmap, OffscreenCanvas: globalThis.OffscreenCanvas };
  t.after(() => Object.assign(globalThis, saved));
  globalThis.createImageBitmap = async (_blob, options) => {
    seen.push(options);
    if (options) throw new TypeError("unsupported option");
    return { width: 100, height: 100, close() {} };
  };
  globalThis.OffscreenCanvas = class {
    getContext() { return { fillRect() {}, drawImage() {} }; }
    async convertToBlob({ type }) { return new Blob([new Uint8Array(10)], { type }); }
  };
  const r = await prepareAvatarImage(file(600 * KB, "image/png"), browserCodec());
  assert.equal(r.ok, true);
  assert.deepEqual(seen, [{ imageOrientation: "from-image" }, undefined]);
});

test("the settings screen uses the shrinking step and no longer refuses large files up front", () => {
  const src = readFileSync(new URL("../src/components/account/AccountProfileSettings.tsx", import.meta.url), "utf8");
  assert.match(src, /const prepared\s*=\s*await prepareAvatarImage\([A-Za-z_$][\w$]*\)/);
  assert.doesNotMatch(src, /file\.size > 512 \* 1024/);
  assert.match(src, /uploadAvatar\(prepared\.file\)/);
});
