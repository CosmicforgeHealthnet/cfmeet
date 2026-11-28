"use client";

import { useEffect, useRef, useState } from "react";
import { ImagePlus, Mic, Palette, Send, Type, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { durationOf, kindOf, LIMITS, segments, SEGMENT_MS, type MediaKind } from "@/services/v2/social.service";
import PhotoEditor from "@/components/media-editor/PhotoEditor";
import TextStatusEditor from "@/components/media-editor/TextStatusEditor";
import VideoEditor from "@/components/media-editor/VideoEditor";
import { canProcessVideo, probeVideo, type VideoEdit, type VideoProbe } from "@/lib/media-editor/video";
import type { TextStatus } from "@/lib/media-editor/text-status";
import { enqueueStatus } from "@/lib/status-publisher/client";
import type { StatusItemInput } from "@/lib/status-publisher/types";
import { VIDEO_EDIT_PROCESSOR } from "@/lib/status-publisher/video-edit-processor";
import { BACKGROUNDS, errorText } from "./shared";
import { textStatusToCreate } from "./status-adapter";
import { AudienceButton, useStatusAudience, type StatusAudienceHandle } from "./audience";
import MentionCaption, { captionMentions, EMPTY_CAPTION, type CaptionValue } from "./MentionCaption";

type Mode = "text" | "media" | "voice";

/** Wall-clock milliseconds (event handlers and timers only). */
const now = () => Date.now();

/** Photos are re-encoded by the photo editor (<= 2560 px, well under the upload limit), so larger originals are fine. */
const MAX_PHOTO_INPUT = 64 * 1024 * 1024;

/**
 * Add status (WhatsApp): text on a colour or gradient (TextStatusEditor), a photo edited in
 * PhotoEditor (crop, filters, text, stickers, drawing) with a caption, a video with a caption,
 * or a voice note (hold to record, at most a minute). Where the browser can process video
 * (WebCodecs), a picked video opens in the video editor (trim, parts, crop, rotate, sound) and
 * is posted as small ≤ 720p MP4 parts of at most a minute each, one status per part. Elsewhere
 * the original is uploaded once and posted as consecutive one-minute time ranges.
 * Posting never blocks: items are queued (src/lib/status-publisher) and the editor closes.
 * Every editor has the audience button beside Publish (this status only, every split part of
 * it) and captions take @mentions of people the audience reaches.
 */
export default function StatusComposer({ initial, onClose, onPosted }: { initial: Mode; onClose: () => void; onPosted: () => void }) {
  const [mode, setMode] = useState<Mode>(initial);
  const [bg, setBg] = useState(0);
  const [file, setFile] = useState<File | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [kind, setKind] = useState<MediaKind | null>(null);
  const [duration, setDuration] = useState<number | null>(null);
  const [captionValue, setCaptionValue] = useState<CaptionValue>(EMPTY_CAPTION);
  const caption = captionValue.text;
  const audience = useStatusAudience();
  /** Set when the video can be edited and processed in this browser. */
  const [probe, setProbe] = useState<VideoProbe | null>(null);
  const [edit, setEdit] = useState<VideoEdit | null>(null);
  const picker = useRef<HTMLInputElement>(null);

  useEffect(() => { if (initial === "media") picker.current?.click(); }, [initial]);
  useEffect(() => () => { if (preview) URL.revokeObjectURL(preview); }, [preview]);

  const pick = async (f: File | undefined) => {
    if (!f) return;
    const k = kindOf(f);
    if (!k || k === "AUDIO") { toast("Choose a photo or a video.", "error"); return; }
    if (k === "IMAGE") {
      if (f.size > MAX_PHOTO_INPUT) { toast("This photo is too big.", "error"); return; }
      setFile(f); setKind(k); setDuration(null); setProbe(null); setEdit(null); setPreview(null); setMode("media");
      return;
    }
    // A video this browser can process is re-encoded small before upload, so the raw size limit
    // only applies when the original itself has to go up.
    let p: VideoProbe | null = null;
    if (await canProcessVideo()) {
      p = await probeVideo(f).catch(() => null);
      if (p && !p.canDecode) p = null;
    }
    if (!p && f.size > LIMITS[k]) { toast("Videos can be at most 64 MB.", "error"); return; }
    let d: number | null = p ? p.durationMs : null;
    if (!p) {
      try { d = await durationOf(f); } catch { toast("This video can't be played here.", "error"); return; }
    }
    setFile(f); setKind(k); setDuration(d); setProbe(p); setEdit(null); setPreview(URL.createObjectURL(f)); setMode("media");
  };

  // Non-blocking (WhatsApp): queued, then processed / uploaded / published in the background
  // (src/lib/status-publisher); the Updates card shows "Adding…" and its progress.
  const enqueue = (item: StatusItemInput) => {
    const mentions = "caption" in item && item.caption ? captionMentions(captionValue, audience.members) : [];
    const withMentions: StatusItemInput = mentions.length ? { ...item, mentions } : item;
    void enqueueStatus({ items: [withMentions], ...audience.jobOptions() }).catch((e) => toast(errorText(e, "Your status couldn't be added. Try again."), "error"));
    onPosted();
    onClose();
  };

  // Text: today's API fields only (the adapter keeps the richer ones for later).
  const postText = (s: TextStatus) => enqueue(textStatusToCreate(s));

  const postPhoto = (blob: Blob) => {
    if (blob.size > LIMITS.IMAGE) { toast("Photos can be at most 8 MB.", "error"); return; }
    enqueue({ kind: "IMAGE", blob, caption: caption.trim() || undefined, preview: blob });
  };

  // An edited video is rendered to ≤ 60 s MP4 parts in the background (named processor, so it
  // resumes after a reload); otherwise the original goes up once, posted as time ranges.
  const postVideo = () => {
    if (!file || kind !== "VIDEO") return;
    if (probe && edit) enqueue({ kind: "VIDEO", process: { processor: VIDEO_EDIT_PROCESSOR, input: file, params: { edit, durationMs: probe.durationMs } }, caption });
    else if (duration) enqueue({ kind: "VIDEO", source: file, durationMs: duration, ranges: segments(duration), caption }); // caption: first part only
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
  const captionField = <MentionCaption value={captionValue} onChange={setCaptionValue} people={audience.mentionable} />;
  const audienceButton = <AudienceButton audience={audience} />;

  if (mode === "media" && file && kind === "VIDEO" && probe) {
    return (
      <VideoEditor file={file} probe={probe} onChange={setEdit} onClose={onClose}
        footer={(
          <div className="space-y-2 bg-gradient-to-t from-black via-black/80 to-transparent p-3" data-testid="status-composer">
            <div className="flex justify-end">{audienceButton}</div>
            <div className="flex items-center gap-2">
              {captionField}
              <button type="button" aria-label="Post status" data-testid="status-send" disabled={!edit} onClick={postVideo}
                className="relative flex size-14 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-lg disabled:opacity-50">
                <Send className="size-6" />
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
        <VoiceStatus bg={BACKGROUNDS[bg]} audience={audience} onCycle={() => setBg((bg + 1) % BACKGROUNDS.length)} onClose={onClose} onPosted={() => { onPosted(); onClose(); }} />
      ) : mode === "text" ? (
        <TextStatusEditor onCancel={onClose} onDone={postText} footer={<div className="flex flex-wrap items-center gap-2">{tabs}{audienceButton}</div>} />
      ) : file && kind === "IMAGE" ? (
        <PhotoEditor file={file} onCancel={() => { setFile(null); setKind(null); }} onDone={(r) => postPhoto(r.blob)} doneLabel="Post status" doneExtra={audienceButton} footer={captionField} />
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
            {preview ? <div className="flex justify-end">{audienceButton}</div> : null}
            <div className="flex items-center gap-2">
              {tabs}
              <button type="button" aria-label="Post status" data-testid="status-send" disabled={!file || kind !== "VIDEO"} onClick={postVideo}
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
function VoiceStatus({ bg, audience, onCycle, onClose, onPosted }: { bg: string; audience: StatusAudienceHandle; onCycle: () => void; onClose: () => void; onPosted: () => void }) {
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
    void enqueueStatus({ items: [{ kind: "AUDIO", blob: clip.blob, durationMs: clip.ms, background: bg }], ...audience.jobOptions() }).catch((e) => toast(errorText(e, "Your voice status couldn't be added."), "error"));
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
        <span className="mr-auto min-w-0"><AudienceButton audience={audience} /></span>
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
