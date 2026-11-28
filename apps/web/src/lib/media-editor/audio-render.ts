// Browser audio for status music and voice: decode the user's own audio file, render the chosen
// range (volume, fade in / out) with an OfflineAudioContext, encode it small (AAC in MP4 or
// Opus in WebM through Mediabunny / WebCodecs; 16-bit WAV where no encoder exists), and read
// waveform peaks. Uploading is the caller's business. Mediabunny is loaded on first use.

import { encodeWav, MAX_AUDIO_MS, waveformPeaks, wavSampleRateFor, type AudioRange, type MusicSettings, WAVEFORM_BARS, fadeGain } from "./audio";

/** The status audio upload limit (ADR 0069 §5: IMAGE / AUDIO 8 MiB). */
export const AUDIO_UPLOAD_LIMIT = 8 * 1024 * 1024;
const RATE = 48_000;
const BITRATE = 128_000;

function offline(channels: number, frames: number, rate = RATE): OfflineAudioContext {
  return new OfflineAudioContext(channels, Math.max(1, frames), rate);
}

const decoded = new WeakMap<Blob, Promise<AudioBuffer>>();
/** Decodes a whole audio (or video) file once per Blob. */
export function decodeAudio(file: Blob): Promise<AudioBuffer> {
  let hit = decoded.get(file);
  if (!hit) {
    hit = file.arrayBuffer().then((data) => offline(2, 1).decodeAudioData(data));
    hit.catch(() => decoded.delete(file));
    decoded.set(file, hit);
  }
  return hit;
}

/** Duration of an audio file in ms (decoded, so it is exact even for VBR MP3 / WebM without a duration). */
export async function audioDurationMs(file: Blob): Promise<number> {
  return Math.round((await decodeAudio(file)).duration * 1000);
}

/** Waveform peaks of a file (or of a range of it). */
export async function audioPeaks(file: Blob, bars = WAVEFORM_BARS, range?: AudioRange): Promise<number[]> {
  const buf = await decodeAudio(file);
  const channels = Array.from({ length: buf.numberOfChannels }, (_, c) => buf.getChannelData(c));
  if (!range) return waveformPeaks(channels, bars);
  const a = Math.floor((range.startMs / 1000) * buf.sampleRate);
  const b = Math.ceil((range.endMs / 1000) * buf.sampleRate);
  return waveformPeaks(channels.map((c) => c.subarray(a, b)), bars);
}

/**
 * Renders `[startMs, endMs)` of a file with gain and fades to a new buffer (stereo 48 kHz, or
 * mono for voice).
 */
export async function renderRange(file: Blob, m: MusicSettings, options: { mono?: boolean } = {}): Promise<AudioBuffer> {
  const src = await decodeAudio(file);
  const len = Math.min(MAX_AUDIO_MS, m.endMs - m.startMs);
  if (!(len > 0)) throw new RangeError("The audio range is empty.");
  const ctx = offline(options.mono ? 1 : 2, Math.ceil((len / 1000) * RATE));
  const node = ctx.createBufferSource();
  node.buffer = src;
  const gain = ctx.createGain();
  if (m.volume !== 1 || m.fadeInMs > 0 || m.fadeOutMs > 0) {
    const points = 256;
    const curve = new Float32Array(points);
    for (let i = 0; i < points; i++) curve[i] = fadeGain(m.startMs + (len * i) / (points - 1), { ...m, endMs: m.startMs + len });
    gain.gain.setValueCurveAtTime(curve, 0, len / 1000);
  }
  node.connect(gain).connect(ctx.destination);
  node.start(0, m.startMs / 1000, len / 1000);
  return ctx.startRendering();
}

type Mediabunny = typeof import("mediabunny");

/** Encodes a buffer as AAC / MP4, else Opus / WebM, else WAV; always under the 8 MiB upload limit. */
export async function encodeAudio(buffer: AudioBuffer): Promise<Blob> {
  const durationMs = buffer.duration * 1000;
  if (typeof globalThis.AudioEncoder === "function") {
    try {
      const m: Mediabunny = await import("mediabunny");
      const quality = new m.Quality({ bitrate: BITRATE });
      const config = { numberOfChannels: buffer.numberOfChannels, sampleRate: buffer.sampleRate, quality };
      for (const [codec, kind] of [["aac", "mp4"], ["opus", "webm"]] as const) {
        if (!(await m.canEncodeAudio(codec, config).catch(() => false))) continue;
        const output = new m.Output({
          format: kind === "mp4" ? new m.Mp4OutputFormat({ fastStart: "in-memory" }) : new m.WebMOutputFormat(),
          target: new m.BufferTarget(),
        });
        const source = new m.AudioBufferSource({ codec, quality });
        output.addAudioTrack(source);
        await output.start();
        await source.add(buffer);
        source.close();
        await output.finalize();
        const bytes = output.target.buffer;
        if (bytes && bytes.byteLength <= AUDIO_UPLOAD_LIMIT) return new Blob([bytes], { type: kind === "mp4" ? "audio/mp4" : "audio/webm" });
      }
    } catch {
      /* fall back to WAV */
    }
  }
  // WAV: mono at the highest rate that fits the limit.
  const rate = wavSampleRateFor(durationMs, AUDIO_UPLOAD_LIMIT);
  const ctx = offline(1, Math.ceil(buffer.duration * rate), rate);
  const node = ctx.createBufferSource();
  node.buffer = buffer;
  node.connect(ctx.destination);
  node.start();
  const mono = await ctx.startRendering();
  return new Blob([encodeWav([mono.getChannelData(0)], rate) as BlobPart], { type: "audio/wav" });
}

/** The chosen range rendered and encoded, ready to upload as AUDIO (≤ 60 s, ≤ 8 MiB). */
export async function renderAudioClip(file: Blob, m: MusicSettings, options: { mono?: boolean } = {}): Promise<{ blob: Blob; durationMs: number }> {
  const buffer = await renderRange(file, m, options);
  const blob = await encodeAudio(buffer);
  return { blob, durationMs: Math.round(buffer.duration * 1000) };
}

/** Plays `[startMs, endMs)` of a file on an <audio> element with the volume and fades applied live (preview). */
export function previewEnvelope(el: HTMLAudioElement, m: MusicSettings): () => void {
  let raf = 0;
  const tick = () => {
    const t = el.currentTime * 1000;
    if (t >= m.endMs || t < m.startMs - 300) {
      el.currentTime = m.startMs / 1000;
    }
    el.volume = Math.min(1, fadeGain(Math.min(m.endMs, Math.max(m.startMs, t)), m));
    raf = requestAnimationFrame(tick);
  };
  el.currentTime = m.startMs / 1000;
  raf = requestAnimationFrame(tick);
  return () => cancelAnimationFrame(raf);
}
