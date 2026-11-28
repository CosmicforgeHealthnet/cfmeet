"use client";

import { useEffect, useRef, useState, type ReactNode } from "react";
import { Loader2, Mic, Palette, Pause, Play, RotateCcw, Send, Square, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import Waveform from "@/components/media-editor/Waveform";
import { formatClock, liveLevel, MAX_AUDIO_MS, pushLevel, WAVEFORM_BARS, type AudioRange } from "@/lib/media-editor/audio";
import { audioPeaks, decodeAudio, renderAudioClip } from "@/lib/media-editor/audio-render";
import { SOLID_BACKGROUNDS } from "@/lib/media-editor/text-status";
import type { StatusItemInput } from "@/lib/status-publisher/types";

const AUDIO_TYPES = ["audio/webm;codecs=opus", "audio/mp4", "audio/ogg;codecs=opus", "audio/webm"];

type Clip = { blob: Blob; url: string; ms: number; peaks: number[] };

/**
 * Voice status: tap the microphone to record (or hold it and release), at most a minute, with
 * a live waveform; then listen back on the clip's waveform, trim either end, re-record, choose
 * the background colour and post it as an AUDIO status.
 */
export default function VoiceStatus({ audienceButton, onClose, onPost }: {
  audienceButton: ReactNode;
  onClose: () => void;
  onPost: (item: StatusItemInput) => void;
}) {
  const [bg, setBg] = useState(0);
  const [recording, setRecording] = useState(false);
  const [levels, setLevels] = useState<number[]>([]);
  const [elapsed, setElapsed] = useState(0);
  const [clip, setClip] = useState<Clip | null>(null);
  const [trim, setTrim] = useState<AudioRange | null>(null);
  const [playing, setPlaying] = useState(false);
  const [position, setPosition] = useState(0);
  const [busy, setBusy] = useState(false);
  const audio = useRef<HTMLAudioElement>(null);
  const rec = useRef<{ recorder: MediaRecorder; stream: MediaStream; ctx: AudioContext; started: number; raf: number; held: boolean } | null>(null);
  const pressAt = useRef(0);

  const release = () => {
    const r = rec.current;
    if (!r) return;
    cancelAnimationFrame(r.raf);
    void r.ctx.close().catch(() => {});
    r.stream.getTracks().forEach((t) => t.stop());
  };
  useEffect(() => () => { release(); }, []);
  useEffect(() => () => { if (clip) URL.revokeObjectURL(clip.url); }, [clip]);

  const stop = () => {
    const r = rec.current;
    if (!r) return;
    if (r.recorder.state !== "inactive") r.recorder.stop();
    setRecording(false);
  };

  const start = async () => {
    if (rec.current) return;
    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true } });
    } catch {
      toast("Allow the microphone to record a voice status.", "error");
      return;
    }
    if (typeof MediaRecorder === "undefined") { stream.getTracks().forEach((t) => t.stop()); toast("This browser can't record audio.", "error"); return; }
    const type = AUDIO_TYPES.find((t) => MediaRecorder.isTypeSupported(t));
    const recorder = new MediaRecorder(stream, type ? { mimeType: type } : undefined);
    const chunks: Blob[] = [];
    const ctx = new AudioContext();
    const analyser = ctx.createAnalyser();
    analyser.fftSize = 1024;
    ctx.createMediaStreamSource(stream).connect(analyser);
    const data = new Uint8Array(analyser.fftSize);
    const started = Date.now();
    let lastBar = 0;
    const tick = () => {
      const r = rec.current;
      if (!r) return;
      const e = Date.now() - started;
      setElapsed(e);
      if (e - lastBar >= 90) {
        lastBar = e;
        analyser.getByteTimeDomainData(data);
        setLevels((l) => pushLevel(l, liveLevel(data)));
      }
      if (e >= MAX_AUDIO_MS) { stop(); return; }
      r.raf = requestAnimationFrame(tick);
    };
    recorder.ondataavailable = (ev) => { if (ev.data.size) chunks.push(ev.data); };
    recorder.onstop = () => {
      release();
      rec.current = null;
      const blob = new Blob(chunks, { type: (recorder.mimeType || type || "audio/webm").split(";")[0] });
      if (Date.now() - started < 700 || !blob.size) { toast("Hold on a little longer to record.", "error"); return; }
      void (async () => {
        try {
          const buf = await decodeAudio(blob);
          const ms = Math.min(MAX_AUDIO_MS, Math.round(buf.duration * 1000));
          const peaks = await audioPeaks(blob, WAVEFORM_BARS);
          setClip({ blob, url: URL.createObjectURL(blob), ms, peaks });
          setTrim({ startMs: 0, endMs: ms });
        } catch {
          // Decoding failed (rare codecs): keep the clip, without waveform or trimming.
          const ms = Math.min(MAX_AUDIO_MS, Date.now() - started);
          setClip({ blob, url: URL.createObjectURL(blob), ms, peaks: [] });
          setTrim(null);
        }
      })();
    };
    rec.current = { recorder, stream, ctx, started, raf: 0, held: false };
    recorder.start(250);
    setClip(null); setTrim(null); setLevels([]); setElapsed(0); setRecording(true);
    rec.current.raf = requestAnimationFrame(tick);
  };

  // Tap to start / stop; or hold and release.
  const down = () => { pressAt.current = Date.now(); if (!recording) void start(); };
  const up = () => { if (recording && Date.now() - pressAt.current > 450) stop(); };
  const tap = () => { if (recording && Date.now() - pressAt.current <= 450 && Date.now() - (rec.current?.started ?? 0) > 450) stop(); };

  const playPause = () => {
    const el = audio.current;
    if (!el || !clip) return;
    if (playing) { el.pause(); return; }
    const r = trim ?? { startMs: 0, endMs: clip.ms };
    if (el.currentTime * 1000 < r.startMs || el.currentTime * 1000 >= r.endMs - 50) el.currentTime = r.startMs / 1000;
    void el.play().then(() => setPlaying(true), () => setPlaying(false));
  };
  const onTime = () => {
    const el = audio.current;
    if (!el || !clip) return;
    const t = el.currentTime * 1000;
    setPosition(t);
    const r = trim ?? { startMs: 0, endMs: clip.ms };
    if (t >= r.endMs) { el.pause(); el.currentTime = r.startMs / 1000; }
  };

  const post = async () => {
    if (!clip || busy) return;
    setBusy(true);
    try {
      const r = trim ?? { startMs: 0, endMs: clip.ms };
      const trimmed = r.startMs > 50 || r.endMs < clip.ms - 50;
      const out = trimmed
        ? await renderAudioClip(clip.blob, { ...r, volume: 1, fadeInMs: 0, fadeOutMs: 0 }, { mono: true })
        : { blob: clip.blob, durationMs: clip.ms };
      onPost({ kind: "AUDIO", blob: out.blob, durationMs: out.durationMs, background: SOLID_BACKGROUNDS[bg] });
    } catch {
      toast("The voice note couldn't be prepared. Try again.", "error");
    } finally {
      setBusy(false);
    }
  };

  const selLen = clip ? (trim ? trim.endMs - trim.startMs : clip.ms) : elapsed;
  return (
    <div className="fixed inset-0 z-[80] flex flex-col text-white transition-colors" style={{ backgroundColor: SOLID_BACKGROUNDS[bg] }} role="dialog" aria-modal="true" aria-label="Voice status" data-testid="voice-status">
      {clip ? <audio ref={audio} src={clip.url} preload="auto" onTimeUpdate={onTime} onPause={() => setPlaying(false)} onEnded={() => setPlaying(false)} /> : null}
      <div className="flex items-center justify-between p-3 pt-[max(env(safe-area-inset-top),0.75rem)]">
        <button type="button" aria-label="Close" onClick={() => { stop(); onClose(); }} className="flex size-11 items-center justify-center rounded-full bg-black/25"><X className="size-6" /></button>
        <button type="button" aria-label="Background colour" onClick={() => setBg((bg + 1) % SOLID_BACKGROUNDS.length)} className="flex size-11 items-center justify-center rounded-full bg-black/25"><Palette className="size-5" /></button>
      </div>

      <div className="flex flex-1 flex-col items-center justify-center gap-5 px-6">
        <div className="w-full max-w-md rounded-3xl bg-black/20 p-4">
          {clip ? (
            <div className="flex items-center gap-3">
              <button type="button" aria-label={playing ? "Pause" : "Play"} onClick={playPause} data-testid="voice-play"
                className="flex size-12 shrink-0 items-center justify-center rounded-full bg-white text-black focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white focus-visible:ring-offset-2 focus-visible:ring-offset-black/40">
                {playing ? <Pause className="size-5" /> : <Play className="ml-0.5 size-5" />}
              </button>
              {clip.peaks.length ? (
                <Waveform peaks={clip.peaks} totalMs={clip.ms} selection={trim} moveWindow={false} label="Voice note: drag the ends to trim"
                  onSelection={(r) => setTrim(r)} progress={position / clip.ms} className="min-w-0 flex-1" />
              ) : <span className="min-w-0 flex-1 text-sm text-white/80">Voice note</span>}
            </div>
          ) : (
            <div className="flex h-14 items-center gap-[3px]" aria-hidden>
              {Array.from({ length: WAVEFORM_BARS }, (_, i) => {
                const v = levels[levels.length - WAVEFORM_BARS + i] ?? 0;
                return <span key={i} className="min-w-[2px] flex-1 rounded-full bg-white" style={{ height: `${Math.max(6, v * 100)}%`, opacity: v ? 1 : 0.35 }} />;
              })}
            </div>
          )}
          <p className="mt-2 text-center font-mono text-sm tabular-nums" aria-live="polite">{formatClock(selLen)} <span className="text-white/60">/ {formatClock(MAX_AUDIO_MS)}</span></p>
        </div>
        <p className="text-center text-sm text-white/85">
          {recording ? "Recording… tap to stop (or release if holding)" : clip ? "Listen, trim the ends, then post — or record again" : "Tap the microphone to record, or hold it"}
        </p>
      </div>

      <div className="space-y-3 p-4 pb-[max(env(safe-area-inset-bottom),1rem)]">
        <div className="flex justify-start">{audienceButton}</div>
        <div className="flex items-center justify-center gap-6">
          {clip ? (
            <button type="button" aria-label="Record again" onClick={() => { setClip(null); setTrim(null); setLevels([]); setElapsed(0); }} data-testid="voice-rerecord"
              className="flex size-12 items-center justify-center rounded-full bg-black/25 hover:bg-black/35"><RotateCcw className="size-5" /></button>
          ) : <span className="size-12" />}
          {clip ? null : (
            <button type="button" aria-label={recording ? "Stop recording" : "Record"} data-testid="voice-record"
              onPointerDown={(e) => { e.preventDefault(); down(); }} onPointerUp={up} onClick={tap} onContextMenu={(e) => e.preventDefault()}
              className={cn("flex size-20 touch-none select-none items-center justify-center rounded-full bg-white text-black shadow-xl transition-transform focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white focus-visible:ring-offset-2", recording && "scale-110 bg-red-500 text-white")}>
              {recording ? <Square className="size-7 fill-current" /> : <Mic className="size-8" />}
            </button>
          )}
          {clip ? (
            <button type="button" aria-label="Post voice status" onClick={() => void post()} disabled={busy} data-testid="voice-post"
              className="flex size-16 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-lg disabled:opacity-60">
              {busy ? <Loader2 className="size-6 animate-spin" /> : <Send className="size-6" />}
            </button>
          ) : <span className="size-12" />}
        </div>
      </div>
    </div>
  );
}
