"use client";

import { useEffect, useRef, useState } from "react";
import { ImagePlus, Mic, Palette, Send, Type, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import PhotoEditor from "@/components/media-editor/PhotoEditor";
import TextStatusEditor from "@/components/media-editor/TextStatusEditor";
import { durationOf, kindOf, LIMITS, segments, SEGMENT_MS, socialService, type MediaKind } from "@/services/v2/social.service";
import { BACKGROUNDS, errorText } from "./shared";
import { textStatusToCreate } from "./status-adapter";

type Mode = "text" | "media" | "voice";

/** Wall-clock milliseconds (event handlers and timers only). */
const now = () => Date.now();

/** Photos are re-encoded by the editor (<= 2560 px, well under the upload limit), so larger originals are fine. */
const MAX_PHOTO_INPUT = 64 * 1024 * 1024;

/**
 * Add status (WhatsApp): text on a colour or gradient (TextStatusEditor), a photo edited in
 * PhotoEditor (crop, filters, text, stickers, drawing) with a caption, a video from the device
 * with a caption, or a voice note (hold to record, at most a minute). A video longer than a
 * minute is uploaded once and posted as consecutive one-minute statuses.
 */
export default function StatusComposer({ initial, onClose, onPosted }: { initial: Mode; onClose: () => void; onPosted: () => void }) {
  const [mode, setMode] = useState<Mode>(initial);
  const [bg, setBg] = useState(0);
  const [file, setFile] = useState<File | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [kind, setKind] = useState<MediaKind | null>(null);
  const [duration, setDuration] = useState<number | null>(null);
  const [caption, setCaption] = useState("");
  const [busy, setBusy] = useState<number | null>(null);
  const picker = useRef<HTMLInputElement>(null);

  useEffect(() => { if (initial === "media") picker.current?.click(); }, [initial]);
  useEffect(() => () => { if (preview) URL.revokeObjectURL(preview); }, [preview]);

  const pick = async (f: File | undefined) => {
    if (!f) return;
    const k = kindOf(f);
    if (!k || k === "AUDIO") { toast("Choose a photo or a video.", "error"); return; }
    if (k === "IMAGE" ? f.size > MAX_PHOTO_INPUT : f.size > LIMITS[k]) { toast(k === "VIDEO" ? "Videos can be at most 64 MB." : "This photo is too big.", "error"); return; }
    let d: number | null = null;
    if (k === "VIDEO") {
      try { d = await durationOf(f); } catch { toast("This video can't be played here.", "error"); return; }
    }
    setFile(f); setKind(k); setDuration(d); setPreview(k === "VIDEO" ? URL.createObjectURL(f) : null); setMode("media");
  };

  const done = () => { toast("Status posted"); onPosted(); onClose(); };

  const postText = async (body: Parameters<typeof textStatusToCreate>[0]) => {
    try {
      await socialService.postStatus(textStatusToCreate(body));
      done();
    } catch (e) {
      toast(errorText(e, "Your status couldn't be posted. Try again."), "error");
    }
  };

  const postPhoto = async (blob: Blob) => {
    if (busy !== null) return;
    setBusy(0);
    try {
      if (blob.size > LIMITS.IMAGE) { toast("Photos can be at most 8 MB.", "error"); return; }
      const media = await socialService.upload(blob, "STATUS", "IMAGE", null, (p) => setBusy(p));
      await socialService.postStatus({ kind: "IMAGE", mediaId: media.mediaId, caption: caption.trim() || undefined });
      done();
    } catch (e) {
      toast(errorText(e, "Your status couldn't be posted. Try again."), "error");
    } finally {
      setBusy(null);
    }
  };

  const postVideo = async () => {
    if (busy !== null || !file || kind !== "VIDEO" || !duration) return;
    setBusy(0);
    try {
      const media = await socialService.upload(file, "STATUS", kind, duration, (p) => setBusy(p));
      const parts = segments(duration);
      for (const [i, s] of parts.entries()) {
        // The caption goes with the first part only, as WhatsApp does.
        await socialService.postStatus({ kind: "VIDEO", mediaId: media.mediaId, startMs: s.startMs, endMs: s.endMs, caption: i === 0 && caption.trim() ? caption.trim() : undefined });
      }
      done();
    } catch (e) {
      toast(errorText(e, "Your status couldn't be posted. Try again."), "error");
    } finally {
      setBusy(null);
    }
  };

  const parts = kind === "VIDEO" && duration ? segments(duration).length : 1;
  const tabs = (
    <div className="flex flex-1 gap-1">
      {([["media", "Photo", ImagePlus], ["text", "Text", Type], ["voice", "Voice", Mic]] as const).map(([m, label, Icon]) => (
        <button key={m} type="button" onClick={() => (m === "media" && !file ? picker.current?.click() : setMode(m))}
          className={cn("flex h-10 items-center gap-1.5 rounded-full px-3.5 text-sm font-semibold", mode === m ? "bg-white/20" : "text-white/80 hover:bg-white/10")}>
          <Icon className="size-4" />{label}
        </button>
      ))}
    </div>
  );
  const captionField = (
    <input value={caption} maxLength={700} onChange={(e) => setCaption(e.target.value)} placeholder="Add a caption…" aria-label="Caption"
      className="h-12 w-full rounded-full bg-white/10 px-5 text-base text-white placeholder:text-white/50 focus:outline-none focus-visible:ring-2 focus-visible:ring-primary" />
  );

  return (
    <div className="fixed inset-0 z-[70] flex flex-col bg-black text-white" role="dialog" aria-label="Add status" data-testid="status-composer">
      <input ref={picker} type="file" accept="image/*,video/*" className="hidden" onChange={(e) => { void pick(e.target.files?.[0]); e.target.value = ""; }} />
      {mode === "voice" ? (
        <VoiceStatus bg={BACKGROUNDS[bg]} onCycle={() => setBg((bg + 1) % BACKGROUNDS.length)} onClose={onClose} onPosted={() => { onPosted(); onClose(); }} />
      ) : mode === "text" ? (
        <TextStatusEditor onCancel={onClose} onDone={postText} footer={tabs} />
      ) : file && kind === "IMAGE" ? (
        <PhotoEditor file={file} onCancel={() => { setFile(null); setKind(null); }} onDone={(r) => postPhoto(r.blob)} doneLabel="Post status"
          footer={<>{captionField}{busy !== null ? <p className="text-center text-xs text-white/70">Uploading {Math.round(busy * 100)}%</p> : null}</>} />
      ) : (
        <>
          <div className="absolute inset-x-0 top-0 z-10 flex items-center justify-between p-4">
            <button type="button" aria-label="Close" onClick={onClose} className="flex size-11 items-center justify-center rounded-full bg-black/30 backdrop-blur"><X className="size-6" /></button>
          </div>

          <div className="flex min-h-0 flex-1 items-center justify-center bg-black">
            {!preview ? (
              <button type="button" onClick={() => picker.current?.click()} className="flex flex-col items-center gap-3 text-white/80"><ImagePlus className="size-12" />Choose a photo or video</button>
            ) : (
              <video src={preview} controls playsInline className="max-h-full max-w-full" />
            )}
          </div>

          <div className="space-y-3 bg-black/90 p-3 pb-[max(env(safe-area-inset-bottom),0.75rem)]">
            {preview ? (
              <>
                {kind === "VIDEO" && duration && duration > SEGMENT_MS ? (
                  <p className="text-center text-xs text-white/70" data-testid="status-segments">Longer than a minute: it will be posted as {parts} statuses of up to 1 minute each.</p>
                ) : null}
                {captionField}
              </>
            ) : null}
            <div className="flex items-center gap-2">
              {tabs}
              <button type="button" aria-label="Post status" data-testid="status-send" disabled={busy !== null || !file} onClick={() => void postVideo()}
                className="relative flex size-14 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-lg disabled:opacity-50">
                {busy !== null ? <span className="text-xs font-bold">{Math.round(busy * 100)}%</span> : <Send className="size-6" />}
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
  const [busy, setBusy] = useState(false);
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
    if (rec.current || busy) return;
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
  const post = async () => {
    if (!clip || busy) return;
    setBusy(true);
    try {
      const media = await socialService.upload(clip.blob, "STATUS", "AUDIO", clip.ms);
      await socialService.postStatus({ kind: "AUDIO", mediaId: media.mediaId });
      toast("Status posted");
      onPosted();
    } catch (e) {
      toast(errorText(e, "Your voice status couldn't be posted."), "error");
    } finally {
      setBusy(false);
    }
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
          <button type="button" aria-label="Post voice status" disabled={busy} onClick={() => void post()} className="flex size-14 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-lg disabled:opacity-50"><Send className="size-6" /></button>
        ) : null}
        <button type="button" aria-label="Hold to record" onPointerDown={(e) => { e.preventDefault(); void start(); }} onPointerUp={stop} onPointerLeave={() => { if (recording) stop(); }} onContextMenu={(e) => e.preventDefault()}
          className={cn("flex size-16 touch-none select-none items-center justify-center rounded-full bg-white text-black shadow-xl transition-transform", recording && "scale-110 bg-red-500 text-white")}>
          <Mic className="size-7" />
        </button>
      </div>
    </div>
  );
}
