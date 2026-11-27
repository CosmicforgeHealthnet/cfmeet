"use client";

import { useEffect, useRef, useState } from "react";
import { ImagePlus, Mic, Palette, Send, Type, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { durationOf, kindOf, LIMITS, segments, SEGMENT_MS, socialService, type MediaKind } from "@/services/v2/social.service";
import VideoEditor from "@/components/media-editor/VideoEditor";
import { canProcessVideo, isAbortError, isVideoProcessingUnsupported, probeVideo, renderVideoEdit, type VideoEdit, type VideoProbe } from "@/lib/media-editor/video";
import { BACKGROUNDS, errorText, FONTS } from "./shared";

type Mode = "text" | "media" | "voice";

/** Wall-clock milliseconds (event handlers and timers only). */
const now = () => Date.now();

/**
 * Add status (WhatsApp): text on a colour, a photo or video from the device with a caption,
 * or a voice note (hold to record, at most a minute). Where the browser can process video
 * (WebCodecs), a picked video opens in the video editor (trim, parts, crop, rotate, sound) and
 * is posted as small ≤ 720p MP4 parts of at most a minute each, one status per part. Elsewhere
 * the original is uploaded once and posted as consecutive one-minute time ranges.
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
  const [busy, setBusy] = useState<number | null>(null);
  /** What a video post is doing ("Processing 1 of 3…", "Uploading… 40%"). */
  const [stage, setStage] = useState<string | null>(null);
  /** Set when the video can be edited and processed in this browser. */
  const [probe, setProbe] = useState<VideoProbe | null>(null);
  const [edit, setEdit] = useState<VideoEdit | null>(null);
  const picker = useRef<HTMLInputElement>(null);
  const work = useRef<AbortController | null>(null);

  useEffect(() => { if (initial === "media") picker.current?.click(); }, [initial]);
  useEffect(() => () => { if (preview) URL.revokeObjectURL(preview); }, [preview]);
  useEffect(() => () => work.current?.abort(), []);

  const close = () => { work.current?.abort(); onClose(); };

  const pick = async (f: File | undefined) => {
    if (!f) return;
    const k = kindOf(f);
    if (!k || k === "AUDIO") { toast("Choose a photo or a video.", "error"); return; }
    // A video this browser can process is re-encoded small before upload, so the raw size limit
    // only applies when the original itself has to go up.
    let p: VideoProbe | null = null;
    if (k === "VIDEO" && (await canProcessVideo())) {
      p = await probeVideo(f).catch(() => null);
      if (p && !p.canDecode) p = null;
    }
    if (!p && f.size > LIMITS[k]) { toast(k === "VIDEO" ? "Videos can be at most 64 MB." : "Photos can be at most 8 MB.", "error"); return; }
    let d: number | null = p ? p.durationMs : null;
    if (k === "VIDEO" && !p) {
      try { d = await durationOf(f); } catch { toast("This video can't be played here.", "error"); return; }
    }
    setFile(f); setKind(k); setDuration(d); setProbe(p); setEdit(null); setPreview(URL.createObjectURL(f)); setMode("media");
  };

  const post = async () => {
    if (busy !== null) return;
    setBusy(0);
    try {
      if (mode === "text") {
        if (!text.trim()) return;
        await socialService.postStatus({ kind: "TEXT", text: text.trim(), background: BACKGROUNDS[bg], font });
      } else if (mode === "media" && file && kind === "VIDEO" && probe && edit) {
        await postEditedVideo(file, probe, edit);
      } else if (mode === "media" && file && kind) {
        const media = await socialService.upload(file, "STATUS", kind, duration, (p) => setBusy(p));
        if (kind === "VIDEO" && duration) {
          const parts = segments(duration);
          for (const [i, s] of parts.entries()) {
            // The caption goes with the first part only, as WhatsApp does.
            await socialService.postStatus({ kind: "VIDEO", mediaId: media.mediaId, startMs: s.startMs, endMs: s.endMs, caption: i === 0 && caption.trim() ? caption.trim() : undefined });
          }
        } else {
          await socialService.postStatus({ kind: "IMAGE", mediaId: media.mediaId, caption: caption.trim() || undefined });
        }
      }
      toast("Status posted");
      onPosted();
      onClose();
    } catch (e) {
      if (!isAbortError(e)) toast(errorText(e, "Your status couldn't be posted. Try again."), "error");
    } finally {
      setBusy(null);
      setStage(null);
      work.current = null;
    }
  };

  /** Edited video: each part is exported to a small MP4, uploaded, then posted in order. */
  const postEditedVideo = async (source: File, info: VideoProbe, v: VideoEdit) => {
    const ac = new AbortController();
    work.current = ac;
    const text = caption.trim() || undefined;
    let blobs: Blob[];
    try {
      blobs = await renderVideoEdit(source, v, {
        probe: info,
        signal: ac.signal,
        onProgress: ({ part, parts, overall }) => { setStage(parts > 1 ? `Processing ${part + 1} of ${parts}…` : "Processing…"); setBusy(overall * 0.5); },
      });
    } catch (e) {
      if (!isVideoProcessingUnsupported(e)) throw e;
      // Cannot encode here after all: send the original and post the chosen time ranges.
      if (source.size > LIMITS.VIDEO) throw new Error("Videos can be at most 64 MB.");
      setStage("Uploading…");
      const media = await socialService.upload(source, "STATUS", "VIDEO", info.durationMs, (p) => { setStage(`Uploading… ${Math.round(p * 100)}%`); setBusy(p); });
      for (const [i, s] of v.segments.entries()) {
        await socialService.postStatus({ kind: "VIDEO", mediaId: media.mediaId, startMs: s.startMs, endMs: s.endMs, caption: i === 0 ? text : undefined });
      }
      return;
    }
    const total = blobs.reduce((sum, b) => sum + b.size, 0) || 1;
    let sent = 0;
    const mediaIds: string[] = [];
    for (const [i, blob] of blobs.entries()) {
      if (ac.signal.aborted) throw new DOMException("Aborted", "AbortError");
      const len = v.segments[i].endMs - v.segments[i].startMs;
      const media = await socialService.upload(blob, "STATUS", "VIDEO", len, (p) => {
        const overall = (sent + p * blob.size) / total;
        setStage(`Uploading… ${Math.round(overall * 100)}%`);
        setBusy(0.5 + overall * 0.5);
      });
      sent += blob.size;
      mediaIds.push(media.mediaId);
    }
    // Posted only once every part is up, so the parts appear together and in order.
    for (const [i, mediaId] of mediaIds.entries()) {
      const len = v.segments[i].endMs - v.segments[i].startMs;
      // The caption goes with the first part only, as WhatsApp does.
      await socialService.postStatus({ kind: "VIDEO", mediaId, startMs: 0, endMs: len, caption: i === 0 ? text : undefined });
    }
  };

  const parts = kind === "VIDEO" && duration ? segments(duration).length : 1;

  if (mode === "media" && file && kind === "VIDEO" && probe) {
    return (
      <VideoEditor file={file} probe={probe} onChange={setEdit} onClose={close}
        footer={(
          <div className="space-y-2 bg-gradient-to-t from-black via-black/80 to-transparent p-3" data-testid="status-composer">
            {stage ? <p className="text-center text-sm font-medium text-white/90" role="status" data-testid="status-progress">{stage}</p> : null}
            <div className="flex items-center gap-2">
              <input value={caption} maxLength={700} onChange={(e) => setCaption(e.target.value)} placeholder="Add a caption…" disabled={busy !== null}
                className="h-12 min-w-0 flex-1 rounded-full bg-white/10 px-5 text-base text-white placeholder:text-white/50 focus:outline-none focus-visible:ring-2 focus-visible:ring-primary" />
              <button type="button" aria-label="Post status" data-testid="status-send" disabled={busy !== null || !edit} onClick={() => void post()}
                className="relative flex size-14 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-lg disabled:opacity-50">
                {busy !== null ? <span className="text-xs font-bold">{Math.round(busy * 100)}%</span> : <Send className="size-6" />}
              </button>
            </div>
          </div>
        )} />
    );
  }

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
              <button type="button" aria-label="Post status" data-testid="status-send" disabled={busy !== null || (mode === "text" ? !text.trim() : !file)} onClick={() => void post()}
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
