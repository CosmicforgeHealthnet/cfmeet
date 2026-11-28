// Status creation (music, voice, video overlays, segments, recent media): the pure rules behind
// the creation screen and editors. Real decoding / encoding runs only in a browser.
//   node --experimental-strip-types --test tests/status-creation.test.mjs
import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";

register("./helpers/ts-resolve.mjs", import.meta.url);
const audio = await import("../src/lib/media-editor/audio.ts");
const vo = await import("../src/lib/media-editor/video-overlays.ts");
const ov = await import("../src/lib/media-editor/overlays.ts");
const ve = await import("../src/lib/media-editor/video-edit.ts");
const lib = await import("../src/lib/media-editor/music-library.ts");
const recent = await import("../src/lib/media-editor/recent-media.ts");

const close = (a, b, eps = 1e-6) => assert.ok(Math.abs(a - b) <= eps, `${a} ≉ ${b}`);
const measure = (text, font) => [...text].length * parseFloat(/(\d+(?:\.\d+)?)px/.exec(font)[1]) * 0.5;

// ------------------------------------------------------------------ audio mixing

test("mixPlan: original and music gains are independent; muting or no source track drops the original", () => {
  assert.deepEqual(audio.mixPlan({ hasOriginalAudio: true, muted: false, originalVolume: 1 }), { original: 1, music: 0, hasAudio: true, transcodeOriginal: false, musicOnly: false });
  const mixed = audio.mixPlan({ hasOriginalAudio: true, muted: false, originalVolume: 0.4, music: { volume: 1.5 } });
  assert.equal(mixed.original, 0.4);
  assert.equal(mixed.music, 1.5);
  assert.equal(mixed.transcodeOriginal, true);
  assert.equal(mixed.musicOnly, false);
  // Music alone when the original is muted, silent or missing.
  for (const p of [{ hasOriginalAudio: true, muted: true, originalVolume: 1 }, { hasOriginalAudio: true, muted: false, originalVolume: 0 }, { hasOriginalAudio: false, muted: false, originalVolume: 1 }]) {
    const plan = audio.mixPlan({ ...p, music: { volume: 1 } });
    assert.equal(plan.original, 0);
    assert.equal(plan.musicOnly, true);
    assert.equal(plan.hasAudio, true);
  }
  assert.equal(audio.mixPlan({ hasOriginalAudio: true, muted: true, originalVolume: 1 }).hasAudio, false);
  // Gains clamp to 0..2.
  assert.equal(audio.mixPlan({ hasOriginalAudio: true, muted: false, originalVolume: 9, music: { volume: -1 } }).original, 2);
  assert.equal(audio.mixPlan({ hasOriginalAudio: true, muted: false, originalVolume: 9, music: { volume: -1 } }).music, 0);
});

test("fadeGain ramps in and out over the music range and is silent outside it", () => {
  const m = { startMs: 10_000, endMs: 40_000, volume: 0.8, fadeInMs: 2_000, fadeOutMs: 4_000 };
  assert.equal(audio.fadeGain(9_999, m), 0);
  assert.equal(audio.fadeGain(10_000, m), 0);
  close(audio.fadeGain(11_000, m), 0.4);
  close(audio.fadeGain(20_000, m), 0.8);
  close(audio.fadeGain(38_000, m), 0.4);
  assert.equal(audio.fadeGain(40_000, m), 0);
  assert.equal(audio.fadeGain(40_001, m), 0);
  const curve = audio.gainCurve(m, 10_000, 30_000, 31);
  assert.equal(curve.length, 31);
  close(curve[0], 0);
  close(curve[15], 0.8, 1e-6);
  close(curve[30], 0);
});

test("mixPlanar scales the original, adds the (mono) music from its offset and clips", () => {
  const frames = 4;
  const data = new Float32Array([0.5, 0.5, 0.5, 0.5, -0.5, -0.5, -0.5, -0.5]); // 2 planes
  const music = [new Float32Array([0.9, 0.9, 0.1])];
  audio.mixPlanar(data, frames, 2, 0.5, music, 1);
  // music sample j = first + i: i=0 → 0.9, i=1 → 0.1, i=2/3 → beyond the music.
  assert.deepEqual([...data].map((v) => Math.round(v * 100) / 100), [1, 0.35, 0.25, 0.25, 0.65, -0.15, -0.25, -0.25]);
  const loud = new Float32Array([0.9, -0.9]);
  audio.mixPlanar(loud, 2, 1, 2, [new Float32Array([0.5, -0.5])], 0);
  assert.deepEqual([...loud], [1, -1]);
});

test("fades fit the clip: each at most 5 s, together never longer than the clip", () => {
  assert.deepEqual(audio.clampFades(9_000, 1_000, 30_000), { fadeInMs: 5_000, fadeOutMs: 1_000 });
  assert.deepEqual(audio.clampFades(3_000, 3_000, 4_000), { fadeInMs: 2_000, fadeOutMs: 2_000 });
  assert.deepEqual(audio.clampFades(-5, Number.NaN, 4_000), { fadeInMs: 0, fadeOutMs: 0 });
  const d = audio.defaultMusicSettings(200_000);
  assert.deepEqual([d.startMs, d.endMs, d.volume], [0, 30_000, 1]);
  assert.deepEqual(audio.defaultMusicSettings(12_000, 30_000).endMs, 12_000);
});

// ------------------------------------------------------------------ music selection on the timeline

test("music selection: clamped to the track, 1 s to 60 s, the dragged end gives way", () => {
  assert.deepEqual(audio.clampSelection({ startMs: -5, endMs: 90_000 }, 200_000), { startMs: 0, endMs: 60_000 });
  // Dragging the start past the 60 s limit pulls nothing: the start gives way.
  assert.deepEqual(audio.clampSelection({ startMs: 10_000, endMs: 100_000 }, 200_000, 60_000, { startMs: 40_000, endMs: 100_000 }), { startMs: 40_000, endMs: 100_000 });
  assert.deepEqual(audio.clampSelection({ startMs: 50_000, endMs: 50_200 }, 200_000), { startMs: 50_000, endMs: 51_000 });
  assert.deepEqual(audio.clampSelection({ startMs: 0, endMs: 9_000 }, 600), { startMs: 0, endMs: 600 });
  assert.deepEqual(audio.moveSelection({ startMs: 10_000, endMs: 40_000 }, 500_000, 100_000), { startMs: 70_000, endMs: 100_000 });
  assert.deepEqual(audio.moveSelection({ startMs: 10_000, endMs: 40_000 }, -50_000, 100_000), { startMs: 0, endMs: 30_000 });
  assert.deepEqual(audio.resizeSelection({ startMs: 90_000, endMs: 95_000 }, 30_000, 100_000), { startMs: 70_000, endMs: 100_000 });
  assert.deepEqual(audio.resizeSelection({ startMs: 0, endMs: 5_000 }, 99_000, 100_000), { startMs: 0, endMs: 60_000 });
});

// ------------------------------------------------------------------ waveform data

test("waveformPeaks: loudest sample per bucket over every channel, scaled to the loudest", () => {
  const left = new Float32Array(100);
  const right = new Float32Array(100);
  left[5] = 0.25; right[30] = -0.5; left[75] = 0.1;
  const peaks = audio.waveformPeaks([left, right], 4);
  assert.deepEqual(peaks, [0.5, 1, 0, 0.2]);
  assert.deepEqual(audio.waveformPeaks([new Float32Array(50)], 5), [0, 0, 0, 0, 0]);
  assert.deepEqual(audio.waveformPeaks([], 3), [0, 0, 0]);
  // More buckets than samples: still one value per bar.
  assert.equal(audio.waveformPeaks([new Float32Array([1, 0.5])], 8).length, 8);
});

test("live level from analyser bytes, rolling live waveform, peaks of a range", () => {
  assert.equal(audio.liveLevel(new Uint8Array(32).fill(128)), 0);
  assert.equal(audio.liveLevel(new Uint8Array(32).fill(255)), 1);
  const quiet = audio.liveLevel(Uint8Array.from({ length: 32 }, (_, i) => (i % 2 ? 136 : 120)));
  assert.ok(quiet > 0 && quiet < 1);
  let bars = [];
  for (let i = 0; i < 70; i++) bars = audio.pushLevel(bars, i / 100, 64);
  assert.equal(bars.length, 64);
  close(bars[63], 0.69);
  assert.deepEqual(audio.slicePeaks([0, 1, 0.5, 0.25], { startMs: 500, endMs: 1000 }, 1000, 2), [0.5, 0.25]);
});

// ------------------------------------------------------------------ encoding

test("WAV: a valid 16-bit PCM header and samples; the fallback rate keeps a minute under 8 MiB", () => {
  const wav = audio.encodeWav([new Float32Array([0, 1, -1])], 16_000);
  const v = new DataView(wav.buffer);
  assert.equal(String.fromCharCode(...wav.slice(0, 4)), "RIFF");
  assert.equal(String.fromCharCode(...wav.slice(8, 12)), "WAVE");
  assert.equal(v.getUint16(22, true), 1);
  assert.equal(v.getUint32(24, true), 16_000);
  assert.equal(v.getUint32(40, true), 6);
  assert.deepEqual([v.getInt16(44, true), v.getInt16(46, true), v.getInt16(48, true)], [0, 32767, -32768]);
  assert.equal(wav.length, 50);
  const limit = 8 * 1024 * 1024;
  const rate = audio.wavSampleRateFor(60_000, limit);
  assert.equal(rate, 44_100);
  assert.ok(audio.wavBytes(60_000, rate) <= limit);
  assert.equal(audio.wavSampleRateFor(60_000, 3_000_000), 24_000);
  assert.deepEqual([...audio.downmix([new Float32Array([1, 0]), new Float32Array([0, 0])])], [0.5, 0]);
});

test("track titles and clock text", () => {
  assert.equal(audio.trackTitle("my_song-final.MP3"), "my song-final");
  assert.equal(audio.trackTitle(".mp3"), "My audio");
  assert.equal(audio.formatClock(61_500), "1:01");
  assert.equal(audio.formatClock(-4), "0:00");
});

// ------------------------------------------------------------------ "My music" library (device only)

test("library: least recently used tracks are pruned past 15 tracks or the byte budget; search ignores case and accents", () => {
  const tracks = Array.from({ length: 18 }, (_, i) => ({ id: `t${i}`, sizeBytes: 10, lastUsedAt: i }));
  assert.deepEqual(lib.tracksToPrune(tracks).sort(), ["t0", "t1", "t2"]);
  assert.deepEqual(lib.tracksToPrune([{ id: "a", sizeBytes: 60, lastUsedAt: 3 }, { id: "b", sizeBytes: 60, lastUsedAt: 2 }, { id: "c", sizeBytes: 30, lastUsedAt: 1 }], 15, 100), ["b"]);
  // The newest is always kept, even alone over the budget.
  assert.deepEqual(lib.tracksToPrune([{ id: "big", sizeBytes: 500, lastUsedAt: 9 }], 15, 100), []);
  const found = lib.searchTracks([{ title: "Café del Mar", lastUsedAt: 1 }, { title: "Morning walk", lastUsedAt: 5 }, { title: "cafe beats", lastUsedAt: 3 }], "CAFE");
  assert.deepEqual(found.map((t) => t.title), ["cafe beats", "Café del Mar"]);
  assert.equal(lib.searchTracks([{ title: "x", lastUsedAt: 1 }], "").length, 1);
  assert.equal(lib.trackKey("a", 3, "audio/mpeg"), lib.trackKey("a", 3, "audio/mpeg"));
});

// ------------------------------------------------------------------ recent media

test("recent media: photos and videos only (by type or name), newest first, the same file once", () => {
  assert.equal(recent.recentKindOf({ name: "a.HEIC", type: "" }), "IMAGE");
  assert.equal(recent.recentKindOf({ name: "clip.mov", type: "" }), "VIDEO");
  assert.equal(recent.recentKindOf({ name: "x.mp4", type: "video/mp4" }), "VIDEO");
  assert.equal(recent.recentKindOf({ name: "notes.txt", type: "text/plain" }), null);
  const f = (name, lastModified, type = "") => ({ name, size: 10, lastModified, type });
  const list = recent.newestMedia([f("old.jpg", 1), f("doc.pdf", 9), f("new.mp4", 5), f("new.mp4", 5), f("mid.png", 3)], 10);
  assert.deepEqual(list.map((x) => x.name), ["new.mp4", "mid.png", "old.jpg"]);
  assert.equal(recent.newestMedia([f("a.jpg", 1), f("b.jpg", 2)], 1)[0].name, "b.jpg");
});

// ------------------------------------------------------------------ segments: reorder and trim

test("segments of a long video: 60 s parts, reorder, remove and trim keep order, bounds and undo", () => {
  let h = ve.createHistory(ve.initialVideoEdit(150_000));
  const ids = h.present.segments.map((s) => s.id);
  assert.deepEqual(h.present.segments.map((s) => [s.startMs, s.endMs]), [[0, 60_000], [60_000, 120_000], [120_000, 150_000]]);
  h = ve.historyReducer(h, { type: "apply", action: { type: "moveSegment", id: ids[2], toIndex: 0 } });
  assert.deepEqual(h.present.segments.map((s) => s.id), [ids[2], ids[0], ids[1]]);
  h = ve.historyReducer(h, { type: "apply", action: { type: "trimSegment", id: ids[0], startMs: 5_000, endMs: 70_000 } });
  const trimmed = h.present.segments.find((s) => s.id === ids[0]);
  assert.deepEqual([trimmed.startMs, trimmed.endMs], [5_000, 65_000]); // at most 60 s: the moved end gives way
  h = ve.historyReducer(h, { type: "apply", action: { type: "removeSegment", id: ids[1] } });
  assert.deepEqual(h.present.segments.map((s) => s.id), [ids[2], ids[0]]);
  assert.equal(ve.editDurationMs(h.present), 30_000 + 60_000);
  h = ve.historyReducer(h, { type: "undo" });
  h = ve.historyReducer(h, { type: "undo" });
  assert.deepEqual(h.present.segments.map((s) => s.id), [ids[2], ids[0], ids[1]]);
  // Music flows across the parts in posting order.
  const m = { offsetMs: 0, startMs: 10_000, endMs: 100_000 };
  assert.deepEqual(ve.musicWindow(ve.musicForPart(m, 30_000), 60_000), { atMs: 0, fromMs: 40_000, durationMs: 60_000 });
});

// ------------------------------------------------------------------ overlay render pipeline

/** A 2D context that records what is drawn. */
function recordingCtx() {
  const calls = [];
  const ctx = new Proxy({ calls }, {
    get(target, key) {
      if (key in target) return target[key];
      return (...args) => { calls.push([key, ...args]); };
    },
    set(target, key, value) { calls.push([`set:${String(key)}`, value]); return true; },
  });
  return ctx;
}

test("video overlays: nothing visible means no drawer (the video can still be copied untouched)", () => {
  assert.equal(vo.overlayFrameDrawer([]), null);
  assert.equal(vo.overlayFrameDrawer(null), null);
  assert.equal(vo.overlayFrameDrawer([ov.createText({ text: "   " }), { id: "s", type: "stroke", color: "#fff", width: 0.01, points: [0.1] }]), null);
  assert.equal(vo.hasVisibleOverlays([ov.createSticker("🎉")]), true);
});

test("video overlays: the layer is painted once per output size with drawOverlays, then blitted on every frame", () => {
  const overlays = [ov.createText({ text: "Hello", x: 0.5, y: 0.3 }), ov.createSticker("🔥", { x: 0.2, y: 0.8 }), { id: "d", type: "stroke", color: "#ef4444", width: 0.02, points: [0.1, 0.1, 0.5, 0.5, 0.9, 0.2] }];
  const layers = [];
  const painted = [];
  const draw = (ctx, list, w, h, m) => { painted.push([list.length, w, h]); ov.drawOverlays(ctx, list, w, h, m); };
  const drawer = vo.overlayFrameDrawer(overlays, {
    measure,
    draw,
    makeLayer: (w, h) => { const ctx = recordingCtx(); const canvas = { w, h, id: layers.length }; layers.push({ canvas, ctx }); return { canvas, ctx }; },
  });
  assert.ok(drawer);
  const frame = recordingCtx();
  for (let i = 0; i < 5; i++) drawer(frame, 1280, 720);
  assert.equal(layers.length, 1, "painted once for five frames");
  assert.deepEqual(painted, [[3, 1280, 720]]);
  const blits = frame.calls.filter((c) => c[0] === "drawImage");
  assert.equal(blits.length, 5);
  assert.deepEqual(blits[0].slice(1), [layers[0].canvas, 0, 0, 1280, 720]);
  // The layer got the text, the sticker and the stroke (real drawOverlays).
  const texts = layers[0].ctx.calls.filter((c) => c[0] === "fillText").map((c) => c[1]);
  assert.ok(texts.includes("Hello"));
  assert.ok(texts.includes("🔥"));
  assert.ok(layers[0].ctx.calls.some((c) => c[0] === "stroke"));
  // The sticker is placed at its normalised position in the output frame.
  assert.ok(layers[0].ctx.calls.some((c) => c[0] === "translate" && Math.abs(c[1] - 256) < 1e-6 && Math.abs(c[2] - 576) < 1e-6));
  // A different output size (another part with another crop) repaints once.
  drawer(frame, 720, 720);
  drawer(frame, 720, 720);
  assert.equal(layers.length, 2);
  // Editing the overlays afterwards does not change a drawer already handed to the export.
  overlays.push(ov.createText({ text: "late" }));
  drawer(frame, 640, 360);
  assert.deepEqual(painted.at(-1), [3, 640, 360]);
});
