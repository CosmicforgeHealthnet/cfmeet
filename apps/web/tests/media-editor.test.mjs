// Media editors (photo / collage / text status / overlays): the pure rules behind them.
//   node --experimental-strip-types --test tests/media-editor.test.mjs
import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";

register("./helpers/ts-resolve.mjs", import.meta.url);
const ov = await import("../src/lib/media-editor/overlays.ts");
const hist = await import("../src/lib/media-editor/history.ts");
const crop = await import("../src/lib/media-editor/crop.ts");
const filters = await import("../src/lib/media-editor/filters.ts");
const photo = await import("../src/lib/media-editor/photo.ts");
const collage = await import("../src/lib/media-editor/collage.ts");
const ts = await import("../src/lib/media-editor/text-status.ts");
const fonts = await import("../src/lib/media-editor/fonts.ts");
const emoji = await import("../src/lib/media-editor/emoji.ts");

const close = (a, b, eps = 1e-6) => assert.ok(Math.abs(a - b) <= eps, `${a} ≉ ${b}`);
/** Every glyph is 0.5 em wide: deterministic text measuring. */
const measure = (text, font) => [...text].length * parseFloat(/(\d+(?:\.\d+)?)px/.exec(font)[1]) * 0.5;

// ------------------------------------------------------------------ overlays

test("text layout wraps at the reference width and keeps explicit newlines", () => {
  const short = ov.layoutText({ text: "Hi\nthere", font: 0, size: 0.1, background: "none" }, measure);
  assert.deepEqual(short.lines.map((l) => l.text), ["Hi", "there"]);
  close(short.fontPx, 100);
  close(short.height, 2 * 100 * ov.LINE_HEIGHT);
  const long = ov.layoutText({ text: "word ".repeat(40).trim(), font: 0, size: 0.1, background: "solid" }, measure);
  assert.ok(long.lines.length > 1);
  for (const l of long.lines) assert.ok(l.width <= ov.WRAP_WIDTH, `line ${l.text} too wide`);
  assert.equal(long.lines.join(" ").length > 0, true);
  // A word longer than a line is split by characters.
  const word = ov.wrapText("x".repeat(30), 100, (s) => s.length * 10);
  assert.deepEqual(word, ["xxxxxxxxxx", "xxxxxxxxxx", "xxxxxxxxxx"]);
});

test("the display font upper-cases its text", () => {
  const l = ov.layoutText({ text: "abc", font: 4, size: 0.1, background: "none" }, measure);
  assert.equal(l.lines[0].text, "ABC");
});

test("overlay boxes scale with the media and hit-testing honours rotation", () => {
  const s = ov.createSticker("🎉", { x: 0.5, y: 0.5, size: 0.2 });
  const a = ov.overlayBox(s, 1000, 2000, measure);
  const b = ov.overlayBox(s, 500, 1000, measure);
  close(a.width / b.width, 2);
  close(a.cx, 500); close(a.cy, 1000);
  // A wide text rotated 90° covers a tall area, not a wide one.
  const t = ov.createText({ text: "wide wide wide", x: 0.5, y: 0.5, rotation: Math.PI / 2 });
  const box = ov.overlayBox(t, 1000, 1000, measure);
  assert.ok(box.width > box.height);
  assert.equal(ov.pointInBox(box, 500, 500 + box.width / 2 - 2), true);
  assert.equal(ov.pointInBox(box, 500 + box.width / 2 - 2, 500), false);
  // Topmost first; strokes are not selectable.
  const under = ov.createSticker("🙂", { x: 0.5, y: 0.5 });
  const over = ov.createSticker("😎", { x: 0.5, y: 0.5 });
  const stroke = { id: "s", type: "stroke", color: "#fff", width: 0.02, points: [0, 0.5, 1, 0.5] };
  assert.equal(ov.hitTest([under, over, stroke], 500, 500, 1000, 1000, measure).id, over.id);
  assert.equal(ov.hitTest([stroke], 500, 500, 1000, 1000, measure), null);
  assert.equal(ov.hitTest([under], 10, 10, 1000, 1000, measure), null);
});

test("box corners rotate around the centre", () => {
  const corners = ov.boxCorners({ cx: 0, cy: 0, width: 2, height: 2, rotation: Math.PI / 2 });
  close(corners[0].x, 1); close(corners[0].y, -1);
});

test("the eraser removes only strokes it touches", () => {
  const a = { id: "a", type: "stroke", color: "#f00", width: 0.01, points: [0.1, 0.1, 0.9, 0.1] };
  const b = { id: "b", type: "stroke", color: "#f00", width: 0.01, points: [0.1, 0.9, 0.9, 0.9] };
  const dot = { id: "d", type: "stroke", color: "#f00", width: 0.05, points: [0.5, 0.5] };
  const s = ov.createSticker("⭐", { x: 0.5, y: 0.1 });
  const out = ov.eraseAt([a, b, dot, s], 500, 102, 1000, 1000, 6);
  assert.deepEqual(out.map((o) => o.id), ["b", "d", s.id]);
  assert.deepEqual(ov.eraseAt(out, 505, 505, 1000, 1000, 2).map((o) => o.id), ["b", s.id]);
  const same = [b];
  assert.equal(ov.eraseAt(same, 0, 0, 1000, 1000, 2), same, "unchanged list is returned as is");
  close(ov.distanceToSegment(5, 5, 0, 0, 10, 0), 5);
  close(ov.distanceToSegment(-3, 4, 0, 0, 10, 0), 5);
});

test("strokes skip points closer than the minimum spacing", () => {
  let s = { id: "s", type: "stroke", color: "#fff", width: 0.01, points: [] };
  s = ov.extendStroke(s, 100, 100, 1000, 500);
  s = ov.extendStroke(s, 101, 100, 1000, 500);
  s = ov.extendStroke(s, 110, 100, 1000, 500);
  assert.deepEqual(s.points, [0.1, 0.2, 0.11, 0.2]);
});

test("pinch and corner-handle gestures scale, rotate and move", () => {
  const start = ov.createSticker("🔥", { x: 0.5, y: 0.5, scale: 1, rotation: 0 });
  const p = ov.pinchTransform(start, { x: 400, y: 500 }, { x: 600, y: 500 }, { x: 400, y: 600 }, { x: 800, y: 600 }, 1000, 1000);
  close(p.scale, 2);
  close(p.x, 0.6); close(p.y, 0.6);
  close(p.rotation, 0);
  const rotated = ov.pinchTransform(start, { x: 400, y: 500 }, { x: 600, y: 500 }, { x: 500, y: 400 }, { x: 500, y: 600 }, 1000, 1000);
  close(rotated.rotation, Math.PI / 2);
  const h = ov.handleTransform(start, { x: 600, y: 600 }, { x: 700, y: 700 }, 1000, 1000);
  close(h.scale, 2);
  close(h.rotation, 0);
  assert.equal(ov.scaleTo(start, 100).scale, ov.MAX_SCALE);
  assert.equal(ov.scaleTo(start, 0).scale, ov.MIN_SCALE);
  close(ov.snapAngle(Math.PI / 2 + 0.03), Math.PI / 2);
  close(ov.snapAngle(0.5), 0.5);
  const moved = ov.moveBy(start, 2000, -2000, 1000, 1000);
  assert.deepEqual([moved.x, moved.y], [1, 0], "centre stays on the media");
});

test("overlay list edits", () => {
  const a = ov.createSticker("a");
  const b = ov.createSticker("b");
  assert.deepEqual(ov.bringToFront([a, b], a.id).map((o) => o.id), [b.id, a.id]);
  assert.deepEqual(ov.removeOverlay([a, b], a.id).map((o) => o.id), [b.id]);
  assert.equal(ov.updateOverlay([a, b], b.id, (o) => ({ ...o, scale: 3 }))[1].scale, 3);
  assert.notEqual(a.id, b.id);
  assert.equal(ov.contrastInk("#ffffff"), "#000000");
  assert.equal(ov.contrastInk("#1e293b"), "#ffffff");
});

// ------------------------------------------------------------------ history

test("undo / redo reducer", () => {
  let h = hist.initHistory(0);
  h = hist.historyReducer(h, { type: "commit", value: 1 });
  h = hist.historyReducer(h, { type: "commit", value: 2 });
  assert.equal(h.present, 2);
  h = hist.historyReducer(h, { type: "undo" });
  assert.equal(h.present, 1);
  assert.equal(hist.canRedo(h), true);
  h = hist.historyReducer(h, { type: "redo" });
  assert.equal(h.present, 2);
  h = hist.historyReducer(h, { type: "undo" });
  h = hist.historyReducer(h, { type: "commit", value: 9 });
  assert.equal(hist.canRedo(h), false, "a new edit drops the redo branch");
  assert.deepEqual(h.past, [0, 1]);
  const same = hist.historyReducer(h, { type: "commit", value: 9 });
  assert.equal(same, h, "committing the present is a no-op");
  const empty = hist.initHistory("x");
  assert.equal(hist.historyReducer(empty, { type: "undo" }), empty);
  assert.equal(hist.historyReducer(empty, { type: "redo" }), empty);
  let big = hist.initHistory(0);
  for (let i = 1; i <= hist.HISTORY_LIMIT + 20; i++) big = hist.historyReducer(big, { type: "commit", value: i });
  assert.equal(big.past.length, hist.HISTORY_LIMIT);
  assert.equal(hist.historyReducer(big, { type: "reset", value: 5 }).past.length, 0);
});

// ------------------------------------------------------------------ crop

test("rotating and flipping the crop follows the image", () => {
  const c = { x: 0.1, y: 0.2, w: 0.3, h: 0.4 };
  const r = crop.rotateCrop(c, 1);
  for (const [k, v] of Object.entries({ x: 0.4, y: 0.1, w: 0.4, h: 0.3 })) close(r[k], v);
  const back = crop.rotateCrop(r, -1);
  for (const k of ["x", "y", "w", "h"]) close(back[k], c[k]);
  const four = [1, 1, 1, 1].reduce((acc) => crop.rotateCrop(acc, 1), c);
  for (const k of ["x", "y", "w", "h"]) close(four[k], c[k]);
  close(crop.flipCropH(c).x, 0.6);
  close(crop.flipCropV(c).y, 0.4);
  assert.equal(crop.rotateBy(0, -1), 270);
  assert.equal(crop.rotateBy(270, 1), 0);
  assert.deepEqual(crop.orientedSize(400, 300, 90), { width: 300, height: 400 });
});

test("orientation maps source corners the way applyOrientation draws them", () => {
  // 90° clockwise: the source's top-left goes to the top-right.
  assert.deepEqual(crop.orientPoint(0, 0, 400, 300, 90, false, false), { x: 300, y: 0 });
  assert.deepEqual(crop.orientPoint(0, 0, 400, 300, 270, false, false), { x: 0, y: 400 });
  assert.deepEqual(crop.orientPoint(0, 0, 400, 300, 180, false, false), { x: 400, y: 300 });
  assert.deepEqual(crop.orientPoint(10, 20, 400, 300, 0, true, false), { x: 390, y: 20 });
  // Replay applyOrientation on a recording context and compare with orientPoint.
  for (const rotation of [0, 90, 180, 270]) {
    for (const [flipH, flipV] of [[false, false], [true, false], [false, true]]) {
      let m = [1, 0, 0, 1, 0, 0];
      const mul = (n) => { const [a, b, c, d, e, f] = m; m = [a * n[0] + c * n[1], b * n[0] + d * n[1], a * n[2] + c * n[3], b * n[2] + d * n[3], a * n[4] + c * n[5] + e, b * n[4] + d * n[5] + f]; };
      const ctx = { scale: (x, y) => mul([x, 0, 0, y, 0, 0]), translate: (x, y) => mul([1, 0, 0, 1, x, y]), rotate: (t) => mul([Math.cos(t), Math.sin(t), -Math.sin(t), Math.cos(t), 0, 0]) };
      crop.applyOrientation(ctx, 400, 300, rotation, flipH, flipV, crop.FULL_CROP, ...Object.values(crop.orientedSize(400, 300, rotation)));
      const p = crop.orientPoint(37, 81, 400, 300, rotation, flipH, flipV);
      close(m[0] * 37 + m[2] * 81 + m[4], p.x, 1e-6);
      close(m[1] * 37 + m[3] * 81 + m[5], p.y, 1e-6);
    }
  }
});

test("aspect presets fit the largest crop of that ratio", () => {
  const sq = crop.fitAspect(crop.FULL_CROP, 1, 4000, 3000);
  close(crop.cropAspect(sq, 4000, 3000), 1);
  close(sq.h, 1); close(sq.w, 0.75); close(sq.x, 0.125);
  const tall = crop.fitAspect({ x: 0, y: 0, w: 0.2, h: 0.2 }, 9 / 16, 4000, 3000);
  close(crop.cropAspect(tall, 4000, 3000), 9 / 16);
  close(tall.x, 0, 1e-9);
  assert.equal(crop.presetRatio(crop.ASPECT_PRESETS[1], 300, 200), 1.5);
  assert.equal(crop.presetRatio(crop.ASPECT_PRESETS[0], 300, 200), null);
});

test("resizing a crop keeps it inside, above the minimum and on aspect", () => {
  const free = crop.resizeCrop(crop.FULL_CROP, "se", -0.3, -0.2, null, 1000, 1000);
  assert.deepEqual(free, { x: 0, y: 0, w: 0.7, h: 0.8 });
  const tiny = crop.resizeCrop(crop.FULL_CROP, "nw", 2, 2, null, 1000, 1000);
  close(tiny.w, crop.MIN_CROP); close(tiny.x, 1 - crop.MIN_CROP);
  const start = crop.fitAspect(crop.FULL_CROP, 1, 2000, 1000);
  const sq = crop.resizeCrop(start, "se", -0.1, 0, 1, 2000, 1000);
  close(crop.cropAspect(sq, 2000, 1000), 1);
  close(sq.x, start.x); close(sq.y, start.y);
  const grown = crop.resizeCrop({ x: 0.4, y: 0.4, w: 0.2, h: 0.2 }, "e", 0.5, 0, 1, 1000, 1000);
  close(crop.cropAspect(grown, 1000, 1000), 1);
  assert.ok(grown.x + grown.w <= 1 + 1e-9 && grown.y >= -1e-9 && grown.y + grown.h <= 1 + 1e-9);
  const moved = crop.moveCrop({ x: 0.5, y: 0.5, w: 0.4, h: 0.4 }, 0.5, -1);
  assert.deepEqual(moved, { x: 0.6, y: 0, w: 0.4, h: 0.4 });
});

test("zooming a crop keeps its aspect and bounds", () => {
  const z = crop.zoomCrop({ x: 0, y: 0, w: 1, h: 1 }, 2);
  for (const [k, v] of Object.entries({ x: 0.25, y: 0.25, w: 0.5, h: 0.5 })) close(z[k], v);
  const out = crop.zoomCrop(z, 0.1);
  close(out.w, 1); close(out.h, 1);
  const deep = crop.zoomCrop(z, 1000);
  close(deep.w, crop.MIN_CROP);
});

test("pixel crop and output sizing", () => {
  assert.deepEqual(crop.cropPixels({ x: 0.5, y: 0.5, w: 0.5, h: 0.5 }, 101, 99), { x: 51, y: 50, width: 50, height: 49 });
  assert.deepEqual(crop.fitLongSide(4000, 3000, 2560), { width: 2560, height: 1920 });
  assert.deepEqual(crop.fitLongSide(800, 600, 2560), { width: 800, height: 600 });
  assert.deepEqual(crop.containSize(1, 400, 200), { width: 200, height: 200 });
  const edits = { ...photo.DEFAULT_EDITS, rotation: 90, crop: { x: 0, y: 0, w: 1, h: 0.5 } };
  assert.deepEqual(photo.outputSize(4000, 3000, edits), { width: 2560, height: 1707 });
  assert.deepEqual(photo.outputSize(4000, 3000, { ...edits, maxSide: 1080 }), { width: 1080, height: 720 });
});

// ------------------------------------------------------------------ filters & encoding

test("colour matrices compose and apply", () => {
  const px = new Uint8ClampedArray([200, 100, 50, 255]);
  filters.applyMatrix(px, filters.saturation(0));
  assert.equal(px[0], px[1]); assert.equal(px[1], px[2]);
  const c = filters.compose(filters.brightness(10), filters.brightness(5));
  close(c[4], 15);
  assert.equal(filters.isIdentity(filters.editMatrix("none", filters.NO_ADJUSTMENTS)), true);
  assert.equal(filters.isIdentity(filters.editMatrix("vivid", filters.NO_ADJUSTMENTS)), false);
  const bright = new Uint8ClampedArray([100, 100, 100, 255]);
  filters.applyMatrix(bright, filters.editMatrix("none", { brightness: 50, contrast: 0, saturation: 0 }));
  assert.ok(bright[0] > 100);
  const contrasty = new Uint8ClampedArray([200, 50, 128, 255]);
  filters.applyMatrix(contrasty, filters.contrast(2));
  assert.deepEqual([...contrasty], [255, 0, 128, 255]);
  assert.ok(filters.FILTER_PRESETS.length >= 6);
  assert.equal(new Set(filters.FILTER_PRESETS.map((p) => p.id)).size, filters.FILTER_PRESETS.length);
});

test("encoding picks the smaller format that fits, ignoring fake WebP", async () => {
  const blob = (type, size) => ({ type, size });
  const sizes = { "image/jpeg": [9e6, 7e6, 4e6], "image/webp": [8e6, 6e6, 3e6] };
  const calls = [];
  const enc = async (type, q) => { calls.push([type, q]); const i = calls.filter((c) => c[0] === type).length - 1; return blob(type, sizes[type][i] ?? 1); };
  const out = await photo.encodeSmallest(enc, 5e6);
  assert.deepEqual(out, { type: "image/webp", size: 3e6 });
  // Older Safari answers WebP with PNG: only JPEG counts, and WebP is not asked again.
  const tries = [];
  const safari = async (type) => { tries.push(type); return type === "image/webp" ? blob("image/png", 10) : blob("image/jpeg", 100); };
  assert.deepEqual(await photo.encodeSmallest(safari, 1000), { type: "image/jpeg", size: 100 });
  assert.deepEqual(tries, ["image/jpeg", "image/webp"]);
  tries.length = 0;
  await photo.encodeSmallest(async (type) => { tries.push(type); return type === "image/webp" ? blob("image/png", 10) : blob("image/jpeg", 5000); }, 1000);
  assert.equal(tries.filter((t) => t === "image/webp").length, 1, "WebP is not asked again once it proved fake");
  // Nothing fits: the smallest attempt is returned.
  const huge = async (type, q) => blob(type, Math.round(q * 1e8));
  assert.equal((await photo.encodeSmallest(huge, 10)).size, Math.round(0.48 * 1e8));
  await assert.rejects(photo.encodeSmallest(async () => null, 10));
});

// ------------------------------------------------------------------ collage

test("collage layouts cover the canvas for every photo count", () => {
  for (let n = collage.MIN_PHOTOS; n <= collage.MAX_PHOTOS; n++) {
    const ls = collage.layoutsFor(n);
    assert.ok(ls.length >= 2, `${n} photos have at least two layouts`);
    for (const l of ls) {
      const area = l.cells.reduce((s, c) => s + c.w * c.h, 0);
      close(area, 1, 1e-9);
      for (const c of l.cells) assert.ok(c.x >= 0 && c.y >= 0 && c.x + c.w <= 1 + 1e-9 && c.y + c.h <= 1 + 1e-9);
    }
  }
  assert.equal(collage.layoutForCount(3, "3-left").id, "3-left");
  assert.equal(collage.layoutForCount(4, "3-left").cells.length, 4);
});

test("cell rectangles leave equal gaps between and around cells", () => {
  const rects = collage.cellRects(collage.layoutById("4-grid"), 1080, 1920, 20);
  assert.deepEqual(rects[0], { x: 20, y: 20, w: 510, h: 930 });
  assert.deepEqual(rects[3], { x: 550, y: 970, w: 510, h: 930 });
  close(rects[1].x - (rects[0].x + rects[0].w), 20);
  const none = collage.cellRects(collage.layoutById("2-cols"), 1000, 1000, 0);
  assert.deepEqual(none, [{ x: 0, y: 0, w: 500, h: 1000 }, { x: 500, y: 0, w: 500, h: 1000 }]);
  assert.equal(collage.cellAt(rects, 600, 1000), 3);
  assert.equal(collage.cellAt(rects, 5, 5), -1);
});

test("photos cover their cell and pan / zoom stays inside the image", () => {
  // A landscape photo in a tall cell: full height, centred.
  const s = collage.coverSource(4000, 2000, 500, 1000, collage.DEFAULT_VIEW);
  assert.deepEqual(s, { sx: 1500, sy: 0, sw: 1000, sh: 2000 });
  const panned = collage.panView(4000, 2000, 500, 1000, collage.DEFAULT_VIEW, 100000, 0);
  close(collage.coverSource(4000, 2000, 500, 1000, panned).sx, 0);
  const zoomed = collage.zoomView(4000, 2000, 500, 1000, collage.DEFAULT_VIEW, 2);
  const zs = collage.coverSource(4000, 2000, 500, 1000, zoomed);
  close(zs.sw, 500); close(zs.sh, 1000);
  assert.equal(collage.zoomView(4000, 2000, 500, 1000, collage.DEFAULT_VIEW, 0.2).zoom, 1);
  assert.equal(collage.zoomView(4000, 2000, 500, 1000, collage.DEFAULT_VIEW, 100).zoom, collage.MAX_CELL_ZOOM);
  const slots = [{ photoId: "a", view: zoomed }, { photoId: "b", view: collage.DEFAULT_VIEW }];
  const swapped = collage.swapSlots(slots, 0, 1);
  assert.deepEqual(swapped.map((x) => x.photoId), ["b", "a"]);
  assert.deepEqual(swapped[1].view, collage.DEFAULT_VIEW);
});

// ------------------------------------------------------------------ text status

test("gradient validation", () => {
  assert.equal(ts.isValidGradient({ from: "#112233", to: "#AABBCC", angle: 90 }), true);
  for (const bad of [null, {}, { from: "#123", to: "#112233", angle: 0 }, { from: "#112233", to: "red", angle: 0 }, { from: "#112233", to: "#112233", angle: 360 }, { from: "#112233", to: "#112233", angle: 1.5 }, { from: "#112233", to: "#112233" }]) {
    assert.equal(ts.isValidGradient(bad), false, JSON.stringify(bad));
  }
  assert.deepEqual(ts.normalizeGradient({ from: "#AABBCC", to: "#000000", angle: -90.4 }), { from: "#aabbcc", to: "#000000", angle: 270 });
  assert.equal(ts.normalizeGradient({ from: "nope", to: "#000000", angle: 0 }), null);
  assert.equal(ts.gradientCss({ from: "#000000", to: "#ffffff", angle: 45 }), "linear-gradient(45deg, #000000, #ffffff)");
  for (const g of ts.GRADIENT_BACKGROUNDS) assert.equal(ts.isValidGradient(g), true);
  for (const c of [...ts.SOLID_BACKGROUNDS, ...ts.TEXT_COLORS]) assert.equal(ts.isHexColor(c), true);
});

test("text status background, sizing and normalising", () => {
  const g = { from: "#000000", to: "#ffffff", angle: 10 };
  assert.equal(ts.backgroundCss({ background: "#123456", backgroundGradient: g }), ts.gradientCss(g));
  assert.equal(ts.backgroundCss({ background: "#123456" }), "#123456");
  assert.equal(ts.backgroundCss({ background: "bad" }), ts.SOLID_BACKGROUNDS[0]);
  assert.equal(ts.representativeColor({ backgroundGradient: g }), "#000000");
  assert.ok(ts.textStatusFontSize("hi", 4) > ts.textStatusFontSize("hi", 0));
  assert.ok(ts.textStatusFontSize("x".repeat(600), 2) < ts.textStatusFontSize("short", 2));
  assert.equal(ts.textStatusFontSize("hi", 99), ts.TEXT_SIZE_CQW[4]);
  const n = ts.normalizeTextStatus({ text: "a", background: null, font: 9, textAlign: "middle", textSize: -3, textColor: "white" });
  assert.deepEqual(n, { text: "a", background: ts.SOLID_BACKGROUNDS[0], font: 4, textColor: "#ffffff", textAlign: "center", textSize: 0 });
  assert.deepEqual(ts.normalizeTextStatus({ text: "b", backgroundGradient: g, background: "#111111" }).backgroundGradient, g);
});

test("fonts and emoji data", () => {
  assert.ok(fonts.EDITOR_FONTS.length >= 5);
  assert.equal(fonts.TEXT_STATUS_FONT_COUNT, 5);
  assert.match(fonts.fontCss(1, 40), /^700 40px .*serif/);
  fonts.registerFontFamily(1, "'__Playfair_x'");
  assert.match(fonts.fontFamily(1), /^'__Playfair_x', /);
  assert.equal(fonts.displayText(4, "abc"), "ABC");
  assert.ok(emoji.EMOJI_CATEGORIES.length >= 8);
  for (const c of emoji.EMOJI_CATEGORIES) {
    assert.ok(c.emoji.length >= 40, c.id);
    assert.equal(new Set(c.emoji).size, c.emoji.length, `${c.id} has no duplicates (React keys)`);
  }
  assert.equal(emoji.allEmoji().length, emoji.EMOJI_CATEGORIES.reduce((s, c) => s + c.emoji.length, 0));
});
