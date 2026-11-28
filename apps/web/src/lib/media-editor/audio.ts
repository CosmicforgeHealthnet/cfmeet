// Pure, framework-free audio helpers for status music and voice: waveform peaks, the music
// selection on an audio timeline (move / resize / clamp), fade envelopes, the original-audio +
// music mix (gains, clipping), voice trimming and WAV encoding. No browser APIs and no imports,
// so node tests load it directly:
//   node --experimental-strip-types --test tests/status-creation.test.mjs
// Decoding, rendering and encoding in the browser live in ./audio-render.ts.

/** Longest status audio (voice, music clip, music on a photo / text status). */
export const MAX_AUDIO_MS = 60_000;
/** Shortest music selection / voice trim. */
export const MIN_AUDIO_MS = 1_000;
/** Default music length on a photo / text / music status. */
export const DEFAULT_MUSIC_MS = 30_000;
/** Longest fade at either end. */
export const MAX_FADE_MS = 5_000;
/** Bars drawn for a waveform. */
export const WAVEFORM_BARS = 64;

export type AudioRange = { startMs: number; endMs: number };

/** What the user chose for music: a range of their own audio file, its gain and fades. */
export type MusicSettings = AudioRange & {
  /** Music gain 0..2 (1 = unchanged). */
  volume: number;
  fadeInMs: number;
  fadeOutMs: number;
};

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

// ---------------------------------------------------------------- waveform

/**
 * `buckets` peak levels (0..1) across the samples of one or more channels: the loudest absolute
 * sample of each bucket, over every channel, scaled so the loudest bucket is 1 (quiet clips
 * still show a shape; silence stays flat at 0).
 */
export function waveformPeaks(channels: readonly ArrayLike<number>[], buckets = WAVEFORM_BARS): number[] {
  const n = Math.max(1, Math.round(buckets));
  const length = channels.reduce((m, c) => Math.max(m, c.length), 0);
  const out = new Array<number>(n).fill(0);
  if (!length) return out;
  for (let b = 0; b < n; b++) {
    const from = Math.floor((b * length) / n);
    const to = Math.max(from + 1, Math.floor(((b + 1) * length) / n));
    let peak = 0;
    for (const c of channels) {
      // Long buckets are sampled (every k-th sample): peaks stay visible, cost stays flat.
      const step = Math.max(1, Math.floor((to - from) / 2048));
      for (let i = from; i < to && i < c.length; i += step) {
        const v = Math.abs(c[i]);
        if (v > peak) peak = v;
      }
    }
    out[b] = peak;
  }
  const max = Math.max(...out);
  if (max < 1e-4) return out.map(() => 0);
  return out.map((v) => Math.round((v / max) * 1000) / 1000);
}

/** The level (0..1) of an AnalyserNode's byte time-domain data: RMS around the 128 midpoint, boosted for speech. */
export function liveLevel(timeDomain: ArrayLike<number>): number {
  if (!timeDomain.length) return 0;
  let sum = 0;
  for (let i = 0; i < timeDomain.length; i++) {
    const v = (timeDomain[i] - 128) / 128;
    sum += v * v;
  }
  return clamp(Math.sqrt(sum / timeDomain.length) * 3, 0, 1);
}

/** Appends a level to a rolling live waveform of at most `max` bars. */
export function pushLevel(levels: readonly number[], level: number, max = WAVEFORM_BARS): number[] {
  const next = levels.length >= max ? levels.slice(levels.length - max + 1) : levels.slice();
  next.push(clamp(level, 0, 1));
  return next;
}

/** Peaks of the part of a waveform inside `range` (of a clip `totalMs` long), resampled to `bars`. */
export function slicePeaks(peaks: readonly number[], range: AudioRange, totalMs: number, bars = peaks.length): number[] {
  if (!peaks.length || totalMs <= 0) return new Array<number>(Math.max(0, bars)).fill(0);
  const a = clamp(range.startMs / totalMs, 0, 1) * peaks.length;
  const b = clamp(range.endMs / totalMs, 0, 1) * peaks.length;
  const out: number[] = [];
  for (let i = 0; i < bars; i++) {
    const from = Math.floor(a + ((b - a) * i) / bars);
    const to = Math.max(from + 1, Math.floor(a + ((b - a) * (i + 1)) / bars));
    let m = 0;
    for (let j = from; j < to && j < peaks.length; j++) m = Math.max(m, peaks[j]);
    out.push(m);
  }
  return out;
}

// ---------------------------------------------------------------- selection on the audio timeline

/**
 * Keeps a selection inside a track of `trackMs`, at least `minMs` (or the whole track when it is
 * shorter) and at most `maxMs` long. When a limit is broken the end that moved (compared with
 * `prev`) gives way, so dragging one handle never moves the other.
 */
export function clampSelection(r: AudioRange, trackMs: number, maxMs = MAX_AUDIO_MS, prev?: AudioRange, minMs = MIN_AUDIO_MS): AudioRange {
  const total = Math.max(0, Math.round(trackMs));
  const min = Math.min(minMs, total);
  const max = Math.max(min, Math.min(maxMs, total));
  let startMs = clamp(Math.round(r.startMs), 0, total);
  let endMs = clamp(Math.round(r.endMs), 0, total);
  if (endMs < startMs) [startMs, endMs] = [endMs, startMs];
  const startMoved = !prev || startMs !== prev.startMs;
  const endMoved = !prev || endMs !== prev.endMs;
  const fix = (len: number) => {
    if (startMoved && !endMoved) startMs = endMs - len;
    else endMs = startMs + len;
    if (endMs > total) { endMs = total; startMs = endMs - len; }
    if (startMs < 0) { startMs = 0; endMs = len; }
  };
  if (endMs - startMs < min) fix(min);
  if (endMs - startMs > max) fix(max);
  return { startMs, endMs };
}

/** The first `durationMs` of a track (the default selection). */
export function initialSelection(trackMs: number, durationMs = DEFAULT_MUSIC_MS): AudioRange {
  return clampSelection({ startMs: 0, endMs: durationMs }, trackMs);
}

/** Slides a selection by `deltaMs`, keeping its length, inside the track. */
export function moveSelection(r: AudioRange, deltaMs: number, trackMs: number): AudioRange {
  const len = r.endMs - r.startMs;
  const startMs = clamp(Math.round(r.startMs + deltaMs), 0, Math.max(0, trackMs - len));
  return { startMs, endMs: startMs + len };
}

/** Changes a selection's length from its start (shifting back when it would run past the end). */
export function resizeSelection(r: AudioRange, durationMs: number, trackMs: number, maxMs = MAX_AUDIO_MS): AudioRange {
  const len = clamp(Math.round(durationMs), Math.min(MIN_AUDIO_MS, trackMs), Math.min(maxMs, trackMs));
  const startMs = clamp(r.startMs, 0, Math.max(0, trackMs - len));
  return { startMs, endMs: startMs + len };
}

/** Fades that fit a clip of `lengthMs`: each ≤ MAX_FADE_MS and together ≤ the clip. */
export function clampFades(fadeInMs: number, fadeOutMs: number, lengthMs: number): { fadeInMs: number; fadeOutMs: number } {
  const len = Math.max(0, lengthMs);
  let i = clamp(Math.round(fadeInMs) || 0, 0, MAX_FADE_MS);
  let o = clamp(Math.round(fadeOutMs) || 0, 0, MAX_FADE_MS);
  if (i + o > len) {
    const k = len / (i + o);
    i = Math.floor(i * k);
    o = Math.floor(o * k);
  }
  return { fadeInMs: i, fadeOutMs: o };
}

/** Default music settings for a track: the first 30 s (or the clip length), full volume, short fades. */
export function defaultMusicSettings(trackMs: number, durationMs = DEFAULT_MUSIC_MS): MusicSettings {
  const r = initialSelection(trackMs, durationMs);
  return { ...r, volume: 1, ...clampFades(500, 1000, r.endMs - r.startMs) };
}

// ---------------------------------------------------------------- gain / fades / mixing

/**
 * The music gain at music-file time `musicMs`: `volume`, ramped linearly from 0 over `fadeInMs`
 * after `startMs` and down to 0 over `fadeOutMs` before `endMs`; 0 outside the range.
 */
export function fadeGain(musicMs: number, m: MusicSettings): number {
  if (musicMs < m.startMs || musicMs > m.endMs) return 0;
  let g = clamp(m.volume, 0, 2);
  if (m.fadeInMs > 0) g *= clamp((musicMs - m.startMs) / m.fadeInMs, 0, 1);
  if (m.fadeOutMs > 0) g *= clamp((m.endMs - musicMs) / m.fadeOutMs, 0, 1);
  return g;
}

/** `points` gain values sampled evenly over `[fromMs, fromMs + durationMs]` of music time (for setValueCurveAtTime). */
export function gainCurve(m: MusicSettings, fromMs: number, durationMs: number, points = 256): Float32Array {
  const n = Math.max(2, Math.round(points));
  const out = new Float32Array(n);
  for (let i = 0; i < n; i++) out[i] = fadeGain(fromMs + (durationMs * i) / (n - 1), m);
  return out;
}

export type MixInput = {
  /** The source has an audio track this browser can decode. */
  hasOriginalAudio: boolean;
  muted: boolean;
  /** Original-audio gain 0..2. */
  originalVolume: number;
  /** Music is attached (null / undefined: none). */
  music?: Pick<MusicSettings, "volume"> | null;
};

export type MixPlan = {
  /** Gain applied to the original audio (0: dropped). */
  original: number;
  /** Music gain (0: no music). */
  music: number;
  /** The output has an audio track at all. */
  hasAudio: boolean;
  /** The original audio must be decoded and re-encoded (volume change or mixing). */
  transcodeOriginal: boolean;
  /** Only music is heard (the original is muted, silent or absent). */
  musicOnly: boolean;
};

/** How the original audio and the music combine in an export. */
export function mixPlan(p: MixInput): MixPlan {
  const original = p.hasOriginalAudio && !p.muted ? Math.round(clamp(p.originalVolume, 0, 2) * 100) / 100 : 0;
  const music = p.music ? Math.round(clamp(p.music.volume, 0, 2) * 100) / 100 : 0;
  return {
    original,
    music,
    hasAudio: original > 0 || music > 0,
    transcodeOriginal: original > 0 && (original !== 1 || music > 0),
    musicOnly: music > 0 && original === 0,
  };
}

/**
 * Mixes planar float audio in place: `data` holds `channels` planes of `frames` samples; each is
 * multiplied by `volume` and the music (planar channels, already gain-shaped) is added from
 * music sample `first` on, then clipped to [-1, 1]. Mono music feeds every channel.
 */
export function mixPlanar(data: Float32Array, frames: number, channels: number, volume: number, music: readonly ArrayLike<number>[], first: number): Float32Array {
  for (let c = 0; c < channels; c++) {
    const mc = music.length ? music[Math.min(c, music.length - 1)] : null;
    const base = c * frames;
    for (let i = 0; i < frames; i++) {
      let v = data[base + i] * volume;
      if (mc) {
        const j = first + i;
        if (j >= 0 && j < mc.length) v += mc[j];
      }
      data[base + i] = v > 1 ? 1 : v < -1 ? -1 : v;
    }
  }
  return data;
}

// ---------------------------------------------------------------- encoding

/** Averages channels into one. */
export function downmix(channels: readonly Float32Array[]): Float32Array {
  if (channels.length === 1) return channels[0];
  const len = channels.reduce((m, c) => Math.max(m, c.length), 0);
  const out = new Float32Array(len);
  for (const c of channels) for (let i = 0; i < c.length; i++) out[i] += c[i] / channels.length;
  return out;
}

/** A 16-bit PCM WAV file of the given planar channels (the fallback where no audio encoder exists). */
export function encodeWav(channels: readonly Float32Array[], sampleRate: number): Uint8Array {
  const count = Math.max(1, channels.length);
  const frames = channels.reduce((m, c) => Math.max(m, c.length), 0);
  const dataBytes = frames * count * 2;
  const out = new Uint8Array(44 + dataBytes);
  const view = new DataView(out.buffer);
  const ascii = (at: number, s: string) => { for (let i = 0; i < s.length; i++) out[at + i] = s.charCodeAt(i); };
  ascii(0, "RIFF");
  view.setUint32(4, 36 + dataBytes, true);
  ascii(8, "WAVE");
  ascii(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true); // PCM
  view.setUint16(22, count, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * count * 2, true);
  view.setUint16(32, count * 2, true);
  view.setUint16(34, 16, true);
  ascii(36, "data");
  view.setUint32(40, dataBytes, true);
  let at = 44;
  for (let i = 0; i < frames; i++) {
    for (let c = 0; c < count; c++) {
      const v = clamp(channels[c]?.[i] ?? 0, -1, 1);
      view.setInt16(at, v < 0 ? v * 0x8000 : v * 0x7fff, true);
      at += 2;
    }
  }
  return out;
}

/** WAV bytes for `durationMs` of mono 16-bit audio at `sampleRate` (to keep fallbacks under the upload limit). */
export function wavBytes(durationMs: number, sampleRate: number, channels = 1): number {
  return 44 + Math.ceil((Math.max(0, durationMs) / 1000) * sampleRate) * channels * 2;
}

/** The highest of the usual sample rates whose mono WAV of `durationMs` fits `maxBytes`. */
export function wavSampleRateFor(durationMs: number, maxBytes: number): number {
  for (const rate of [44_100, 32_000, 24_000, 22_050, 16_000]) if (wavBytes(durationMs, rate) <= maxBytes) return rate;
  return 16_000;
}

// ---------------------------------------------------------------- display

/** "0:07", "1:00". */
export function formatClock(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** A track name from a file name: no extension, underscores as spaces, at most 120 characters. */
export function trackTitle(fileName: string): string {
  const base = fileName.replace(/\.[a-z0-9]{1,5}$/i, "").replace(/[_]+/g, " ").replace(/\s+/g, " ").trim();
  return (base || "My audio").slice(0, 120);
}
