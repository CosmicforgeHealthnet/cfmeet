// Pure, framework-free parts of the video editor: segment planning, the editable state
// (`VideoEdit`) with its reducer and undo/redo history, crop geometry, output-size planning.
// No browser APIs and no imports, so node tests can load it directly:
//   node --experimental-strip-types --test tests/media-editor-video.test.mjs

/** Longest status video: longer selections are posted as consecutive parts of at most this. */
export const MAX_SEGMENT_MS = 60_000;
/** Shortest part / trim selection. */
export const MIN_SEGMENT_MS = 1_000;
/** Output never exceeds this many pixels on its SHORT side (and 16:9 of it on the long side: "720p"); never upscaled. */
export const DEFAULT_MAX_HEIGHT = 720;
/** Video bitrate bounds (bits per second). */
export const MIN_VIDEO_BITRATE = 300_000;
export const MAX_VIDEO_BITRATE = 2_500_000;
/** Audio bitrates: 128 kbps for good sources / music, 96 kbps otherwise. */
export const HIGH_AUDIO_BITRATE = 128_000;
export const LOW_AUDIO_BITRATE = 96_000;
/** Bits per pixel per frame used to size the video bitrate (H.264, phone footage). */
const BITS_PER_PIXEL = 0.07;
/** A source this close to the target bitrate is not worth re-encoding (pass-through). */
const PASS_THROUGH_BITRATE = MAX_VIDEO_BITRATE * 1.25;

export type Rotation = 0 | 90 | 180 | 270;
export type TimeRange = { startMs: number; endMs: number };
export type Segment = TimeRange & { id: string };

/** A crop rectangle in 0..1 units of the (rotated, flipped) frame. */
export type NormalizedCrop = { x: number; y: number; width: number; height: number };
export type CropAspect = "free" | "original" | "1:1" | "9:16" | "4:5" | "16:9";
export const CROP_ASPECTS: { key: CropAspect; label: string; ratio: number | null }[] = [
  { key: "free", label: "Free", ratio: null },
  { key: "original", label: "Original", ratio: null },
  { key: "9:16", label: "9:16", ratio: 9 / 16 },
  { key: "1:1", label: "1:1", ratio: 1 },
  { key: "4:5", label: "4:5", ratio: 4 / 5 },
  { key: "16:9", label: "16:9", ratio: 16 / 9 },
];

/**
 * Everything the user decided in the video editor. Times are in SOURCE milliseconds.
 * `segments` are the parts to post, in posting order (each ≤ 60 s, inside `range`).
 */
export type VideoEdit = {
  version: 1;
  sourceDurationMs: number;
  /** The overall trim selection on the source timeline. */
  range: TimeRange;
  /** Parts to post, in order. One part when the selection is ≤ 60 s. */
  segments: Segment[];
  /** Crop in the rotated frame, or null for the whole frame. */
  crop: NormalizedCrop | null;
  cropAspect: CropAspect;
  /** Extra clockwise rotation on top of the file's own orientation. */
  rotation: Rotation;
  flipH: boolean;
  muted: boolean;
  /** Original-audio gain, 0..2 (1 = unchanged). */
  volume: number;
};

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));
const round = (v: number) => Math.round(v);

// ---------------------------------------------------------------- segments

/**
 * Consecutive segments of at most `maxMs` covering `[0, durationMs)` (or `[offsetMs, offsetMs +
 * durationMs)`). A tail shorter than `MIN_SEGMENT_MS` is not dropped: the last two parts share
 * the remainder equally, so no media is lost and every part stays ≤ `maxMs`.
 */
export function planSegments(durationMs: number, maxMs = MAX_SEGMENT_MS, offsetMs = 0): TimeRange[] {
  const total = Math.max(0, round(durationMs));
  if (total === 0) return [];
  const max = Math.max(MIN_SEGMENT_MS, round(maxMs));
  const out: TimeRange[] = [];
  for (let start = 0; start < total; start += max) out.push({ startMs: start, endMs: Math.min(total, start + max) });
  const last = out[out.length - 1];
  if (out.length > 1 && last.endMs - last.startMs < MIN_SEGMENT_MS) {
    out.pop();
    const prev = out.pop()!;
    const mid = round((prev.startMs + total) / 2);
    out.push({ startMs: prev.startMs, endMs: mid }, { startMs: mid, endMs: total });
  }
  return out.map((s) => ({ startMs: s.startMs + offsetMs, endMs: s.endMs + offsetMs }));
}

/** Segments of a selection, with stable ids. */
export function segmentsOf(range: TimeRange, maxMs = MAX_SEGMENT_MS): Segment[] {
  return planSegments(range.endMs - range.startMs, maxMs, range.startMs).map((s, i) => ({ ...s, id: `seg-${i}-${s.startMs}` }));
}

/** Removes a segment (never the last remaining one). */
export function removeSegment(segments: Segment[], id: string): Segment[] {
  if (segments.length <= 1) return segments;
  const next = segments.filter((s) => s.id !== id);
  return next.length === segments.length ? segments : next;
}

/** Moves a segment to `toIndex` (clamped) in posting order. */
export function moveSegment(segments: Segment[], id: string, toIndex: number): Segment[] {
  const from = segments.findIndex((s) => s.id === id);
  if (from < 0) return segments;
  const to = clamp(Math.round(toIndex), 0, segments.length - 1);
  if (to === from) return segments;
  const next = segments.slice();
  const [item] = next.splice(from, 1);
  next.splice(to, 0, item);
  return next;
}

/** Trims a segment, keeping it inside `bounds`, at least `MIN_SEGMENT_MS` and at most `maxMs`. */
export function trimSegment(segments: Segment[], id: string, startMs: number, endMs: number, bounds: TimeRange, maxMs = MAX_SEGMENT_MS): Segment[] {
  const seg = segments.find((s) => s.id === id);
  if (!seg) return segments;
  const t = clampRange({ startMs, endMs }, bounds, maxMs, seg);
  if (t.startMs === seg.startMs && t.endMs === seg.endMs) return segments;
  return segments.map((s) => (s.id === id ? { ...s, ...t } : s));
}

/**
 * Clamps a range into `bounds`, with a length of at least `MIN_SEGMENT_MS` and at most `maxMs`.
 * When the requested range violates a length limit, the edge that moved (compared with `prev`)
 * gives way, so dragging one handle never moves the other.
 */
export function clampRange(r: TimeRange, bounds: TimeRange, maxMs = Infinity, prev?: TimeRange): TimeRange {
  const min = Math.min(MIN_SEGMENT_MS, bounds.endMs - bounds.startMs);
  let startMs = clamp(round(r.startMs), bounds.startMs, bounds.endMs);
  let endMs = clamp(round(r.endMs), bounds.startMs, bounds.endMs);
  const startMoved = !prev || startMs !== prev.startMs;
  const endMoved = !prev || endMs !== prev.endMs;
  const fix = (len: number) => {
    if (startMoved && !endMoved) startMs = endMs - len;
    else endMs = startMs + len;
    if (endMs > bounds.endMs) { endMs = bounds.endMs; startMs = endMs - len; }
    if (startMs < bounds.startMs) { startMs = bounds.startMs; endMs = startMs + len; }
  };
  if (endMs - startMs < min) fix(min);
  if (endMs - startMs > maxMs) fix(maxMs);
  return { startMs, endMs };
}

/** Total posted duration of an edit. */
export function editDurationMs(edit: Pick<VideoEdit, "segments">): number {
  return edit.segments.reduce((sum, s) => sum + (s.endMs - s.startMs), 0);
}

// ---------------------------------------------------------------- crop geometry

/** The largest centred crop with `ratio` (width / height) inside a `frameW` x `frameH` frame. */
export function aspectCrop(frameW: number, frameH: number, ratio: number): NormalizedCrop {
  const frame = frameW / frameH;
  if (ratio > frame) {
    const h = frame / ratio;
    return { x: 0, y: (1 - h) / 2, width: 1, height: h };
  }
  const w = ratio / frame;
  return { x: (1 - w) / 2, y: 0, width: w, height: 1 };
}

/** Keeps a crop inside the frame with a minimum size. */
export function clampCrop(c: NormalizedCrop, minSize = 0.1): NormalizedCrop {
  const width = clamp(c.width, minSize, 1);
  const height = clamp(c.height, minSize, 1);
  return { x: clamp(c.x, 0, 1 - width), y: clamp(c.y, 0, 1 - height), width, height };
}

/** The same crop after the frame is rotated 90° clockwise. */
export function rotateCropCw(c: NormalizedCrop): NormalizedCrop {
  return { x: 1 - (c.y + c.height), y: c.x, width: c.height, height: c.width };
}

/** The same crop after the frame is flipped horizontally. */
export function flipCropH(c: NormalizedCrop): NormalizedCrop {
  return { ...c, x: 1 - (c.x + c.width) };
}

/** Frame size after an extra clockwise rotation. */
export function rotatedSize(width: number, height: number, rotation: Rotation): { width: number; height: number } {
  return rotation % 180 === 0 ? { width, height } : { width: height, height: width };
}

/** A normalized crop in pixels of a `width` x `height` frame (integers, even sizes, inside the frame). */
export function cropToPixels(c: NormalizedCrop, width: number, height: number): { left: number; top: number; width: number; height: number } {
  const w = Math.max(2, Math.floor((c.width * width) / 2) * 2);
  const h = Math.max(2, Math.floor((c.height * height) / 2) * 2);
  const left = clamp(Math.round(c.x * width), 0, width - w);
  const top = clamp(Math.round(c.y * height), 0, height - h);
  return { left, top, width: w, height: h };
}

// ---------------------------------------------------------------- output planning

export type OutputPlanInput = {
  /** Display size of the source (its own orientation applied). */
  width: number;
  height: number;
  rotation?: Rotation;
  crop?: NormalizedCrop | null;
  flipH?: boolean;
  /** Average source bitrates in bits per second, if known. */
  sourceVideoBitrate?: number | null;
  sourceAudioBitrate?: number | null;
  frameRate?: number | null;
  hasAudio: boolean;
  /** Source codecs as reported by the demuxer ("avc", "hevc", "aac", ...). */
  videoCodec?: string | null;
  audioCodec?: string | null;
  muted?: boolean;
  volume?: number;
  hasMusic?: boolean;
  hasOverlay?: boolean;
  maxHeight?: number;
};

export type OutputPlan = {
  width: number;
  height: number;
  videoBitrate: number;
  /** 0 when the output has no audio track. */
  audioBitrate: number;
  /** True when the media can be copied as-is (trim only, no re-encode). */
  passThrough: boolean;
};

const even = (v: number) => Math.max(2, Math.round(v / 2) * 2);

/** Output size and bitrates for an export: ≤ 720p short side, never upscaled, ~1.5–2.5 Mbps at 720p. */
export function planOutput(p: OutputPlanInput): OutputPlan {
  const rotation = p.rotation ?? 0;
  const maxHeight = p.maxHeight ?? DEFAULT_MAX_HEIGHT;
  const frame = rotatedSize(p.width, p.height, rotation);
  const cropped = p.crop ? cropToPixels(p.crop, frame.width, frame.height) : frame;
  // 720p: short side ≤ maxHeight and long side ≤ 16:9 of it (1280), whichever binds first.
  const scale = Math.min(1, maxHeight / Math.min(cropped.width, cropped.height), Math.round((maxHeight * 16) / 9) / Math.max(cropped.width, cropped.height));
  const width = even(cropped.width * scale);
  const height = even(cropped.height * scale);
  const fps = clamp(p.frameRate && Number.isFinite(p.frameRate) ? p.frameRate : 30, 15, 30);
  let videoBitrate = clamp(round(width * height * fps * BITS_PER_PIXEL), MIN_VIDEO_BITRATE, MAX_VIDEO_BITRATE);
  if (p.sourceVideoBitrate && Number.isFinite(p.sourceVideoBitrate)) {
    // Re-encoding cannot add detail: never spend more than the (scaled) source did.
    videoBitrate = Math.max(MIN_VIDEO_BITRATE, Math.min(videoBitrate, round(p.sourceVideoBitrate * Math.max(scale * scale, 0.5))));
  }
  const muted = Boolean(p.muted);
  const keepsAudio = (p.hasAudio && !muted) || Boolean(p.hasMusic);
  const audioBitrate = !keepsAudio ? 0
    : p.hasMusic || (p.sourceAudioBitrate ?? 0) >= HIGH_AUDIO_BITRATE ? HIGH_AUDIO_BITRATE : LOW_AUDIO_BITRATE;

  const untouched = !p.crop && rotation === 0 && !p.flipH && !p.hasMusic && !p.hasOverlay && (p.volume ?? 1) === 1;
  const passThrough = untouched
    && scale === 1
    && p.videoCodec === "avc"
    && (!p.hasAudio || muted || p.audioCodec === "aac")
    && Boolean(p.sourceVideoBitrate && p.sourceVideoBitrate <= PASS_THROUGH_BITRATE);
  if (passThrough) {
    return {
      width: p.width,
      height: p.height,
      videoBitrate: round(p.sourceVideoBitrate!),
      audioBitrate: p.hasAudio && !muted ? round(p.sourceAudioBitrate ?? LOW_AUDIO_BITRATE) : 0,
      passThrough: true,
    };
  }
  return { width, height, videoBitrate, audioBitrate, passThrough: false };
}

/** MP4 container overhead: a fixed header plus a small per-second index cost. */
const MP4_FIXED_BYTES = 4_096;
const MP4_BYTES_PER_SECOND = 600;

/** Expected size in bytes of an MP4 with these bitrates and duration. */
export function estimateOutputSize(p: { durationMs: number; videoBitrate: number; audioBitrate?: number }): number {
  const seconds = Math.max(0, p.durationMs) / 1000;
  if (seconds === 0) return 0;
  const media = ((p.videoBitrate + (p.audioBitrate ?? 0)) * seconds) / 8;
  return Math.ceil(media + MP4_FIXED_BYTES + MP4_BYTES_PER_SECOND * seconds);
}

/** "696 kB", "12.4 MB" (decimal units, as phones show). */
export function formatBytes(bytes: number): string {
  if (bytes < 1000) return `${Math.max(0, Math.round(bytes))} B`;
  if (bytes < 1_000_000) return `${Math.round(bytes / 1000)} kB`;
  return `${(bytes / 1_000_000).toFixed(bytes < 10_000_000 ? 1 : 0)} MB`;
}

/** "0:08", "1:05", "1:02:03". */
export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = String(total % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

// ---------------------------------------------------------------- edit state

/** The starting edit for a source of `durationMs`: everything selected, no changes. */
export function initialVideoEdit(durationMs: number): VideoEdit {
  const range = { startMs: 0, endMs: Math.max(0, round(durationMs)) };
  return { version: 1, sourceDurationMs: range.endMs, range, segments: segmentsOf(range), crop: null, cropAspect: "free", rotation: 0, flipH: false, muted: false, volume: 1 };
}

export type VideoEditAction =
  | { type: "setRange"; startMs: number; endMs: number }
  | { type: "trimSegment"; id: string; startMs: number; endMs: number }
  | { type: "removeSegment"; id: string }
  | { type: "moveSegment"; id: string; toIndex: number }
  | { type: "resetSegments" }
  | { type: "setCrop"; crop: NormalizedCrop | null; aspect?: CropAspect }
  | { type: "rotate" }
  | { type: "flip" }
  | { type: "setMuted"; muted: boolean }
  | { type: "setVolume"; volume: number };

/** Applies one action; returns the same object when nothing changed. */
export function videoEditReducer(edit: VideoEdit, action: VideoEditAction): VideoEdit {
  const bounds = { startMs: 0, endMs: edit.sourceDurationMs };
  switch (action.type) {
    case "setRange": {
      const range = clampRange(action, bounds, Infinity, edit.range);
      if (range.startMs === edit.range.startMs && range.endMs === edit.range.endMs) return edit;
      return { ...edit, range, segments: segmentsOf(range) };
    }
    case "trimSegment": {
      if (edit.segments.length === 1) return videoEditReducer(edit, { type: "setRange", startMs: action.startMs, endMs: action.endMs });
      const segments = trimSegment(edit.segments, action.id, action.startMs, action.endMs, edit.range);
      return segments === edit.segments ? edit : { ...edit, segments };
    }
    case "removeSegment": {
      const segments = removeSegment(edit.segments, action.id);
      return segments === edit.segments ? edit : { ...edit, segments };
    }
    case "moveSegment": {
      const segments = moveSegment(edit.segments, action.id, action.toIndex);
      return segments === edit.segments ? edit : { ...edit, segments };
    }
    case "resetSegments":
      return { ...edit, segments: segmentsOf(edit.range) };
    case "setCrop": {
      const crop = action.crop ? clampCrop(action.crop) : null;
      const full = crop && crop.x <= 0.001 && crop.y <= 0.001 && crop.width >= 0.999 && crop.height >= 0.999;
      return { ...edit, crop: full ? null : crop, cropAspect: action.aspect ?? edit.cropAspect };
    }
    case "rotate": {
      const swapped: Partial<Record<CropAspect, CropAspect>> = { "9:16": "16:9", "16:9": "9:16", "4:5": "free" };
      return {
        ...edit,
        rotation: ((edit.rotation + 90) % 360) as Rotation,
        crop: edit.crop ? rotateCropCw(edit.crop) : null,
        cropAspect: swapped[edit.cropAspect] ?? edit.cropAspect,
      };
    }
    case "flip":
      return { ...edit, flipH: !edit.flipH, crop: edit.crop ? flipCropH(edit.crop) : null };
    case "setMuted":
      return action.muted === edit.muted ? edit : { ...edit, muted: action.muted };
    case "setVolume": {
      const volume = Math.round(clamp(action.volume, 0, 2) * 100) / 100;
      return volume === edit.volume ? edit : { ...edit, volume };
    }
  }
}

/** Undo / redo history of edits. `mergeKey` groups a drag into one undo step. */
export type EditHistory = { past: VideoEdit[]; present: VideoEdit; future: VideoEdit[]; mergeKey: string | null };
export type HistoryAction =
  | { type: "apply"; action: VideoEditAction; mergeKey?: string }
  | { type: "undo" }
  | { type: "redo" }
  | { type: "reset"; edit: VideoEdit };

export const HISTORY_LIMIT = 100;

export function createHistory(edit: VideoEdit): EditHistory {
  return { past: [], present: edit, future: [], mergeKey: null };
}

export function historyReducer(h: EditHistory, a: HistoryAction): EditHistory {
  switch (a.type) {
    case "apply": {
      const next = videoEditReducer(h.present, a.action);
      if (next === h.present) return h;
      const mergeKey = a.mergeKey ?? null;
      if (mergeKey !== null && mergeKey === h.mergeKey) return { ...h, present: next, future: [] };
      return { past: h.past.concat([h.present]).slice(-HISTORY_LIMIT), present: next, future: [], mergeKey };
    }
    case "undo": {
      if (!h.past.length) return h;
      return { past: h.past.slice(0, -1), present: h.past[h.past.length - 1], future: [h.present].concat(h.future), mergeKey: null };
    }
    case "redo": {
      if (!h.future.length) return h;
      return { past: h.past.concat([h.present]), present: h.future[0], future: h.future.slice(1), mergeKey: null };
    }
    case "reset":
      return createHistory(a.edit);
  }
}

// ---------------------------------------------------------------- music timeline

export type MusicPlacement = {
  /** Where in the CLIP (ms from the clip's first frame) music-file time `startMs` plays. May be negative. */
  offsetMs: number;
  /** The part of the music file to use. */
  startMs: number;
  endMs: number;
};

/**
 * The music placement for one exported part, given the placement on the whole posted timeline
 * (all parts back to back) and where the part starts on that timeline. Music therefore flows on
 * across parts instead of restarting in each.
 */
export function musicForPart(m: MusicPlacement, partStartOnTimelineMs: number): MusicPlacement {
  return { ...m, offsetMs: m.offsetMs - partStartOnTimelineMs };
}

/**
 * The window of the music file heard in a clip of `clipMs`: when it starts in the clip, which
 * music time is heard first and for how long. Null when no music is heard in this clip.
 */
export function musicWindow(m: MusicPlacement, clipMs: number): { atMs: number; fromMs: number; durationMs: number } | null {
  const atMs = Math.max(0, m.offsetMs);
  const fromMs = m.startMs + (atMs - m.offsetMs);
  const durationMs = Math.min(clipMs - atMs, m.endMs - fromMs);
  return durationMs > 0 ? { atMs, fromMs, durationMs } : null;
}
