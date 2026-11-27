// Status video editor (pure parts): 60 s segment planning, remove / reorder / trim of parts,
// edit reducer with undo / redo, crop geometry, output planning (≤ 720p, bitrates,
// pass-through) and size estimates. Real encoding runs in a browser only.
//   node --experimental-strip-types --test tests/media-editor-video.test.mjs
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import {
  MAX_SEGMENT_MS,
  MIN_SEGMENT_MS,
  aspectCrop,
  clampRange,
  createHistory,
  cropToPixels,
  editDurationMs,
  estimateOutputSize,
  flipCropH,
  formatBytes,
  formatDuration,
  historyReducer,
  initialVideoEdit,
  moveSegment,
  musicForPart,
  musicWindow,
  planOutput,
  planSegments,
  removeSegment,
  rotateCropCw,
  segmentsOf,
  trimSegment,
  videoEditReducer,
} from "../src/lib/media-editor/video-edit.ts";

const spans = (segs) => segs.map((s) => [s.startMs, s.endMs]);
const close = (a, b, eps = 1e-9) => assert.ok(Math.abs(a - b) < eps, `${a} ≉ ${b}`);

test("planSegments splits into consecutive parts of at most 60 s", () => {
  assert.deepEqual(spans(planSegments(8_000)), [[0, 8_000]]);
  assert.deepEqual(spans(planSegments(60_000)), [[0, 60_000]]);
  assert.deepEqual(spans(planSegments(125_000)), [[0, 60_000], [60_000, 120_000], [120_000, 125_000]]);
  assert.deepEqual(spans(planSegments(300_000)).length, 5);
  assert.deepEqual(planSegments(0), []);
  // custom max and offset
  assert.deepEqual(spans(planSegments(50_000, 20_000, 1_000)), [[1_000, 21_000], [21_000, 41_000], [41_000, 51_000]]);
});

test("planSegments never drops a short tail: the last two parts share it", () => {
  const parts = planSegments(120_400);
  assert.deepEqual(spans(parts), [[0, 60_000], [60_000, 90_200], [90_200, 120_400]]);
  for (const p of parts) assert.ok(p.endMs - p.startMs <= MAX_SEGMENT_MS && p.endMs - p.startMs >= MIN_SEGMENT_MS);
  // contiguous and complete
  for (let i = 1; i < parts.length; i++) assert.equal(parts[i].startMs, parts[i - 1].endMs);
  assert.equal(parts.at(-1).endMs, 120_400);
});

test("segmentsOf gives stable unique ids on the source timeline", () => {
  const segs = segmentsOf({ startMs: 5_000, endMs: 135_000 });
  assert.deepEqual(spans(segs), [[5_000, 65_000], [65_000, 125_000], [125_000, 135_000]]);
  assert.equal(new Set(segs.map((s) => s.id)).size, 3);
  assert.deepEqual(segmentsOf({ startMs: 5_000, endMs: 135_000 }).map((s) => s.id), segs.map((s) => s.id));
});

test("remove and reorder parts", () => {
  const segs = segmentsOf({ startMs: 0, endMs: 150_000 });
  const [a, b, c] = segs.map((s) => s.id);
  assert.deepEqual(moveSegment(segs, c, 0).map((s) => s.id), [c, a, b]);
  assert.deepEqual(moveSegment(segs, a, 99).map((s) => s.id), [b, c, a]);
  assert.equal(moveSegment(segs, b, 1), segs, "no-op keeps identity");
  assert.equal(moveSegment(segs, "nope", 0), segs);
  assert.deepEqual(removeSegment(segs, b).map((s) => s.id), [a, c]);
  const one = removeSegment(removeSegment(segs, a), b);
  assert.equal(removeSegment(one, c), one, "the last part cannot be removed");
});

test("trimming a part keeps it within the selection, ≥ 1 s and ≤ 60 s", () => {
  const range = { startMs: 0, endMs: 150_000 };
  const segs = segmentsOf(range);
  const id = segs[1].id;
  assert.deepEqual(spans(trimSegment(segs, id, 62_000, 100_000, range))[1], [62_000, 100_000]);
  // longer than 60 s: the moved edge gives way
  assert.deepEqual(spans(trimSegment(segs, id, 30_000, 120_000, range))[1], [60_000, 120_000].map((v, i) => (i === 0 ? 60_000 : 120_000)));
  assert.deepEqual(spans(trimSegment(segs, id, 50_000, 120_000, range))[1], [60_000, 120_000]);
  // shorter than 1 s
  assert.deepEqual(spans(trimSegment(segs, id, 60_000, 60_200, range))[1], [60_000, 61_000]);
  // outside the selection
  assert.deepEqual(spans(trimSegment(segs, segs[2].id, 120_000, 999_000, range))[2], [120_000, 150_000]);
});

test("clampRange moves only the dragged edge", () => {
  const bounds = { startMs: 0, endMs: 10_000 };
  assert.deepEqual(clampRange({ startMs: 4_800, endMs: 5_000 }, bounds, Infinity, { startMs: 1_000, endMs: 5_000 }), { startMs: 4_000, endMs: 5_000 });
  assert.deepEqual(clampRange({ startMs: 1_000, endMs: 1_200 }, bounds, Infinity, { startMs: 1_000, endMs: 5_000 }), { startMs: 1_000, endMs: 2_000 });
  assert.deepEqual(clampRange({ startMs: -50, endMs: 20_000 }, bounds), { startMs: 0, endMs: 10_000 });
  // a sub-second source can still be selected whole
  assert.deepEqual(clampRange({ startMs: 0, endMs: 400 }, { startMs: 0, endMs: 400 }), { startMs: 0, endMs: 400 });
});

test("the edit reducer re-plans parts when the selection changes", () => {
  let e = initialVideoEdit(130_000);
  assert.equal(e.segments.length, 3);
  assert.equal(editDurationMs(e), 130_000);
  e = videoEditReducer(e, { type: "setRange", startMs: 10_000, endMs: 100_000 });
  assert.deepEqual(spans(e.segments), [[10_000, 70_000], [70_000, 100_000]]);
  e = videoEditReducer(e, { type: "removeSegment", id: e.segments[0].id });
  assert.deepEqual(spans(e.segments), [[70_000, 100_000]]);
  e = videoEditReducer(e, { type: "resetSegments" });
  assert.equal(e.segments.length, 2);
  // with a single part, trimming it trims the selection
  let s = initialVideoEdit(30_000);
  s = videoEditReducer(s, { type: "trimSegment", id: s.segments[0].id, startMs: 2_000, endMs: 20_000 });
  assert.deepEqual(s.range, { startMs: 2_000, endMs: 20_000 });
  assert.deepEqual(spans(s.segments), [[2_000, 20_000]]);
  // no-op actions keep identity
  assert.equal(videoEditReducer(s, { type: "setMuted", muted: false }), s);
  assert.equal(videoEditReducer(s, { type: "setRange", startMs: 2_000, endMs: 20_000 }), s);
});

test("rotate, flip, crop, mute and volume", () => {
  let e = initialVideoEdit(10_000);
  e = videoEditReducer(e, { type: "setCrop", crop: { x: 0, y: 0.25, width: 1, height: 0.5 }, aspect: "16:9" });
  e = videoEditReducer(e, { type: "rotate" });
  assert.equal(e.rotation, 90);
  assert.deepEqual(e.crop, { x: 0.25, y: 0, width: 0.5, height: 1 });
  assert.equal(e.cropAspect, "9:16");
  for (let i = 0; i < 3; i++) e = videoEditReducer(e, { type: "rotate" });
  assert.equal(e.rotation, 0);
  assert.deepEqual(e.crop, { x: 0, y: 0.25, width: 1, height: 0.5 });
  e = videoEditReducer(e, { type: "setCrop", crop: { x: 0.1, y: 0, width: 0.3, height: 1 } });
  e = videoEditReducer(e, { type: "flip" });
  assert.equal(e.flipH, true);
  close(e.crop.x, 0.6);
  e = videoEditReducer(e, { type: "setCrop", crop: { x: 0, y: 0, width: 1, height: 1 } });
  assert.equal(e.crop, null, "a full-frame crop is no crop");
  e = videoEditReducer(e, { type: "setCrop", crop: { x: 0.95, y: -1, width: 0.01, height: 2 } });
  assert.deepEqual(e.crop, { x: 0.9, y: 0, width: 0.1, height: 1 }, "clamped to the frame, min size");
  e = videoEditReducer(e, { type: "setVolume", volume: 5 });
  assert.equal(e.volume, 2);
  e = videoEditReducer(e, { type: "setVolume", volume: 0.333 });
  assert.equal(e.volume, 0.33);
  e = videoEditReducer(e, { type: "setMuted", muted: true });
  assert.equal(e.muted, true);
});

test("undo / redo, with a drag merged into one step", () => {
  let h = createHistory(initialVideoEdit(90_000));
  const start = h.present;
  h = historyReducer(h, { type: "apply", action: { type: "setRange", startMs: 0, endMs: 80_000 }, mergeKey: "drag-1" });
  h = historyReducer(h, { type: "apply", action: { type: "setRange", startMs: 0, endMs: 70_000 }, mergeKey: "drag-1" });
  h = historyReducer(h, { type: "apply", action: { type: "setRange", startMs: 0, endMs: 50_000 }, mergeKey: "drag-1" });
  assert.equal(h.past.length, 1, "one drag = one undo step");
  h = historyReducer(h, { type: "apply", action: { type: "rotate" } });
  h = historyReducer(h, { type: "apply", action: { type: "setMuted", muted: true } });
  assert.equal(h.past.length, 3);
  h = historyReducer(h, { type: "undo" });
  assert.equal(h.present.muted, false);
  h = historyReducer(h, { type: "undo" });
  assert.equal(h.present.rotation, 0);
  h = historyReducer(h, { type: "undo" });
  assert.equal(h.present, start);
  assert.equal(historyReducer(h, { type: "undo" }), h, "nothing more to undo");
  h = historyReducer(h, { type: "redo" });
  assert.equal(h.present.range.endMs, 50_000);
  // a new change clears the redo stack
  h = historyReducer(h, { type: "apply", action: { type: "flip" } });
  assert.equal(h.future.length, 0);
  assert.equal(historyReducer(h, { type: "redo" }), h);
  // a no-op does not add history
  assert.equal(historyReducer(h, { type: "apply", action: { type: "setMuted", muted: false } }), h);
  // a new drag after the same key starts a new step
  const before = h.past.length;
  h = historyReducer(h, { type: "apply", action: { type: "setRange", startMs: 1_000, endMs: 50_000 }, mergeKey: "drag-2" });
  assert.equal(h.past.length, before + 1);
});

test("crop geometry", () => {
  // 9:16 inside a 1920x1080 frame: centred, full height
  const c = aspectCrop(1920, 1080, 9 / 16);
  close(c.height, 1);
  close(c.width * 1920 / (c.height * 1080), 9 / 16);
  close(c.x, (1 - c.width) / 2);
  // 1:1 in portrait
  const sq = aspectCrop(1080, 1920, 1);
  close(sq.width, 1);
  close(sq.height * 1920, 1080);
  // four rotations / two flips are identities
  const r = { x: 0.1, y: 0.2, width: 0.3, height: 0.4 };
  let x = r;
  for (let i = 0; i < 4; i++) x = rotateCropCw(x);
  for (const k of Object.keys(r)) close(x[k], r[k]);
  const f = flipCropH(flipCropH(r));
  for (const k of Object.keys(r)) close(f[k], r[k]);
  // pixels: even sizes inside the frame
  assert.deepEqual(cropToPixels({ x: 0.5, y: 0.5, width: 0.5, height: 0.5 }, 1081, 1921), { left: 541, top: 961, width: 540, height: 960 });
});

test("output plan: ≤ 720p, never upscaled, sensible bitrates", () => {
  const phone = planOutput({ width: 1080, height: 1920, hasAudio: true, sourceVideoBitrate: 16_000_000, sourceAudioBitrate: 192_000, frameRate: 30, videoCodec: "hevc", audioCodec: "aac" });
  assert.deepEqual([phone.width, phone.height], [720, 1280]);
  assert.ok(phone.videoBitrate >= 1_500_000 && phone.videoBitrate <= 2_500_000, String(phone.videoBitrate));
  assert.equal(phone.audioBitrate, 128_000);
  assert.equal(phone.passThrough, false);

  const fourK = planOutput({ width: 3840, height: 2160, hasAudio: false, frameRate: 60 });
  assert.deepEqual([fourK.width, fourK.height], [1280, 720]);
  assert.ok(fourK.videoBitrate <= 2_500_000);
  assert.equal(fourK.audioBitrate, 0);

  // long side capped too (a thin strip crop)
  const strip = planOutput({ width: 1920, height: 1080, crop: { x: 0, y: 0.25, width: 1, height: 0.5 }, hasAudio: false });
  assert.deepEqual([strip.width, strip.height], [1280, 360]);

  // small source: never upscaled; bitrate not above the source's
  const small = planOutput({ width: 480, height: 270, hasAudio: true, sourceVideoBitrate: 400_000, sourceAudioBitrate: 64_000, videoCodec: "vp9" });
  assert.deepEqual([small.width, small.height], [480, 270]);
  assert.ok(small.videoBitrate <= 400_000);
  assert.equal(small.audioBitrate, 96_000);

  // rotation swaps the frame
  const rotated = planOutput({ width: 1920, height: 1080, rotation: 90, hasAudio: false });
  assert.deepEqual([rotated.width, rotated.height], [720, 1280]);

  // odd sizes become even
  const odd = planOutput({ width: 641, height: 359, hasAudio: false });
  assert.equal(odd.width % 2, 0);
  assert.equal(odd.height % 2, 0);
});

test("output plan: unedited small H.264 / AAC sources are copied, anything else re-encoded", () => {
  const src = { width: 640, height: 360, hasAudio: true, sourceVideoBitrate: 700_000, sourceAudioBitrate: 128_000, videoCodec: "avc", audioCodec: "aac" };
  const copy = planOutput(src);
  assert.equal(copy.passThrough, true);
  assert.deepEqual([copy.width, copy.height, copy.videoBitrate, copy.audioBitrate], [640, 360, 700_000, 128_000]);
  assert.equal(planOutput({ ...src, muted: true }).passThrough, true, "muting only drops the audio track");
  assert.equal(planOutput({ ...src, muted: true }).audioBitrate, 0);
  for (const change of [{ rotation: 90 }, { flipH: true }, { crop: { x: 0, y: 0, width: 0.5, height: 1 } }, { volume: 1.5 }, { hasMusic: true }, { hasOverlay: true },
    { videoCodec: "hevc" }, { audioCodec: "opus" }, { sourceVideoBitrate: 9_000_000 }, { width: 1920, height: 1080 }, { sourceVideoBitrate: null }]) {
    assert.equal(planOutput({ ...src, ...change }).passThrough, false, JSON.stringify(change));
  }
  // music on a silent video adds a 128 kbps track
  assert.equal(planOutput({ ...src, hasAudio: false, hasMusic: true }).audioBitrate, 128_000);
});

test("size estimate follows bitrate × duration plus a small MP4 overhead", () => {
  const bytes = estimateOutputSize({ durationMs: 60_000, videoBitrate: 2_000_000, audioBitrate: 128_000 });
  const media = (2_128_000 * 60) / 8;
  assert.ok(bytes > media && bytes < media * 1.01, String(bytes));
  assert.equal(estimateOutputSize({ durationMs: 0, videoBitrate: 2_000_000 }), 0);
  // linear in duration (apart from the fixed header)
  const a = estimateOutputSize({ durationMs: 10_000, videoBitrate: 1_000_000 });
  const b = estimateOutputSize({ durationMs: 20_000, videoBitrate: 1_000_000 });
  assert.ok(Math.abs(b - 2 * a + 4_096) < 2);
});

test("music continues across parts", () => {
  const m = { offsetMs: 2_000, startMs: 10_000, endMs: 100_000 };
  // first part (0-60 s on the posted timeline): music starts 2 s in, from 10 s of the file
  assert.deepEqual(musicWindow(m, 60_000), { atMs: 2_000, fromMs: 10_000, durationMs: 58_000 });
  // second part starts 60 s into the timeline: music resumes from 68 s of the file
  const p2 = musicForPart(m, 60_000);
  assert.deepEqual(musicWindow(p2, 60_000), { atMs: 0, fromMs: 68_000, durationMs: 32_000 });
  // third part: music is over
  assert.equal(musicWindow(musicForPart(m, 120_000), 60_000), null);
});

test("formatting", () => {
  assert.equal(formatDuration(8_000), "0:08");
  assert.equal(formatDuration(65_400), "1:05");
  assert.equal(formatDuration(3_723_000), "1:02:03");
  assert.equal(formatBytes(696_000), "696 kB");
  assert.equal(formatBytes(4_470_000), "4.5 MB");
  assert.equal(formatBytes(16_750_000), "17 MB");
  assert.equal(formatBytes(512), "512 B");
});

test("the engine loads Mediabunny lazily and needs no CSP changes", () => {
  const engine = readFileSync(new URL("../src/lib/media-editor/video.ts", import.meta.url), "utf8");
  assert.doesNotMatch(engine, /^import\s+(?!type\b)[^;]*from\s+"mediabunny"/m, "only type imports at module level");
  assert.match(engine, /import\("mediabunny"\)/);
  const code = engine.replace(/^\s*\/\/.*$/gm, "");
  assert.doesNotMatch(code, /new Worker|WebAssembly|eval\(/);
});
