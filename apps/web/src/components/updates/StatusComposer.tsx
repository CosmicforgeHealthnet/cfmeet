"use client";

import { useEffect, useRef, useState, type ReactNode } from "react";
import { ChevronRight, Loader2, Music2, Plus, Send, Trash2, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { durationOf, LIMITS, segments, SEGMENT_MS } from "@/services/v2/social.service";
import CollageEditor from "@/components/media-editor/CollageEditor";
import MusicPicker, { MusicChip, type MusicChoice } from "@/components/media-editor/MusicPicker";
import PhotoEditor from "@/components/media-editor/PhotoEditor";
import TextStatusEditor from "@/components/media-editor/TextStatusEditor";
import VideoEditor from "@/components/media-editor/VideoEditor";
import { RoundButton } from "@/components/media-editor/ui";
import { renderAudioClip } from "@/lib/media-editor/audio-render";
import type { Overlay } from "@/lib/media-editor/overlays";
import { recentKindOf, thumbnailOf } from "@/lib/media-editor/recent-media";
import { canProcessVideo, probeVideo, type VideoEdit, type VideoProbe } from "@/lib/media-editor/video";
import { enqueueStatus } from "@/lib/status-publisher/client";
import type { MusicAttachment, StatusItemInput } from "@/lib/status-publisher/types";
import { MUSIC_BLOB, VIDEO_EDIT_PROCESSOR, type VideoEditParams } from "@/lib/status-publisher/video-edit-processor";
import { errorText } from "./shared";
import { textStatusItem } from "./status-adapter";
import { AudienceButton, useStatusAudience, type StatusAudienceHandle } from "./audience";
import MentionCaption, { captionMentions, EMPTY_CAPTION, type CaptionValue } from "./MentionCaption";
import StatusCreator, { type CreatorAction } from "./StatusCreator";
import CameraCapture from "./CameraCapture";
import VoiceStatus from "./VoiceStatus";
import MusicStatus from "./MusicStatus";

export type ComposerStart = "create" | "media" | "text" | "voice" | "music" | "camera";
type Screen = { kind: "create" } | { kind: "text" } | { kind: "voice" } | { kind: "music" } | { kind: "camera" } | { kind: "collage"; files?: File[] } | { kind: "media" };

/** Photos are re-encoded by the photo editor (<= 2560 px, well under the upload limit), so larger originals are fine. */
const MAX_PHOTO_INPUT = 64 * 1024 * 1024;
/** Most photos / videos in one go. */
const MAX_QUEUE = 30;

/**
 * Add status (WhatsApp): the creation screen (Text, Music, Layout, Voice, Camera, recent photos
 * and videos with multi-select), then the editors. Several photos / videos are edited one after
 * another (a strip shows the queue; more can be added) and posted as separate statuses in the
 * order chosen, with the same audience. Posting never blocks: everything is queued
 * (src/lib/status-publisher) and processed / uploaded / published in the background.
 */
export default function StatusComposer({ initial, onClose, onPosted }: { initial: ComposerStart; onClose: () => void; onPosted: () => void }) {
  const [screen, setScreen] = useState<Screen>(() => ({ kind: initial === "media" ? "create" : initial } as Screen));
  const [queue, setQueue] = useState<File[]>([]);
  const [index, setIndex] = useState(0);
  const prepared = useRef<StatusItemInput[]>([]);
  const audience = useStatusAudience();
  const [captionValue, setCaptionValue] = useState<CaptionValue>(EMPTY_CAPTION);
  const more = useRef<HTMLInputElement>(null);

  const withMentions = (item: StatusItemInput): StatusItemInput => {
    const mentions = captionMentions(captionValue, audience.members);
    return mentions.length ? { ...item, mentions } : item;
  };

  /** Queues every prepared status in order (one job each, same audience) and closes. */
  const publish = async (items: StatusItemInput[]) => {
    onClose();
    onPosted();
    for (const item of items) {
      try {
        await enqueueStatus({ items: [item], ...audience.jobOptions() });
      } catch (e) {
        toast(errorText(e, "Your status couldn't be added. Try again."), "error");
      }
    }
    onPosted();
  };
  const postOne = (item: StatusItemInput) => void publish([withMentions(item)]);

  const startQueue = (files: File[]) => {
    const ok = files.filter((f) => {
      const k = recentKindOf(f);
      if (k === "IMAGE" && f.size > MAX_PHOTO_INPUT) { toast(`${f.name} is too big.`, "error"); return false; }
      return Boolean(k);
    }).slice(0, MAX_QUEUE);
    if (!ok.length) return;
    prepared.current = [];
    setQueue(ok);
    setIndex(0);
    setCaptionValue(EMPTY_CAPTION);
    setScreen({ kind: "media" });
  };

  const addMore = (files: File[]) => {
    const ok = files.filter((f) => recentKindOf(f));
    setQueue((q) => [...q, ...ok].slice(0, MAX_QUEUE));
  };

  /** The current item is ready: keep it, then edit the next one or post them all. */
  const next = (item: StatusItemInput | null) => {
    if (item) prepared.current = [...prepared.current, withMentions(item)];
    setCaptionValue(EMPTY_CAPTION);
    if (index + 1 < queue.length) setIndex(index + 1);
    else if (prepared.current.length) void publish(prepared.current);
    else onClose();
  };
  const removeCurrent = () => {
    const q = queue.filter((_, i) => i !== index);
    setCaptionValue(EMPTY_CAPTION);
    if (index >= q.length) {
      setQueue(q);
      if (prepared.current.length) void publish(prepared.current); else setScreen({ kind: "create" });
      return;
    }
    setQueue(q);
  };

  const onAction = (action: CreatorAction, files?: File[]) => {
    if (action === "collage") setScreen({ kind: "collage", files });
    else setScreen({ kind: action });
  };

  const captionField = <MentionCaption value={captionValue} onChange={setCaptionValue} people={audience.mentionable} />;
  const audienceButton = <AudienceButton audience={audience} />;
  const back = () => setScreen({ kind: "create" });
  const hiddenMore = (
    <input ref={more} type="file" accept="image/*,video/*" multiple className="hidden"
      onChange={(e) => { addMore([...(e.target.files ?? [])]); e.target.value = ""; }} />
  );

  switch (screen.kind) {
    case "create":
      return <StatusCreator onClose={onClose} onAction={onAction} onPick={startQueue} />;
    case "camera":
      return <CameraCapture onClose={back} onCaptured={(f) => startQueue([f])} onGallery={back} />;
    case "voice":
      return <VoiceStatus audienceButton={audienceButton} onClose={back} onPost={postOne} />;
    case "music":
      return <MusicStatus audienceButton={audienceButton} captionField={captionField} caption={captionValue.text} onClose={back} onPost={postOne} />;
    case "text":
      return <TextStep audience={audience} onCancel={back} onPost={postOne} />;
    case "collage":
      return <CollageStep files={screen.files} audienceButton={audienceButton} captionField={captionField} caption={captionValue.text} onCancel={back} onPost={postOne} />;
    case "media": {
      const file = queue[index];
      if (!file) return null;
      const last = index === queue.length - 1;
      const strip = (
        <QueueStrip files={queue} index={index} onAdd={() => more.current?.click()} onRemove={removeCurrent} />
      );
      return (
        <>
          {hiddenMore}
          <MediaStep key={`${index}-${file.name}-${file.size}-${file.lastModified}`} file={file} last={last} position={queue.length > 1 ? `${index + 1} of ${queue.length}` : null}
            audienceButton={audienceButton} captionField={captionField} caption={captionValue.text} strip={strip}
            onCancel={() => { if (index === 0) back(); else onClose(); }} onDone={next} />
        </>
      );
    }
  }
}

/** A local video file in a <video> (its object URL made and revoked with the element). */
function FileVideo({ file }: { file: Blob }) {
  const ref = useRef<HTMLVideoElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const url = URL.createObjectURL(file);
    el.src = url;
    return () => { el.removeAttribute("src"); el.load(); URL.revokeObjectURL(url); };
  }, [file]);
  return <video ref={ref} controls playsInline className="max-h-full max-w-full" />;
}

// ------------------------------------------------------------------ queue strip

function StripThumb({ file, active }: { file: File; active: boolean }) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    thumbnailOf(file).then((t) => { if (live) setUrl(t.url || null); }, () => {});
    return () => { live = false; };
  }, [file]);
  return (
    <span className={cn("block size-11 shrink-0 overflow-hidden rounded-lg bg-white/10 ring-2", active ? "ring-primary" : "ring-transparent opacity-70")}>
      {/* eslint-disable-next-line @next/next/no-img-element */}
      {url ? <img src={url} alt="" className="size-full object-cover" /> : null}
    </span>
  );
}

/** The selected photos / videos: which one is being edited, add more, remove this one. */
function QueueStrip({ files, index, onAdd, onRemove }: { files: File[]; index: number; onAdd: () => void; onRemove: () => void }) {
  return (
    <div className="flex items-center gap-2" data-testid="status-queue-strip">
      <ol className="flex min-w-0 flex-1 gap-1.5 overflow-x-auto py-1 [scrollbar-width:none]" aria-label={`Status ${index + 1} of ${files.length}`}>
        {files.map((f, i) => (
          <li key={`${i}-${f.name}`} aria-current={i === index ? "step" : undefined} aria-label={`${i + 1}: ${f.name}${i < index ? ", ready" : ""}`}>
            <StripThumb file={f} active={i === index} />
          </li>
        ))}
      </ol>
      {files.length > 1 ? (
        <button type="button" aria-label="Remove this one" onClick={onRemove} className="flex size-11 shrink-0 items-center justify-center rounded-full bg-white/10 hover:bg-white/20"><Trash2 className="size-5" /></button>
      ) : null}
      <button type="button" aria-label="Add more photos or videos" onClick={onAdd} data-testid="status-add-more" className="flex size-11 shrink-0 items-center justify-center rounded-full bg-white/10 hover:bg-white/20"><Plus className="size-5" /></button>
    </div>
  );
}

// ------------------------------------------------------------------ steps

async function musicClip(music: MusicChoice | null): Promise<MusicAttachment | undefined> {
  if (!music) return undefined;
  const clip = await renderAudioClip(music.file, music.settings);
  return { ...clip, title: music.title };
}

function MusicButton({ music, onClick }: { music: MusicChoice | null; onClick: () => void }) {
  return <RoundButton label={music ? "Edit music" : "Add music"} active={Boolean(music)} onClick={onClick}><Music2 className="size-5" /></RoundButton>;
}

function SendButton({ last, busy, disabled, onClick }: { last: boolean; busy?: boolean; disabled?: boolean; onClick: () => void }) {
  return (
    <button type="button" aria-label={last ? "Post status" : "Next"} data-testid="status-send" disabled={disabled || busy} onClick={onClick}
      className="relative flex size-14 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-lg disabled:opacity-50 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white focus-visible:ring-offset-2 focus-visible:ring-offset-black">
      {busy ? <Loader2 className="size-6 animate-spin" /> : last ? <Send className="size-6" /> : <ChevronRight className="size-7" />}
    </button>
  );
}

function TextStep({ audience, onCancel, onPost }: { audience: StatusAudienceHandle; onCancel: () => void; onPost: (item: StatusItemInput) => void }) {
  const [music, setMusic] = useState<MusicChoice | null>(null);
  const [picking, setPicking] = useState(false);
  return (
    <>
      <TextStatusEditor onCancel={onCancel}
        toolbarExtra={<MusicButton music={music} onClick={() => setPicking(true)} />}
        footer={<div className="flex flex-wrap items-center gap-2"><AudienceButton audience={audience} />{music ? <MusicChip choice={music} onClick={() => setPicking(true)} /> : null}</div>}
        onDone={async (s) => {
          try {
            onPost({ ...textStatusItem(s), music: await musicClip(music) });
          } catch {
            toast("The music couldn't be prepared. Try again.", "error");
          }
        }} />
      {picking ? <MusicPicker initial={music} onDone={(c) => { setMusic(c); setPicking(false); }} onRemove={music ? () => { setMusic(null); setPicking(false); } : undefined} onClose={() => setPicking(false)} /> : null}
    </>
  );
}

function CollageStep({ files, audienceButton, captionField, caption, onCancel, onPost }: {
  files?: File[]; audienceButton: ReactNode; captionField: ReactNode; caption: string; onCancel: () => void; onPost: (item: StatusItemInput) => void;
}) {
  const [music, setMusic] = useState<MusicChoice | null>(null);
  const [picking, setPicking] = useState(false);
  return (
    <>
      <CollageEditor files={files} onCancel={onCancel} doneLabel="Post status" doneIcon={<Send className="size-6" />}
        toolbarExtra={<MusicButton music={music} onClick={() => setPicking(true)} />}
        footer={<div className="space-y-2"><div className="flex flex-wrap items-center justify-between gap-2">{music ? <MusicChip choice={music} onClick={() => setPicking(true)} /> : <span />}{audienceButton}</div>{captionField}</div>}
        onDone={async (r) => {
          if (r.blob.size > LIMITS.IMAGE) { toast("Photos can be at most 8 MB.", "error"); return; }
          try {
            onPost({ kind: "IMAGE", blob: r.blob, preview: r.blob, caption: caption.trim() || undefined, music: await musicClip(music) });
          } catch {
            toast("The music couldn't be prepared. Try again.", "error");
          }
        }} />
      {picking ? <MusicPicker initial={music} onDone={(c) => { setMusic(c); setPicking(false); }} onRemove={music ? () => { setMusic(null); setPicking(false); } : undefined} onClose={() => setPicking(false)} /> : null}
    </>
  );
}

type VideoState = { probe: VideoProbe } | { fallback: number } | { error: string } | null;

/** One photo or video of the queue, in its editor. */
function MediaStep({ file, last, position, audienceButton, captionField, caption, strip, onCancel, onDone }: {
  file: File; last: boolean; position: string | null; audienceButton: ReactNode; captionField: ReactNode; caption: string; strip: ReactNode;
  onCancel: () => void; onDone: (item: StatusItemInput | null) => void;
}) {
  const kind = recentKindOf(file);
  const [music, setMusic] = useState<MusicChoice | null>(null);
  const [picking, setPicking] = useState(false);
  const [video, setVideo] = useState<VideoState>(null);
  const [edit, setEdit] = useState<VideoEdit | null>(null);
  const overlays = useRef<Overlay[]>([]);
  const [busy, setBusy] = useState(false);

  // A video this browser can process opens in the editor; otherwise the original is uploaded once.
  useEffect(() => {
    if (kind !== "VIDEO") return;
    let live = true;
    void (async () => {
      let p: VideoProbe | null = null;
      if (await canProcessVideo()) {
        p = await probeVideo(file).catch(() => null);
        if (p && !p.canDecode) p = null;
      }
      if (!live) return;
      if (p) { setVideo({ probe: p }); return; }
      if (file.size > LIMITS.VIDEO) { setVideo({ error: "Videos can be at most 64 MB in this browser." }); return; }
      try {
        const d = await durationOf(file);
        if (live) setVideo({ fallback: d });
      } catch {
        if (live) setVideo({ error: "This video can't be played here." });
      }
    })();
    return () => { live = false; };
  }, [file, kind]);

  const picker = picking ? (
    <MusicPicker initial={music} onDone={(c) => { setMusic(c); setPicking(false); }} onRemove={music ? () => { setMusic(null); setPicking(false); } : undefined} onClose={() => setPicking(false)}
      maxMs={video && "probe" in video ? Math.max(60_000, edit ? edit.range.endMs - edit.range.startMs : video.probe.durationMs) : undefined}
      defaultMs={video && "probe" in video && edit ? Math.min(edit.range.endMs - edit.range.startMs, 60_000) : undefined} />
  ) : null;
  const top = position ? <span className="pointer-events-none fixed left-1/2 top-[max(env(safe-area-inset-top),0.75rem)] z-[90] mt-14 -translate-x-1/2 rounded-full bg-black/55 px-3 py-1 text-xs font-semibold tabular-nums text-white">{position}</span> : null;

  if (kind === "IMAGE") {
    return (
      <>
        {top}
        <PhotoEditor file={file} onCancel={onCancel} doneLabel={last ? "Post status" : "Next"} doneIcon={last ? undefined : <ChevronRight className="size-7" />}
          toolbarExtra={<MusicButton music={music} onClick={() => setPicking(true)} />} doneExtra={audienceButton}
          footer={<div className="space-y-2">{strip}{music ? <MusicChip choice={music} onClick={() => setPicking(true)} /> : null}{captionField}</div>}
          onDone={async (r) => {
            if (r.blob.size > LIMITS.IMAGE) { toast("Photos can be at most 8 MB.", "error"); return; }
            try {
              onDone({ kind: "IMAGE", blob: r.blob, preview: r.blob, caption: caption.trim() || undefined, music: await musicClip(music) });
            } catch {
              toast("The music couldn't be prepared. Try again.", "error");
            }
          }} />
        {picker}
      </>
    );
  }

  if (video && "probe" in video) {
    const probe = video.probe;
    const post = () => {
      if (!edit) return;
      const params: VideoEditParams = {
        edit, durationMs: probe.durationMs,
        ...(overlays.current.length ? { overlays: overlays.current } : {}),
        ...(music ? { music: music.settings } : {}),
      };
      onDone({
        kind: "VIDEO", caption,
        process: { processor: VIDEO_EDIT_PROCESSOR, input: file, params, ...(music ? { blobs: { [MUSIC_BLOB]: music.file } } : {}) },
      });
    };
    return (
      <>
        {top}
        <VideoEditor file={file} probe={probe} onChange={setEdit} onClose={onCancel} onOverlaysChange={(o) => { overlays.current = o; }}
          estimate={{ hasMusic: Boolean(music) }}
          toolbarExtra={<MusicButton music={music} onClick={() => setPicking(true)} />}
          footer={(
            <div className="space-y-2 bg-gradient-to-t from-black via-black/80 to-transparent p-3" data-testid="status-composer">
              {strip}
              <div className="flex flex-wrap items-center justify-between gap-2">{music ? <MusicChip choice={music} onClick={() => setPicking(true)} /> : <span />}{audienceButton}</div>
              <div className="flex items-center gap-2">{captionField}<SendButton last={last} disabled={!edit} onClick={post} /></div>
            </div>
          )} />
        {picker}
      </>
    );
  }

  // Loading, error, or the fallback (no in-browser video processing): the original, posted as 1-minute parts.
  const fallback = video && "fallback" in video ? video.fallback : null;
  const parts = fallback ? segments(fallback).length : 1;
  return (
    <div className="fixed inset-0 z-[70] flex flex-col bg-black text-white" role="dialog" aria-label="Video status" data-testid="status-composer">
      {top}
      <div className="absolute inset-x-0 top-0 z-10 flex items-center justify-between p-3 pt-[max(env(safe-area-inset-top),0.75rem)]">
        <button type="button" aria-label="Close" onClick={onCancel} className="flex size-11 items-center justify-center rounded-full bg-black/30 backdrop-blur"><X className="size-6" /></button>
      </div>
      <div className="flex min-h-0 flex-1 items-center justify-center">
        {fallback ? <FileVideo file={file} />
          : video && "error" in video ? <p className="max-w-xs text-center text-sm text-white/80" role="alert">{video.error}</p>
            : <Loader2 className="size-8 animate-spin text-white/70" aria-label="Opening video" />}
      </div>
      <div className="space-y-3 bg-black/90 p-3 pb-[max(env(safe-area-inset-bottom),0.75rem)]">
        {strip}
        {fallback && fallback > SEGMENT_MS ? <p className="text-center text-xs text-white/70" data-testid="status-segments">Longer than a minute: it will be posted as {parts} statuses of up to 1 minute each.</p> : null}
        <div className="flex justify-end">{audienceButton}</div>
        <div className="flex items-center gap-2">
          {captionField}
          {video && "error" in video ? (
            <button type="button" onClick={() => onDone(null)} className="h-14 shrink-0 rounded-full bg-white/10 px-5 text-sm font-semibold">Skip</button>
          ) : (
            <SendButton last={last} busy={busy} disabled={!fallback} onClick={() => {
              if (!fallback) return;
              setBusy(true);
              onDone({ kind: "VIDEO", source: file, durationMs: fallback, ranges: segments(fallback), caption });
            }} />
          )}
        </div>
      </div>
    </div>
  );
}
