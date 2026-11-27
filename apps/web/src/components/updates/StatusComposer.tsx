"use client";

import { useEffect, useRef, useState } from "react";
import { ImagePlus, Mic, Palette, Send, Type, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { durationOf, kindOf, LIMITS, segments, SEGMENT_MS, type MediaKind } from "@/services/v2/social.service";
import { enqueueStatus } from "@/lib/status-publisher/client";
import { BACKGROUNDS, errorText, FONTS } from "./shared";

type Mode = "text" | "media" | "voice";

/** Wall-clock milliseconds (event handlers and timers only). */
const now = () => Date.now();

/**
 * Add status (WhatsApp): text on a colour, a photo or video from the device with a caption,
 * or a voice note (hold to record, at most a minute). A video longer than a minute is
 * uploaded once and posted as consecutive one-minute statuses.
 */
export default function StatusComposer({ initial, onClose, onPosted }: { initial: Mode; onClose: () => void; onPosted: () => void }) {
  const [mode, setMode] = useState<Mode>(initial);
  const [text, setText] = useState("");
  const [bg, setBg] = useState(0);
  const [font, setFont] = useState(0);
  const [file, setFile] = useState<File | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [kind, setKind] = useState<MediaKind | null>(null);
  const [duration, setDuration] = useState<number | null>(null);
  const [caption, setCaption] = useState("");
  const picker = useRef<HTMLInputElement>(null);

  useEffect(() => { if (initial === "media") picker.current?.click(); }, [initial]);
  useEffect(() => () => { if (preview) URL.revokeObjectURL(preview); }, [preview]);

  const pick = async (f: File | undefined) => {
    if (!f) return;
    const k = kindOf(f);
    if (!k || k === "AUDIO") { toast("Choose a photo or a video.", "error"); return; }
    if (f.size > LIMITS[k]) { toast(k === "VIDEO" ? "Videos can be at most 64 MB." : "Photos can be at most 8 MB.", "error"); return; }
    let d: number | null = null;
    if (k === "VIDEO") {
      try { d = await durationOf(f); } catch { toast("This video can't be played here.", "error"); return; }
    }
    setFile(f); setKind(k); setDuration(d); setPreview(URL.createObjectURL(f)); setMode("media");
  };

  // Non-blocking (WhatsApp): queued, then processed / uploaded / published in the background
  // (src/lib/status-publisher); the Updates card shows "Adding…" and its progress.
  const post = () => {
    if (mode === "text" ? !text.trim() : !(mode === "media" && file && kind)) return;
    void enqueueStatus({ items: [mode === "text" ? { kind: "TEXT", text: text.trim(), background: BACKGROUNDS[bg], font }
      : kind === "VIDEO" && duration ? { kind: "VIDEO", source: file!, durationMs: duration, ranges: segments(duration), caption } // caption: first part only
      : { kind: "IMAGE", blob: file!, caption }] }).catch((e) => toast(errorText(e, "Your status couldn't be added. Try again."), "error"));
    onPosted();
    onClose();
  };

  const parts = kind === "VIDEO" && duration ? segments(duration).length : 1;

  return (
    <div className="fixed inset-0 z-[70] flex flex-col bg-black text-white" role="dialog" aria-label="Add status" data-testid="status-composer">
      <input ref={picker} type="file" accept="image/*,video/*" className="hidden" onChange={(e) => { void pick(e.target.files?.[0]); e.target.value = ""; }} />
      {mode === "voice" ? (
        <VoiceStatus bg={BACKGROUNDS[bg]} onCycle={() => setBg((bg + 1) % BACKGROUNDS.length)} onClose={onClose} onPosted={() => { onPosted(); onClose(); }} />
      ) : (
        <>
          <div className="absolute inset-x-0 top-0 z-10 flex items-center justify-between p-4">
            <button type="button" aria-label="Close" onClick={onClose} className="flex size-11 items-center justify-center rounded-full bg-black/30 backdrop-blur"><X className="size-6" /></button>
            {mode === "text" ? (
              <div className="flex gap-2">
                <button type="button" aria-label="Font" onClick={() => setFont((font + 1) % FONTS.length)} className="flex size-11 items-center justify-center rounded-full bg-black/25 text-base font-semibold backdrop-blur">Aa</button>
                <button type="button" aria-label="Background colour" onClick={() => setBg((bg + 1) % BACKGROUNDS.length)} className="flex size-11 items-center justify-center rounded-full bg-black/25 backdrop-blur"><Palette className="size-5" /></button>
              </div>
            ) : null}
          </div>

          {mode === "text" ? (
            <div className="flex flex-1 items-center justify-center px-6" style={{ backgroundColor: BACKGROUNDS[bg] }}>
              <textarea autoFocus value={text} maxLength={700} onChange={(e) => setText(e.target.value)} placeholder="Type a status" data-testid="status-text"
                className={cn("w-full resize-none bg-transparent text-center text-3xl leading-snug text-white placeholder:text-white/60 focus:outline-none", FONTS[font])}
                rows={Math.min(8, Math.max(2, Math.ceil(text.length / 18)))} />
            </div>
          ) : (
            <div className="flex min-h-0 flex-1 items-center justify-center bg-black">
              {!preview ? (
                <button type="button" onClick={() => picker.current?.click()} className="flex flex-col items-center gap-3 text-white/80"><ImagePlus className="size-12" />Choose a photo or video</button>
              ) : kind === "VIDEO" ? (
                <video src={preview} controls playsInline className="max-h-full max-w-full" />
              ) : (
                // eslint-disable-next-line @next/next/no-img-element
                <img src={preview} alt="" className="max-h-full max-w-full object-contain" />
              )}
            </div>
          )}

          <div className="space-y-3 bg-black/90 p-3 pb-[max(env(safe-area-inset-bottom),0.75rem)]">
            {mode === "media" && preview ? (
              <>
                {kind === "VIDEO" && duration && duration > SEGMENT_MS ? (
                  <p className="text-center text-xs text-white/70" data-testid="status-segments">Longer than a minute: it will be posted as {parts} statuses of up to 1 minute each.</p>
                ) : null}
                <input value={caption} maxLength={700} onChange={(e) => setCaption(e.target.value)} placeholder="Add a caption…" className="h-12 w-full rounded-full bg-white/10 px-5 text-base text-white placeholder:text-white/50 focus:outline-none" />
              </>
            ) : null}
            <div className="flex items-center gap-2">
              <div className="flex flex-1 gap-1">
                {([["media", "Photo", ImagePlus], ["text", "Text", Type], ["voice", "Voice", Mic]] as const).map(([m, label, Icon]) => (
                  <button key={m} type="button" onClick={() => (m === "media" && !preview ? picker.current?.click() : setMode(m))}
                    className={cn("flex h-10 items-center gap-1.5 rounded-full px-3.5 text-sm font-semibold", mode === m ? "bg-white/20" : "text-white/80 hover:bg-white/10")}>
                    <Icon className="size-4" />{label}
                  </button>
                ))}
              </div>
              <button type="button" aria-label="Post status" data-testid="status-send" disabled={mode === "text" ? !text.trim() : !file} onClick={post}
                className="relative flex size-14 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-lg disabled:opacity-50">
                <Send className="size-6" />
              </button>
            </div>
          </div>
        </>
      )}
    </div>
  );
}

/** Voice status: hold the microphone to record (at most a minute), then post. */
function VoiceStatus({ bg, onCycle, onClose, onPosted }: { bg: string; onCycle: () => void; onClose: () => void; onPosted: () => void }) {
  const [recording, setRecording] = useState(false);
  const [clip, setClip] = useState<{ blob: Blob; url: string; ms: number } | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const rec = useRef<{ recorder: MediaRecorder; started: number; stream: MediaStream; timer: ReturnType<typeof setInterval> } | null>(null);

  useEffect(() => () => { const r = rec.current; if (r) { clearInterval(r.timer); r.stream.getTracks().forEach((t) => t.stop()); } }, []);
  useEffect(() => () => { if (clip) URL.revokeObjectURL(clip.url); }, [clip]);

  const stop = () => {
    const r = rec.current;
    if (!r) return;
    clearInterval(r.timer);
    rec.current = null;
    if (r.recorder.state !== "inactive") r.recorder.stop();
    setRecording(false);
  };
  const start = async () => {
    if (rec.current) return;
    let stream: MediaStream;
    try { stream = await navigator.mediaDevices.getUserMedia({ audio: true }); } catch { toast("Allow the microphone to record a voice status.", "error"); return; }
    const recorder = new MediaRecorder(stream);
    const chunks: Blob[] = [];
    recorder.ondataavailable = (e) => { if (e.data.size) chunks.push(e.data); };
    const started = now();
    recorder.onstop = () => {
      stream.getTracks().forEach((t) => t.stop());
      const ms = Math.min(SEGMENT_MS, now() - started);
      if (ms < 700) return;
      const blob = new Blob(chunks, { type: (recorder.mimeType || "audio/webm").split(";")[0] });
      setClip({ blob, url: URL.createObjectURL(blob), ms });
    };
    const timer = setInterval(() => {
      const e = now() - started;
      setElapsed(e);
      if (e >= SEGMENT_MS) stop();
    }, 200);
    rec.current = { recorder, started, stream, timer };
    recorder.start();
    setRecording(true); setElapsed(0); setClip(null);
  };
  const post = () => {
    if (!clip) return;
    void enqueueStatus({ items: [{ kind: "AUDIO", blob: clip.blob, durationMs: clip.ms, background: bg }] }).catch((e) => toast(errorText(e, "Your voice status couldn't be added."), "error"));
    onPosted();
  };
  const secs = Math.floor((clip ? clip.ms : elapsed) / 1000);

  return (
    <div className="flex flex-1 flex-col" style={{ backgroundColor: bg }} data-testid="voice-status">
      <div className="flex items-center justify-between p-4">
        <button type="button" aria-label="Close" onClick={onClose} className="flex size-11 items-center justify-center rounded-full bg-black/25"><X className="size-6" /></button>
        <button type="button" aria-label="Background colour" onClick={onCycle} className="flex size-11 items-center justify-center rounded-full bg-black/25"><Palette className="size-5" /></button>
      </div>
      <div className="flex flex-1 flex-col items-center justify-center gap-4 px-8">
        <div className="flex w-full max-w-sm items-center gap-4 rounded-2xl bg-black/15 p-4">
          <span className="flex size-14 items-center justify-center rounded-full bg-black/40"><Mic className={cn("size-6", recording && "animate-pulse text-red-300")} /></span>
          {clip ? <audio src={clip.url} controls className="h-10 min-w-0 flex-1" /> : <span className="flex-1 border-t-2 border-dotted border-white/50" />}
          <span className="w-10 text-right font-mono text-sm">0:{String(secs).padStart(2, "0")}</span>
        </div>
        <p className="text-sm text-white/80">{recording ? "Release to stop · at most 1 minute" : clip ? "Listen, then post — or hold to record again" : "Hold the microphone to record"}</p>
      </div>
      <div className="flex items-center justify-end gap-3 p-4 pb-[max(env(safe-area-inset-bottom),1rem)]">
        {clip ? (
          <button type="button" aria-label="Post voice status" onClick={post} className="flex size-14 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-lg disabled:opacity-50"><Send className="size-6" /></button>
        ) : null}
        <button type="button" aria-label="Hold to record" onPointerDown={(e) => { e.preventDefault(); void start(); }} onPointerUp={stop} onPointerLeave={() => { if (recording) stop(); }} onContextMenu={(e) => e.preventDefault()}
          className={cn("flex size-16 touch-none select-none items-center justify-center rounded-full bg-white text-black shadow-xl transition-transform", recording && "scale-110 bg-red-500 text-white")}>
          <Mic className="size-7" />
        </button>
      </div>
    </div>
  );
}
