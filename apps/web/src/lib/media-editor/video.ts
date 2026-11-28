// In-browser video processing for status videos: probe, timeline thumbnails, export
// (trim / crop / rotate / flip / resize / re-encode to ≤ 720p H.264 + AAC MP4, audio volume and
// music mixing, per-frame overlay drawing). Built on Mediabunny (MPL-2.0, WebCodecs), which is
// loaded on first use with a dynamic import so it never weighs on the initial bundle. Pure
// JavaScript: no workers, no WebAssembly, no eval, so it runs under the app's CSP and Trusted
// Types policy unchanged.
//
// Framework-free. Blob / File in, MP4 Blobs out; uploading is the caller's business.
// When `canProcessVideo()` resolves false (no WebCodecs encoder), callers keep their fallback
// (upload the original); `exportVideo` then fails fast with `VideoProcessingUnsupportedError`.

import type { Input, InputVideoTrack } from "mediabunny";
import { gainCurve, mixPlanar } from "./audio";
import {
  DEFAULT_MAX_HEIGHT,
  MAX_SEGMENT_MS,
  cropToPixels,
  editDurationMs,
  estimateOutputSize,
  musicForPart,
  musicWindow,
  planOutput,
  rotatedSize,
  type NormalizedCrop,
  type OutputPlan,
  type Rotation,
  type VideoEdit,
} from "./video-edit";

export * from "./video-edit";

type Mediabunny = typeof import("mediabunny");
let mediabunny: Promise<Mediabunny> | null = null;
/** Mediabunny, loaded once on first use. */
function load(): Promise<Mediabunny> {
  if (!mediabunny) {
    mediabunny = import("mediabunny");
    mediabunny.catch(() => { mediabunny = null; });
  }
  return mediabunny;
}

// ---------------------------------------------------------------- errors & capability

export type VideoUnsupportedReason = "no-webcodecs" | "no-encoder" | "undecodable" | "no-video-track" | "unreadable";

/** This browser / file cannot be processed here: fall back to uploading the original. */
export class VideoProcessingUnsupportedError extends Error {
  readonly reason: VideoUnsupportedReason;
  constructor(reason: VideoUnsupportedReason, message?: string) {
    super(message ?? UNSUPPORTED_MESSAGES[reason]);
    this.name = "VideoProcessingUnsupportedError";
    this.reason = reason;
  }
}

const UNSUPPORTED_MESSAGES: Record<VideoUnsupportedReason, string> = {
  "no-webcodecs": "This browser cannot process video (no WebCodecs).",
  "no-encoder": "This browser cannot encode video in a supported format.",
  undecodable: "This browser cannot decode this video's format.",
  "no-video-track": "This file has no video.",
  unreadable: "This video file could not be read.",
};

export function isVideoProcessingUnsupported(e: unknown): e is VideoProcessingUnsupportedError {
  return e instanceof VideoProcessingUnsupportedError;
}

const abortError = () => new DOMException("The operation was aborted.", "AbortError");
export function isAbortError(e: unknown): boolean {
  return e instanceof DOMException && e.name === "AbortError";
}

/** The WebCodecs classes processing needs exist (synchronous, no codec check). */
export function hasWebCodecs(): boolean {
  return typeof globalThis.VideoEncoder === "function"
    && typeof globalThis.VideoDecoder === "function"
    && typeof globalThis.AudioDecoder === "function"
    && typeof globalThis.VideoFrame === "function";
}

const VIDEO_CODECS = ["avc", "vp9", "av1", "hevc"] as const;
const AUDIO_CODECS = ["aac", "opus"] as const;
const OUTPUT_SAMPLE_RATE = 48_000;

let capability: Promise<boolean> | null = null;
/** Whether this browser can export videos (WebCodecs with an MP4-compatible video encoder at 720p). */
export function canProcessVideo(): Promise<boolean> {
  if (!hasWebCodecs()) return Promise.resolve(false);
  if (!capability) {
    capability = load()
      .then((m) => m.getFirstEncodableVideoCodec([...VIDEO_CODECS], { width: 1280, height: 720, bitrate: 2_000_000 }))
      .then((codec) => codec !== null)
      .catch(() => false);
  }
  return capability;
}

// ---------------------------------------------------------------- input helpers

function openInput(m: Mediabunny, file: Blob): Input {
  return new m.Input({ source: new m.BlobSource(file), formats: m.ALL_FORMATS });
}

async function primaryVideo(input: Input): Promise<InputVideoTrack> {
  let track: InputVideoTrack | null;
  try {
    track = await input.getPrimaryVideoTrack();
  } catch {
    throw new VideoProcessingUnsupportedError("unreadable");
  }
  if (!track) throw new VideoProcessingUnsupportedError("no-video-track");
  return track;
}

// ---------------------------------------------------------------- probe

export type VideoProbe = {
  durationMs: number;
  /** Display size, the file's own orientation applied. */
  width: number;
  height: number;
  /** The file's own rotation metadata (already applied to width / height). */
  rotation: Rotation;
  hasAudio: boolean;
  sizeBytes: number;
  mimeType: string;
  videoCodec: string | null;
  audioCodec: string | null;
  frameRate: number | null;
  videoBitrate: number | null;
  audioBitrate: number | null;
  /** This browser can decode the video (and audio, if any) track. */
  canDecode: boolean;
  canDecodeAudio: boolean;
};

/** Reads a video's metadata (container index only; nothing is decoded). */
export async function probeVideo(file: Blob): Promise<VideoProbe> {
  if (!hasWebCodecs()) throw new VideoProcessingUnsupportedError("no-webcodecs");
  const m = await load();
  const input = openInput(m, file);
  try {
    const video = await primaryVideo(input);
    const audio = await input.getPrimaryAudioTrack().catch(() => null);
    const [duration, width, height, rotation, videoCodec, canDecode, stats] = await Promise.all([
      input.computeDuration(),
      video.getDisplayWidth(),
      video.getDisplayHeight(),
      video.getRotation(),
      video.getCodec(),
      video.canDecode().catch(() => false),
      video.computePacketStats(240).catch(() => null),
    ]);
    let audioCodec: string | null = null;
    let audioBitrate: number | null = null;
    let canDecodeAudio = false;
    if (audio) {
      audioCodec = await audio.getCodec();
      canDecodeAudio = await audio.canDecode().catch(() => false);
      const a = await audio.computePacketStats(400).catch(() => null);
      audioBitrate = a && Number.isFinite(a.averageBitrate) ? Math.round(a.averageBitrate) : null;
    }
    const finite = (v: number | undefined) => (v !== undefined && Number.isFinite(v) && v > 0 ? v : null);
    return {
      durationMs: Math.round(duration * 1000),
      width,
      height,
      rotation,
      hasAudio: Boolean(audio),
      sizeBytes: file.size,
      mimeType: file.type || (await input.getMimeType().catch(() => "")),
      videoCodec,
      audioCodec,
      frameRate: finite(stats?.averagePacketRate),
      // Whole-file average (a prefix of packets can be far from it): container overhead is tiny.
      videoBitrate: duration > 0 ? Math.max(1, Math.round((file.size * 8) / duration - (audioBitrate ?? 0))) : finite(stats?.averageBitrate),
      audioBitrate,
      canDecode,
      canDecodeAudio,
    };
  } catch (e) {
    if (isVideoProcessingUnsupported(e)) throw e;
    throw new VideoProcessingUnsupportedError("unreadable", e instanceof Error ? e.message : undefined);
  } finally {
    input.dispose();
  }
}

// ---------------------------------------------------------------- thumbnails

export type VideoThumbnail = { timeMs: number; bitmap: ImageBitmap };

/**
 * `count` frame thumbnails `height` px tall, evenly spread over the video. Decodes only key
 * frames when they are dense enough (fast: no inter-frame decoding), exact frames otherwise.
 * `onThumbnail` fires as each one is ready. Abort with `signal`; the caller owns (and should
 * `close()`) the returned bitmaps.
 */
export async function timelineThumbnails(
  file: Blob,
  count: number,
  height: number,
  options: { signal?: AbortSignal; onThumbnail?: (thumb: VideoThumbnail, index: number) => void } = {},
): Promise<VideoThumbnail[]> {
  const { signal, onThumbnail } = options;
  if (signal?.aborted) throw abortError();
  if (!hasWebCodecs()) throw new VideoProcessingUnsupportedError("no-webcodecs");
  const m = await load();
  const input = openInput(m, file);
  const out: VideoThumbnail[] = [];
  const bitmaps = new Map<number, ImageBitmap | null>();
  try {
    const track = await primaryVideo(input);
    if (!(await track.canDecode())) throw new VideoProcessingUnsupportedError("undecodable");
    const start = await track.getFirstTimestamp();
    const duration = Math.max(0.001, (await track.computeDuration()) - start);
    const n = Math.max(1, Math.round(count));
    const wanted = Array.from({ length: n }, (_, i) => start + ((i + 0.5) * duration) / n);

    // Snap to key frames when there are enough of them.
    const packets = new m.EncodedPacketSink(track);
    const keyTimes = await Promise.all(wanted.map(async (t) => (await packets.getKeyPacket(t, { metadataOnly: true }))?.timestamp ?? t));
    const unique = Array.from(new Set(keyTimes)).sort((a, b) => a - b);
    const useKeys = unique.length >= Math.ceil(n * 0.6);
    const times = useKeys ? unique : wanted;

    let emitted = 0;
    const emit = (t: number, bitmap: ImageBitmap | null) => {
      bitmaps.set(t, bitmap);
      // Emit every wanted slot that is now ready, in order.
      while (emitted < n) {
        const b = bitmaps.get(useKeys ? keyTimes[emitted] : wanted[emitted]);
        if (b === undefined) break;
        if (b) {
          const thumb = { timeMs: Math.round((wanted[emitted] - start) * 1000), bitmap: b };
          out.push(thumb);
          onThumbnail?.(thumb, emitted);
        }
        emitted++;
      }
    };
    const thumbHeight = Math.max(16, Math.round(height));
    const config = useKeys && typeof OffscreenCanvas === "function" ? await track.getDecoderConfig() : null;
    if (config) {
      // Key frames decode on their own: feed only those to one decoder (~10 ms a frame).
      const [dw, dh, rotation] = await Promise.all([track.getDisplayWidth(), track.getDisplayHeight(), track.getRotation()]);
      const tw = Math.max(1, Math.round((thumbHeight * dw) / dh));
      const byMicros = new Map<number, number>();
      let failure: unknown = null;
      const decoder = new VideoDecoder({
        output: (frame) => {
          try {
            const t = byMicros.get(frame.timestamp);
            const canvas = new OffscreenCanvas(tw, thumbHeight);
            const ctx = canvas.getContext("2d");
            if (ctx && t !== undefined) {
              const [w, h] = rotation % 180 ? [thumbHeight, tw] : [tw, thumbHeight];
              ctx.translate(tw / 2, thumbHeight / 2);
              ctx.rotate((rotation * Math.PI) / 180);
              ctx.drawImage(frame, -w / 2, -h / 2, w, h);
              emit(t, canvas.transferToImageBitmap());
            }
          } finally {
            frame.close();
          }
        },
        error: (e) => { failure = e; },
      });
      try {
        decoder.configure(config);
        for (const t of unique) {
          if (signal?.aborted) throw abortError();
          const packet = await packets.getKeyPacket(t, { verifyKeyPackets: true });
          if (!packet) { emit(t, null); continue; }
          const chunk = packet.toEncodedVideoChunk();
          byMicros.set(chunk.timestamp, t);
          decoder.decode(chunk);
        }
        await decoder.flush();
        if (failure) throw failure;
        if (signal?.aborted) throw abortError();
      } finally {
        if (decoder.state !== "closed") decoder.close();
      }
    } else {
      const sink = new m.CanvasSink(track, { height: thumbHeight, poolSize: 0 });
      let i = 0;
      for await (const frame of sink.canvasesAtTimestamps(times)) {
        if (signal?.aborted) throw abortError();
        emit(times[i++], frame ? await createImageBitmap(frame.canvas) : null);
      }
    }
    return out;
  } catch (e) {
    for (const b of bitmaps.values()) b?.close();
    throw e;
  } finally {
    input.dispose();
  }
}

// ---------------------------------------------------------------- music

export type MusicTrack = {
  file: Blob;
  /** Where in the clip (ms) music time `startMs` plays; may be negative (music began earlier). */
  offsetMs: number;
  /** The part of the music file to use. */
  startMs: number;
  endMs: number;
  /** Gain 0..2. */
  volume: number;
  /** Fades at the start / end of the music range. */
  fadeInMs: number;
  fadeOutMs: number;
};

const decodedMusic = new WeakMap<Blob, Promise<AudioBuffer>>();
function decodeMusic(file: Blob): Promise<AudioBuffer> {
  let hit = decodedMusic.get(file);
  if (!hit) {
    hit = file.arrayBuffer().then((data) => new OfflineAudioContext(2, 1, OUTPUT_SAMPLE_RATE).decodeAudioData(data));
    hit.catch(() => decodedMusic.delete(file));
    decodedMusic.set(file, hit);
  }
  return hit;
}

/** The music heard in a clip of `clipMs`, rendered (gain, fades) to a stereo 48 kHz buffer, or null. */
async function renderMusic(music: MusicTrack, clipMs: number): Promise<AudioBuffer | null> {
  const win = musicWindow(music, clipMs);
  if (!win || music.volume <= 0) return null;
  const decoded = await decodeMusic(music.file);
  const frames = Math.max(1, Math.ceil((clipMs / 1000) * OUTPUT_SAMPLE_RATE));
  const ctx = new OfflineAudioContext(2, frames, OUTPUT_SAMPLE_RATE);
  const src = ctx.createBufferSource();
  src.buffer = decoded;
  const gain = ctx.createGain();
  // Gain as a function of music time: volume, ramped in / out at the ends of the music range.
  const curve = gainCurve(music, win.fromMs, win.durationMs);
  gain.gain.setValueCurveAtTime(curve, win.atMs / 1000, win.durationMs / 1000);
  src.connect(gain).connect(ctx.destination);
  src.start(win.atMs / 1000, win.fromMs / 1000, win.durationMs / 1000);
  return ctx.startRendering();
}

// ---------------------------------------------------------------- export

export type OverlayDrawer = (
  ctx: CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D,
  width: number,
  height: number,
  timeMs: number,
  frame: { sourceMs: number },
) => void;

export type ExportVideoOptions = {
  file: Blob;
  /** Source range; at most 60 s long. */
  startMs: number;
  endMs: number;
  /** Crop in 0..1 units of the rotated (and flipped) frame. */
  crop?: NormalizedCrop | null;
  rotation?: Rotation;
  flipH?: boolean;
  mute?: boolean;
  /** Original-audio gain 0..2. */
  volume?: number;
  music?: MusicTrack | null;
  /** Draws on each output frame (after crop / rotate / resize); `timeMs` is from the clip's start. */
  drawOverlay?: OverlayDrawer | null;
  /** Short-side limit of the output, default 720. */
  maxHeight?: number;
  signal?: AbortSignal;
  onProgress?: (progress: number) => void;
  /** Probe of `file`, when the caller already has one (saves re-reading the index). */
  probe?: VideoProbe;
};

/** Output parameters `exportVideo` would use (for size estimates in the UI). */
export function planExport(probe: VideoProbe, o: Pick<ExportVideoOptions, "crop" | "rotation" | "flipH" | "mute" | "volume" | "maxHeight"> & { hasMusic?: boolean; hasOverlay?: boolean }): OutputPlan {
  return planOutput({
    width: probe.width,
    height: probe.height,
    rotation: o.rotation ?? 0,
    crop: o.crop ?? null,
    flipH: o.flipH,
    sourceVideoBitrate: probe.videoBitrate,
    sourceAudioBitrate: probe.audioBitrate,
    frameRate: probe.frameRate,
    hasAudio: probe.hasAudio && probe.canDecodeAudio,
    videoCodec: probe.videoCodec,
    audioCodec: probe.audioCodec,
    muted: o.mute,
    volume: o.volume,
    hasMusic: o.hasMusic,
    hasOverlay: o.hasOverlay,
    maxHeight: o.maxHeight ?? DEFAULT_MAX_HEIGHT,
  });
}

/**
 * Exports `[startMs, endMs)` of a video as an MP4 Blob (≤ 60 s): H.264 / AAC where the browser
 * can encode them (else VP9 / AV1 / HEVC video, Opus audio), ≤ 720p, ~1.5–2.5 Mbps. A small,
 * unedited H.264 source is copied without re-encoding. Only the requested range is decoded.
 */
export async function exportVideo(o: ExportVideoOptions): Promise<Blob> {
  const { signal, onProgress } = o;
  if (signal?.aborted) throw abortError();
  if (!hasWebCodecs()) throw new VideoProcessingUnsupportedError("no-webcodecs");
  if (!(o.endMs > o.startMs)) throw new RangeError("exportVideo: endMs must be after startMs");
  if (o.endMs - o.startMs > MAX_SEGMENT_MS + 1) throw new RangeError("exportVideo: a part can be at most 60 s");
  const m = await load();
  const probe = o.probe ?? await probeVideo(o.file);
  if (!probe.canDecode) throw new VideoProcessingUnsupportedError("undecodable");

  const rotation = o.rotation ?? 0;
  const clipMs = Math.min(o.endMs, probe.durationMs) - o.startMs;
  const music = o.music && musicWindow(o.music, clipMs) && o.music.volume > 0 ? o.music : null;
  const volume = Math.min(2, Math.max(0, o.volume ?? 1));
  const sourceAudio = probe.hasAudio && probe.canDecodeAudio && !o.mute && volume > 0;
  const plan = planExport(probe, { ...o, rotation, mute: o.mute || volume === 0, volume, hasMusic: Boolean(music), hasOverlay: Boolean(o.drawOverlay) });

  if (plan.passThrough) {
    const copied = await tryCopy(m, o, probe, signal, onProgress);
    if (copied) return copied;
  }

  const videoCodec = await m.getFirstEncodableVideoCodec([...VIDEO_CODECS], { width: plan.width, height: plan.height, bitrate: plan.videoBitrate });
  if (!videoCodec) throw new VideoProcessingUnsupportedError("no-encoder");
  const needsAudio = sourceAudio || Boolean(music);
  const audioCodec = needsAudio
    ? await m.getFirstEncodableAudioCodec([...AUDIO_CODECS], { numberOfChannels: 2, sampleRate: OUTPUT_SAMPLE_RATE, bitrate: plan.audioBitrate })
    : null;
  if (needsAudio && !audioCodec) throw new VideoProcessingUnsupportedError("no-encoder", "This browser cannot encode audio in a supported format.");
  if (signal?.aborted) throw abortError();

  const musicBuffer = music ? await renderMusic(music, clipMs) : null;
  if (signal?.aborted) throw abortError();

  const frame = rotatedSize(probe.width, probe.height, rotation);
  const crop = o.crop ? cropToPixels(o.crop, frame.width, frame.height) : undefined;
  const process = o.drawOverlay ? overlayProcessor(o.drawOverlay, o.startMs) : undefined;

  const input = openInput(m, o.file);
  const output = new m.Output({ format: new m.Mp4OutputFormat({ fastStart: "in-memory" }), target: new m.BufferTarget() });
  const musicOnly = Boolean(musicBuffer) && !sourceAudio;
  const audioQuality = new m.Quality({ bitrate: plan.audioBitrate || 96_000 });
  let conversion: import("mediabunny").Conversion | null = null;
  const onAbort = () => { void conversion?.cancel(); };
  signal?.addEventListener("abort", onAbort);
  try {
    conversion = await m.Conversion.init({
      input,
      output,
      tracks: "primary",
      composable: musicOnly,
      showWarnings: false,
      trim: { start: o.startMs / 1000, end: (o.startMs + clipMs) / 1000 },
      video: {
        width: plan.width,
        height: plan.height,
        fit: "fill",
        rotate: rotation,
        flip: o.flipH || undefined,
        crop,
        codec: videoCodec,
        // Constant bitrate keeps sizes predictable (the UI shows an estimate before export).
        quality: new m.Quality({ bitrate: plan.videoBitrate, bitrateMode: "constant" }),
        keyFrameInterval: 2,
        process,
      },
      audio: sourceAudio
        ? {
          codec: audioCodec!,
          quality: audioQuality,
          numberOfChannels: 2,
          sampleRate: OUTPUT_SAMPLE_RATE,
          forceTranscode: volume !== 1 || Boolean(musicBuffer) || undefined,
          process: volume !== 1 || musicBuffer ? audioMixer(m, volume, musicBuffer) : undefined,
        }
        : { discard: true },
    });
    if (!conversion.isValid) {
      const reasons = conversion.discardedTracks.map((d) => d.reason);
      throw new VideoProcessingUnsupportedError(reasons.includes("undecodable_source_codec") ? "undecodable" : "no-encoder");
    }
    if (conversion.discardedTracks.some((d) => d.track.isVideoTrack())) throw new VideoProcessingUnsupportedError("no-encoder");
    conversion.onProgress = (p) => onProgress?.(Math.min(0.99, p));

    if (musicOnly) {
      const source = new m.AudioBufferSource({ codec: audioCodec!, quality: audioQuality });
      output.addAudioTrack(source);
      await output.start();
      await Promise.all([
        conversion.execute(),
        (async () => { await source.add(musicBuffer!); source.close(); })(),
      ]);
      await output.finalize();
    } else {
      await conversion.execute();
    }
    if (signal?.aborted) throw abortError();
    const buffer = output.target.buffer;
    if (!buffer) throw new Error("The video export produced no data.");
    onProgress?.(1);
    return new Blob([buffer], { type: output.format.mimeType });
  } catch (e) {
    if (signal?.aborted || e instanceof m.ConversionCanceledError) throw abortError();
    throw e;
  } finally {
    signal?.removeEventListener("abort", onAbort);
    if (output.state !== "finalized" && output.state !== "canceled") await output.cancel().catch(() => {});
    input.dispose();
  }
}

/** Copies the range without re-encoding; null when the source does not allow it here. */
async function tryCopy(m: Mediabunny, o: ExportVideoOptions, probe: VideoProbe, signal?: AbortSignal, onProgress?: (p: number) => void): Promise<Blob | null> {
  const input = openInput(m, o.file);
  const output = new m.Output({ format: new m.Mp4OutputFormat({ fastStart: "in-memory" }), target: new m.BufferTarget() });
  const onAbort = () => { void conversion?.cancel(); };
  let conversion: import("mediabunny").Conversion | null = null;
  signal?.addEventListener("abort", onAbort);
  try {
    conversion = await m.Conversion.init({
      input,
      output,
      tracks: "primary",
      showWarnings: false,
      trim: { start: o.startMs / 1000, end: Math.min(o.endMs, probe.durationMs) / 1000 },
      audio: o.mute ? { discard: true } : undefined,
      // Never longer than asked (statuses are ≤ 60 s), and at most a quarter second shorter.
      copy: { mode: "forced", boundaryPolicy: "shrink", boundaryTolerance: 0.25, shiftTolerance: 0.25 },
    });
    if (!conversion.isValid || conversion.discardedTracks.some((d) => d.reason !== "discarded_by_user")) return null;
    conversion.onProgress = (p) => onProgress?.(Math.min(0.99, p));
    await conversion.execute();
    const buffer = output.target.buffer;
    if (!buffer) return null;
    onProgress?.(1);
    return new Blob([buffer], { type: output.format.mimeType });
  } catch (e) {
    if (signal?.aborted || e instanceof m.ConversionCanceledError) throw abortError();
    return null;
  } finally {
    signal?.removeEventListener("abort", onAbort);
    if (output.state !== "finalized" && output.state !== "canceled") await output.cancel().catch(() => {});
    input.dispose();
  }
}

type VideoSampleLike = import("mediabunny").VideoSample;
type AudioSampleLike = import("mediabunny").AudioSample;

/** Frame processor: draws each (already transformed) frame on a canvas, then the overlay. */
function overlayProcessor(draw: OverlayDrawer, sourceStartMs: number) {
  let canvas: OffscreenCanvas | HTMLCanvasElement | null = null;
  let ctx: OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D | null = null;
  return (sample: VideoSampleLike) => {
    const w = sample.displayWidth;
    const h = sample.displayHeight;
    if (!canvas || canvas.width !== w || canvas.height !== h) {
      canvas = typeof OffscreenCanvas === "function" ? new OffscreenCanvas(w, h) : Object.assign(document.createElement("canvas"), { width: w, height: h });
      ctx = canvas.getContext("2d", { alpha: false }) as OffscreenCanvasRenderingContext2D | CanvasRenderingContext2D | null;
      if (!ctx) throw new VideoProcessingUnsupportedError("no-encoder", "Canvas 2D is not available.");
    }
    const c = ctx!;
    c.save();
    sample.draw(c, 0, 0, w, h);
    c.restore();
    const timeMs = sample.timestamp * 1000;
    c.save();
    draw(c, w, h, timeMs, { sourceMs: sourceStartMs + timeMs });
    c.restore();
    return canvas;
  };
}

/** Audio processor: original audio × volume + rendered music, clipped to [-1, 1]. */
function audioMixer(m: Mediabunny, volume: number, music: AudioBuffer | null) {
  const musicChannels = music ? Array.from({ length: music.numberOfChannels }, (_, c) => music.getChannelData(c)) : [];
  const musicRate = music?.sampleRate ?? OUTPUT_SAMPLE_RATE;
  return (sample: AudioSampleLike) => {
    const n = sample.numberOfFrames;
    const channels = sample.numberOfChannels;
    const data = new Float32Array(n * channels);
    for (let c = 0; c < channels; c++) sample.copyTo(data.subarray(c * n, (c + 1) * n), { planeIndex: c, format: "f32-planar" });
    mixPlanar(data, n, channels, volume, musicChannels, Math.round(sample.timestamp * musicRate));
    return new m.AudioSample({ data, format: "f32-planar", numberOfChannels: channels, sampleRate: sample.sampleRate, timestamp: sample.timestamp });
  };
}

// ---------------------------------------------------------------- edits

export type RenderVideoEditOptions = {
  /** `timeMs` is on the POSTED timeline (all parts back to back); `frame.partIndex` says which part. */
  drawOverlay?: ((
    ctx: CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D,
    width: number,
    height: number,
    timeMs: number,
    frame: { sourceMs: number; partIndex: number },
  ) => void) | null;
  /** Music placed on the posted timeline (`offsetMs` from the first part's start). */
  music?: MusicTrack | null;
  maxHeight?: number;
  signal?: AbortSignal;
  /** `part` is 0-based; `partProgress` 0..1 within it; `overall` 0..1 across all parts. */
  onProgress?: (p: { part: number; parts: number; partProgress: number; overall: number }) => void;
  probe?: VideoProbe;
};

/**
 * Turns a `VideoEdit` into MP4 Blobs, one per part (each ≤ 60 s), in posting order. Parts are
 * exported one at a time (each decodes only its own range) so memory stays flat.
 */
export async function renderVideoEdit(file: Blob, edit: VideoEdit, o: RenderVideoEditOptions = {}): Promise<Blob[]> {
  const probe = o.probe ?? await probeVideo(file);
  const total = Math.max(1, editDurationMs(edit));
  const out: Blob[] = [];
  let timeline = 0;
  for (const [part, seg] of edit.segments.entries()) {
    const len = seg.endMs - seg.startMs;
    const before = timeline;
    const draw = o.drawOverlay;
    const blob = await exportVideo({
      file,
      probe,
      startMs: seg.startMs,
      endMs: seg.endMs,
      crop: edit.crop,
      rotation: edit.rotation,
      flipH: edit.flipH,
      mute: edit.muted,
      volume: edit.volume,
      music: o.music ? { ...o.music, ...musicForPart(o.music, before) } : null,
      drawOverlay: draw ? (ctx, w, h, t, f) => draw(ctx, w, h, before + t, { sourceMs: f.sourceMs, partIndex: part }) : null,
      maxHeight: o.maxHeight,
      signal: o.signal,
      onProgress: (p) => o.onProgress?.({ part, parts: edit.segments.length, partProgress: p, overall: (before + p * len) / total }),
    });
    out.push(blob);
    timeline += len;
  }
  return out;
}

/** Estimated total output size of an edit in bytes (sum over its parts). */
export function estimateEditSize(probe: VideoProbe, edit: VideoEdit, extra: { hasMusic?: boolean; hasOverlay?: boolean; maxHeight?: number } = {}): number {
  const plan = planExport(probe, { crop: edit.crop, rotation: edit.rotation, flipH: edit.flipH, mute: edit.muted, volume: edit.volume, ...extra });
  return edit.segments.reduce((sum, s) => sum + estimateOutputSize({ durationMs: s.endMs - s.startMs, videoBitrate: plan.videoBitrate, audioBitrate: plan.audioBitrate }), 0);
}
