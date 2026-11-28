"use client";

import { useCallback, useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import {
  ChevronLeft,
  ChevronRight,
  Check,
  Crop,
  Eraser,
  FlipHorizontal2,
  Loader2,
  Pause,
  Pencil,
  Play,
  Redo2,
  RotateCcw,
  RotateCw,
  Smile,
  Trash2,
  Type,
  Undo2,
  Volume2,
  VolumeX,
  X,
} from "lucide-react";
import { cn } from "@/lib/utils";
import {
  CROP_ASPECTS,
  aspectCrop,
  clampCrop,
  createHistory,
  editDurationMs,
  estimateEditSize,
  formatBytes,
  formatDuration,
  historyReducer,
  initialVideoEdit,
  probeVideo,
  rotatedSize,
  timelineThumbnails,
  type CropAspect,
  type EditHistory,
  type HistoryAction,
  type NormalizedCrop,
  type VideoEdit,
  type VideoEditAction,
  type VideoProbe,
} from "@/lib/media-editor/video";

import { BRUSH_SIZES, createSticker, createText, OVERLAY_COLORS, updateOverlay, type Overlay, type TextOverlay } from "@/lib/media-editor/overlays";
import { hasVisibleOverlays } from "@/lib/media-editor/video-overlays";
import { fadeGain, type MusicSettings } from "@/lib/media-editor/audio";
import EmojiPicker from "./EmojiPicker";
import OverlayLayer from "./OverlayLayer";
import TextOverlayEditor, { DEFAULT_TEXT_DRAFT, type TextDraft } from "./TextOverlayEditor";
import { ColorRow, Segmented, useHistory } from "./ui";

export type { VideoEdit } from "@/lib/media-editor/video";

export type VideoEditorProps = {
  /** The picked video. */
  file: File | Blob;
  /** Starting edit (e.g. when reopening the editor). Read on mount; remount (`key`) to replace it. */
  value?: VideoEdit;
  /** Every change (and the starting edit once the video is read). */
  onChange: (edit: VideoEdit) => void;
  /** Rendered over the preview, in the output frame's box (text / stickers / drawing layer). */
  overlaySlot?: ReactNode;
  /** Extra round buttons for the top-right toolbar (e.g. music). */
  toolbarExtra?: ReactNode;
  /** Bottom area supplied by the parent (caption field, send button). */
  footer?: ReactNode;
  onClose: () => void;
  /** Already-read metadata of `file`, if the parent has it. */
  probe?: VideoProbe;
  /** The file could not be read / decoded here (the parent may fall back). */
  onError?: (error: unknown) => void;
  /** Included in the size estimate. */
  estimate?: { hasMusic?: boolean; hasOverlay?: boolean };
  /**
   * Text, stickers, emojis and drawing on the video (burned in by the export). Passing
   * `onOverlaysChange` turns the tools on; they share undo / redo with the edit. Read on mount.
   */
  initialOverlays?: Overlay[];
  onOverlaysChange?: (overlays: Overlay[]) => void;
  /** Music to hear with the preview, in step with the posted timeline (the export mixes it for real). */
  previewMusic?: { file: Blob; settings: MusicSettings } | null;
};

/**
 * Full-screen status video editor: preview with play / pause, a timeline of real frames with
 * trim handles and a scrubbing playhead, duration + estimated size, mute and volume, crop
 * (presets or free) and rotate, undo / redo. Selections longer than a minute become 60 s
 * parts that can be previewed, trimmed, reordered and removed. Produces a `VideoEdit`;
 * `renderVideoEdit` (lib/media-editor/video) turns it into MP4 files.
 */
export default function VideoEditor(props: VideoEditorProps) {
  const { file, probe: given, onError, onClose } = props;
  const [read, setRead] = useState<{ file: Blob; probe?: VideoProbe; error?: unknown } | null>(null);

  useEffect(() => {
    if (given) return;
    let live = true;
    probeVideo(file).then(
      (probe) => {
        if (!live) return;
        if (!probe.canDecode) {
          const error = new Error("This browser cannot play this video's format.");
          setRead({ file, error });
          onError?.(error);
        } else setRead({ file, probe });
      },
      (error) => { if (live) { setRead({ file, error }); onError?.(error); } },
    );
    return () => { live = false; };
  }, [file, given, onError]);

  const probe = given ?? (read?.file === file ? read.probe : undefined);
  const error = !given && read?.file === file ? read.error : undefined;

  if (!probe) {
    return (
      <div className="fixed inset-0 z-[80] flex flex-col items-center justify-center gap-4 bg-black text-white" role="dialog" aria-label="Edit video" data-testid="video-editor">
        <button type="button" aria-label="Close" onClick={onClose} className="absolute left-3 top-[max(env(safe-area-inset-top),0.75rem)] flex size-11 items-center justify-center rounded-full bg-white/10 hover:bg-white/20"><X className="size-6" /></button>
        {error ? <p className="max-w-xs text-center text-sm text-white/80">{error instanceof Error ? error.message : "This video can't be edited here."}</p>
          : <><Loader2 className="size-8 animate-spin text-white/70" /><p className="text-sm text-white/70">Opening video…</p></>}
      </div>
    );
  }
  return <Editor {...props} probe={probe} />;
}

type Size = { width: number; height: number };

/** Size of an element, kept current with a ResizeObserver. */
function useElementSize<T extends HTMLElement>(): [(el: T | null) => void, Size] {
  const [size, setSize] = useState<Size>({ width: 0, height: 0 });
  const observer = useRef<ResizeObserver | null>(null);
  const ref = useCallback((el: T | null) => {
    observer.current?.disconnect();
    observer.current = null;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => {
      const r = entry.contentRect;
      setSize((s) => (s.width === r.width && s.height === r.height ? s : { width: r.width, height: r.height }));
    });
    ro.observe(el);
    observer.current = ro;
  }, []);
  return [ref, size];
}

const FULL: NormalizedCrop = { x: 0, y: 0, width: 1, height: 1 };
const THUMBS = 10;
const STEP_MS = 100;

type Tag = "edit" | "overlay";
type Tool = "none" | "draw" | "sticker";

function Editor({ file, value, onChange, overlaySlot, toolbarExtra, footer, onClose, probe, estimate, initialOverlays, onOverlaysChange, previewMusic }: VideoEditorProps & { probe: VideoProbe }) {
  const musicEl = useRef<HTMLAudioElement>(null);
  const musicRef = useRef(previewMusic);
  useEffect(() => { musicRef.current = previewMusic; });
  const musicFile = previewMusic?.file;
  useEffect(() => {
    const a = musicEl.current;
    if (!a || !musicFile) return;
    const url = URL.createObjectURL(musicFile);
    a.src = url;
    return () => { a.pause(); a.removeAttribute("src"); a.load(); URL.revokeObjectURL(url); };
  }, [musicFile]);
  /** Keeps the preview music at `timelineMs` of the posted timeline (or silent). */
  const syncMusic = (timelineMs: number | null) => {
    const a = musicEl.current;
    const m = musicRef.current?.settings;
    if (!a || !m || !a.src) return;
    const target = timelineMs === null ? null : m.startMs + timelineMs;
    if (target === null || target >= m.endMs) { if (!a.paused) a.pause(); return; }
    if (Math.abs(a.currentTime * 1000 - target) > 300) a.currentTime = target / 1000;
    a.volume = Math.min(1, fadeGain(target, m));
    if (a.paused) void a.play().catch(() => {});
  };
  const historyRef = useRef<EditHistory | null>(null);
  const [history, setHistory] = useState<EditHistory>(() => createHistory(value ?? initialVideoEdit(probe.durationMs)));
  const edit = history.present;
  const video = useRef<HTMLVideoElement>(null);
  const root = useRef<HTMLDivElement>(null);
  const [playing, setPlaying] = useState(false);
  const [timeMs, setTimeMs] = useState(edit.segments[0]?.startMs ?? 0);
  /** Part being played (index in posting order). */
  const [part, setPart] = useState(0);
  /** Loop a single part instead of playing all parts in order. */
  const [loopPart, setLoopPart] = useState(false);
  /** Part whose ends the trim handles move (null = the whole selection). */
  const [selected, setSelected] = useState<string | null>(null);
  const [cropMode, setCropMode] = useState(false);
  const [volumeOpen, setVolumeOpen] = useState(false);
  const [thumbs, setThumbs] = useState<{ file: Blob; list: (ImageBitmap | null)[] }>({ file, list: [] });
  const [stageRef, stage] = useElementSize<HTMLDivElement>();
  const drags = useRef(0);

  // One undo / redo across the edit (trim, crop, sound...) and the overlays: each step is tagged
  // with the history it belongs to, so Undo always reverses the most recent change.
  const overlaysOn = Boolean(onOverlaysChange);
  const ov = useHistory<Overlay[]>(() => initialOverlays ?? []);
  const [tags, setTags] = useState<{ undo: Tag[]; redo: Tag[] }>({ undo: [], redo: [] });
  const [tool, setTool] = useState<Tool>("none");
  const [text, setText] = useState<{ id?: string; draft: TextDraft } | null>(null);
  const [brushColor, setBrushColor] = useState(OVERLAY_COLORS[2]);
  const [brushSize, setBrushSize] = useState(1);
  const [eraser, setEraser] = useState(false);
  const [gesture, setGesture] = useState(false);
  const pushTag = (t: Tag) => setTags((s) => ({ undo: [...s.undo, t].slice(-200), redo: [] }));

  const dispatch = useCallback((a: HistoryAction) => {
    const cur = historyRef.current ?? history;
    const next = historyReducer(cur, a);
    historyRef.current = next;
    if (next === cur) return;
    if (a.type === "apply" && !(a.mergeKey != null && a.mergeKey === cur.mergeKey)) setTags((s) => ({ undo: [...s.undo, "edit" as Tag].slice(-200), redo: [] }));
    setHistory(next);
    if (next.present !== cur.present) onChange(next.present);
  }, [history, onChange]);

  const commitOverlays = (next: Overlay[]) => {
    if (next === ov.present) return;
    pushTag("overlay");
    ov.commit(next);
  };
  useEffect(() => { onOverlaysChange?.(ov.present); }, [ov.present, onOverlaysChange]);
  const apply = useCallback((action: VideoEditAction, mergeKey?: string) => dispatch({ type: "apply", action, mergeKey }), [dispatch]);
  const latest = () => (historyRef.current ?? history).present;

  // Report the starting edit once.
  const reported = useRef(false);
  useEffect(() => {
    if (reported.current) return;
    reported.current = true;
    if (!value) onChange(history.present);
  }, [value, onChange, history.present]);

  // The video element plays the original file.
  useEffect(() => {
    const el = video.current;
    if (!el) return;
    const url = URL.createObjectURL(file);
    el.src = url;
    return () => { el.pause(); el.removeAttribute("src"); el.load(); URL.revokeObjectURL(url); };
  }, [file]);

  useEffect(() => {
    const el = video.current;
    if (!el) return;
    el.muted = edit.muted;
    el.volume = Math.min(1, edit.volume);
  }, [edit.muted, edit.volume]);

  // Timeline thumbnails (real frames), cancelled when the file changes or the editor closes.
  useEffect(() => {
    const ac = new AbortController();
    const made: ImageBitmap[] = [];
    timelineThumbnails(file, THUMBS, 96, {
      signal: ac.signal,
      onThumbnail: (t, i) => {
        made.push(t.bitmap);
        setThumbs((s) => {
          const list = s.file === file ? s.list.slice() : [];
          list[i] = t.bitmap;
          return { file, list };
        });
      },
    }).catch(() => { /* thumbnails are decoration: the timeline still works without them */ });
    return () => { ac.abort(); for (const b of made) b.close(); };
  }, [file]);

  // Playback: follow the parts in posting order (or loop one), updating the playhead.
  useEffect(() => {
    if (!playing) return;
    let raf = 0;
    const tick = () => {
      const el = video.current;
      const segs = latest().segments;
      if (el && segs.length) {
        const t = el.currentTime * 1000;
        const i = Math.min(part, segs.length - 1);
        const seg = segs[i];
        if (t >= seg.endMs - 20 || el.ended) {
          if (loopPart) el.currentTime = seg.startMs / 1000;
          else if (i + 1 < segs.length) { setPart(i + 1); el.currentTime = segs[i + 1].startMs / 1000; }
          else { el.pause(); syncMusic(null); setPlaying(false); setPart(0); el.currentTime = segs[0].startMs / 1000; setTimeMs(segs[0].startMs); return; }
        } else if (t < seg.startMs - 250) {
          el.currentTime = seg.startMs / 1000;
        }
        setTimeMs(el.currentTime * 1000);
        const before = segs.slice(0, i).reduce((sum, x) => sum + (x.endMs - x.startMs), 0);
        syncMusic(loopPart ? null : before + Math.max(0, t - seg.startMs));
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- `latest` reads the live history
  }, [playing, part, loopPart]);

  const seek = (ms: number) => {
    const el = video.current;
    if (!el) return;
    el.currentTime = Math.max(0, ms) / 1000;
    setTimeMs(ms);
  };

  const play = (fromPart?: number, loop = false) => {
    const el = video.current;
    const segs = latest().segments;
    if (!el || !segs.length) return;
    let i = fromPart ?? segs.findIndex((s) => timeMs >= s.startMs && timeMs < s.endMs - 50);
    if (i < 0) i = 0;
    const seg = segs[i];
    if (fromPart !== undefined || timeMs < seg.startMs || timeMs >= seg.endMs - 50) el.currentTime = seg.startMs / 1000;
    setPart(i);
    setLoopPart(loop);
    void el.play().then(() => setPlaying(true), () => setPlaying(false));
  };
  const pause = () => { video.current?.pause(); syncMusic(null); setPlaying(false); };
  const toggle = () => (playing ? pause() : play());

  const undo = () => {
    const t = tags.undo[tags.undo.length - 1];
    if (!t) return;
    setTags((s) => ({ undo: s.undo.slice(0, -1), redo: [...s.redo, t] }));
    if (t === "edit") dispatch({ type: "undo" }); else ov.undo();
  };
  const redo = () => {
    const t = tags.redo[tags.redo.length - 1];
    if (!t) return;
    setTags((s) => ({ undo: [...s.undo, t], redo: s.redo.slice(0, -1) }));
    if (t === "edit") dispatch({ type: "redo" }); else ov.redo();
  };
  const canUndo = tags.undo.length > 0 && (history.past.length > 0 || ov.canUndo);
  const canRedo = tags.redo.length > 0 && (history.future.length > 0 || ov.canRedo);

  const addSticker = (emoji: string) => { commitOverlays([...ov.present, createSticker(emoji)]); setTool("none"); };
  const saveText = (draft: TextDraft) => {
    const id = text?.id;
    setText(null);
    if (!draft.text.trim()) return;
    if (id) commitOverlays(updateOverlay<TextOverlay>(ov.present, id, (t) => ({ ...t, ...draft })));
    else commitOverlays([...ov.present, createText(draft)]);
  };

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const tag = (e.target as HTMLElement).tagName;
    const typing = tag === "INPUT" || tag === "TEXTAREA" || (e.target as HTMLElement).isContentEditable;
    const mod = e.metaKey || e.ctrlKey;
    if (mod && !typing && e.key.toLowerCase() === "z") { e.preventDefault(); if (e.shiftKey) redo(); else undo(); return; }
    if (mod && !typing && e.key.toLowerCase() === "y") { e.preventDefault(); redo(); return; }
    if (e.key === "Escape") {
      e.preventDefault();
      if (text) setText(null);
      else if (tool !== "none") { setTool("none"); setEraser(false); }
      else if (volumeOpen) setVolumeOpen(false);
      else if (cropMode) setCropMode(false);
      else onClose();
      return;
    }
    if (!typing && tag !== "BUTTON" && (e.key === " " || e.key === "k")) { e.preventDefault(); toggle(); }
  };

  useEffect(() => { root.current?.focus(); }, []);

  // ------------------------------------------------------------ geometry of the preview
  const frame = rotatedSize(probe.width, probe.height, edit.rotation);
  const view = cropMode ? FULL : (edit.crop ?? FULL);
  const outW = view.width * frame.width;
  const outH = view.height * frame.height;
  const scale = stage.width && stage.height ? Math.min(stage.width / outW, stage.height / outH) : 0;
  const box = { width: outW * scale, height: outH * scale };
  const fw = frame.width * scale;
  const fh = frame.height * scale;
  const elW = edit.rotation % 180 ? fh : fw;
  const elH = edit.rotation % 180 ? fw : fh;
  const cx = -view.x * fw + fw / 2;
  const cy = -view.y * fh + fh / 2;

  const selectedMs = editDurationMs(edit);
  const hasOverlay = hasVisibleOverlays(ov.present);
  const size = estimateEditSize(probe, edit, { ...estimate, hasOverlay: hasOverlay || estimate?.hasOverlay });
  const shownOverlays = text?.id ? ov.present.filter((x) => x.id !== text.id) : ov.present;
  const chrome = cn("transition-opacity duration-150", gesture && "pointer-events-none opacity-0");
  const editText = (t: TextOverlay) => { pause(); setText({ id: t.id, draft: { text: t.text, font: t.font, color: t.color, background: t.background, align: t.align, size: t.size } }); };
  const multi = edit.segments.length > 1;
  const selectedSeg = multi ? edit.segments.find((s) => s.id === selected) ?? null : null;

  return (
    <div ref={root} tabIndex={-1} onKeyDown={onKeyDown} role="dialog" aria-label="Edit video" data-testid="video-editor"
      className="fixed inset-0 z-[80] flex select-none flex-col bg-black text-white outline-none">
      {/* Top bar */}
      <div className={cn("flex items-start justify-between gap-2 px-3 pt-[max(env(safe-area-inset-top),0.75rem)]", chrome)}>
        {tool === "draw" ? (
          <>
            <RoundButton label="Undo" onClick={undo} disabled={!canUndo}><Undo2 className="size-5" /></RoundButton>
            <div className="flex items-center gap-2">
              <RoundButton label="Eraser" pressed={eraser} onClick={() => setEraser(!eraser)}><Eraser className="size-5" /></RoundButton>
              <button type="button" onClick={() => { setTool("none"); setEraser(false); }} data-testid="video-draw-done"
                className="flex h-11 items-center gap-1.5 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white">
                <Check className="size-4" />Done
              </button>
            </div>
          </>
        ) : (
        <>
        <RoundButton label="Close" onClick={onClose}><X className="size-6" /></RoundButton>
        <div className="flex flex-wrap items-center justify-end gap-2">
          <RoundButton label="Undo" onClick={undo} disabled={!canUndo}><Undo2 className="size-5" /></RoundButton>
          <RoundButton label="Redo" onClick={redo} disabled={!canRedo}><Redo2 className="size-5" /></RoundButton>
          {overlaysOn && !cropMode ? (
            <>
              <RoundButton label="Stickers and emojis" onClick={() => { pause(); setTool("sticker"); setVolumeOpen(false); }}><Smile className="size-5" /></RoundButton>
              <RoundButton label="Add text" onClick={() => { pause(); setText({ draft: DEFAULT_TEXT_DRAFT }); setVolumeOpen(false); }}><Type className="size-5" /></RoundButton>
              <RoundButton label="Draw" onClick={() => { pause(); setTool("draw"); setVolumeOpen(false); }}><Pencil className="size-5" /></RoundButton>
            </>
          ) : null}
          <RoundButton label="Crop" pressed={cropMode} onClick={() => { pause(); setCropMode(!cropMode); setVolumeOpen(false); }}><Crop className="size-5" /></RoundButton>
          <RoundButton label="Rotate" onClick={() => apply({ type: "rotate" })}><RotateCw className="size-5" /></RoundButton>
          <div className="relative">
            <RoundButton label="Volume" pressed={volumeOpen} onClick={() => setVolumeOpen(!volumeOpen)} disabled={!probe.hasAudio}><Volume2 className="size-5" /></RoundButton>
            {volumeOpen ? (
              <div className="absolute right-0 top-12 z-20 w-56 rounded-2xl bg-neutral-900/95 p-3 shadow-xl ring-1 ring-white/10">
                <label className="flex items-center justify-between text-xs text-white/70" htmlFor="video-editor-volume">
                  Original sound<span className="font-mono">{Math.round(edit.volume * 100)}%</span>
                </label>
                <input id="video-editor-volume" type="range" min={0} max={200} step={5} value={Math.round(edit.volume * 100)}
                  onPointerDown={() => { drags.current += 1; }}
                  onChange={(e) => apply({ type: "setVolume", volume: Number(e.target.value) / 100 }, `volume-${drags.current}`)}
                  className="mt-2 w-full accent-primary" />
              </div>
            ) : null}
          </div>
          {toolbarExtra}
        </div>
        </>
        )}
      </div>

      {/* Timeline + info chip */}
      <div className="px-3 pt-3">
        <Timeline
          edit={edit}
          durationMs={probe.durationMs}
          timeMs={timeMs}
          thumbs={thumbs.file === file ? thumbs.list : []}
          selected={selectedSeg}
          onTrim={(startMs, endMs, mergeKey) => {
            if (selectedSeg) apply({ type: "trimSegment", id: selectedSeg.id, startMs, endMs }, mergeKey);
            else apply({ type: "setRange", startMs, endMs }, mergeKey);
          }}
          onSeek={(ms) => { pause(); seek(ms); }}
          newDragKey={() => `trim-${++drags.current}`}
          latest={latest}
        />
        <div className="mt-2 flex items-center justify-center gap-2">
          <span className="rounded-full bg-black/60 px-3 py-1 text-xs font-medium tabular-nums ring-1 ring-white/15" data-testid="video-editor-info">
            {formatDuration(selectedMs)} • {formatBytes(size)}
            {selectedMs < probe.durationMs - 500 ? <span className="text-white/50"> · of {formatDuration(probe.durationMs)}</span> : null}
          </span>
          <button type="button" aria-label={edit.muted ? "Unmute" : "Mute"} aria-pressed={edit.muted} disabled={!probe.hasAudio}
            onClick={() => apply({ type: "setMuted", muted: !edit.muted })}
            className="flex size-8 items-center justify-center rounded-full bg-black/60 ring-1 ring-white/15 hover:bg-white/15 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary disabled:opacity-40">
            {edit.muted || !probe.hasAudio ? <VolumeX className="size-4" /> : <Volume2 className="size-4" />}
          </button>
        </div>
      </div>

      {/* Preview */}
      <div ref={stageRef} className="relative mx-3 my-3 min-h-0 flex-1">
        <div className="absolute inset-0 flex items-center justify-center">
          <div className="relative overflow-hidden" style={{ width: box.width, height: box.height }}
            onClick={() => { if (!cropMode) toggle(); }}>
            <audio ref={musicEl} preload="auto" className="hidden" />
            <video ref={video} playsInline preload="auto" aria-label="Video preview"
              onPause={() => { syncMusic(null); setPlaying(false); }}
              className="pointer-events-none absolute max-w-none"
              style={{
                width: elW,
                height: elH,
                left: cx - elW / 2,
                top: cy - elH / 2,
                transform: `${edit.flipH ? "scaleX(-1) " : ""}rotate(${edit.rotation}deg)`,
              }} />
            {cropMode ? (
              <CropOverlay crop={edit.crop ?? FULL} frame={frame}
                aspect={CROP_ASPECTS.find((a) => a.key === edit.cropAspect)?.ratio ?? (edit.cropAspect === "original" ? probe.width / probe.height : null)}
                onChange={(crop, mergeKey) => apply({ type: "setCrop", crop }, mergeKey)}
                newDragKey={() => `crop-${++drags.current}`} />
            ) : (
              <div className="absolute inset-0" onClick={(e) => e.stopPropagation()}>
                {overlaysOn ? (
                  <OverlayLayer overlays={shownOverlays} onChange={commitOverlays}
                    tool={tool === "draw" ? (eraser ? "erase" : "draw") : text || tool === "sticker" ? "none" : "move"}
                    brush={{ color: brushColor, width: BRUSH_SIZES[brushSize] }} onEditText={editText} onGestureChange={setGesture} />
                ) : null}
                {overlaySlot}
              </div>
            )}
          </div>
        </div>
        {!cropMode && tool === "none" && !text ? (
          <button type="button" aria-label={playing ? "Pause" : "Play"} onClick={toggle}
            className={cn("absolute left-1/2 top-1/2 flex size-16 -translate-x-1/2 -translate-y-1/2 items-center justify-center rounded-full bg-black/45 backdrop-blur transition-opacity focus-visible:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary",
              playing ? "opacity-0 hover:opacity-100" : "opacity-100")}>
            {playing ? <Pause className="size-8" /> : <Play className="ml-1 size-8" />}
          </button>
        ) : null}
      </div>

      {text ? <TextOverlayEditor initial={text.draft} onDone={saveText} onCancel={() => setText(null)} /> : null}
      {tool === "sticker" ? (
        <div className="absolute inset-x-0 bottom-0 z-30"><EmojiPicker onPick={addSticker} onClose={() => setTool("none")} /></div>
      ) : null}

      {tool === "draw" ? (
        <div className="space-y-2 px-3 pb-[max(env(safe-area-inset-bottom),0.75rem)]" data-testid="video-draw-panel">
          <ColorRow label="Brush colour" colors={OVERLAY_COLORS} value={brushColor} onChange={(c) => { setBrushColor(c); setEraser(false); }} />
          <Segmented label="Brush size" value={brushSize} onChange={(v) => { setBrushSize(v); setEraser(false); }}
            options={BRUSH_SIZES.map((_, i) => ({ value: i, aria: `Brush size ${i + 1}`, label: <span className="block rounded-full bg-current" style={{ width: 6 + i * 5, height: 6 + i * 5 }} /> }))} />
        </div>
      ) : cropMode ? (
        <div className="space-y-3 px-3 pb-[max(env(safe-area-inset-bottom),0.75rem)]">
          <div className="flex gap-2 overflow-x-auto pb-1" role="radiogroup" aria-label="Crop shape">
            {CROP_ASPECTS.map((a) => (
              <button key={a.key} type="button" role="radio" aria-checked={edit.cropAspect === a.key}
                onClick={() => apply({ type: "setCrop", aspect: a.key, crop: cropFor(a.key, frame, probe, edit.crop) })}
                className={cn("h-9 shrink-0 rounded-full px-4 text-sm font-semibold ring-1 ring-white/15 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary",
                  edit.cropAspect === a.key ? "bg-primary text-primary-foreground" : "bg-white/10 hover:bg-white/15")}>
                {a.label}
              </button>
            ))}
          </div>
          <div className="flex items-center justify-between gap-2">
            <div className="flex gap-2">
              <RoundButton label="Flip" pressed={edit.flipH} onClick={() => apply({ type: "flip" })}><FlipHorizontal2 className="size-5" /></RoundButton>
              <RoundButton label="Reset crop" onClick={() => apply({ type: "setCrop", crop: null, aspect: "free" })} disabled={!edit.crop}><RotateCcw className="size-5" /></RoundButton>
            </div>
            <button type="button" onClick={() => setCropMode(false)} className="h-11 rounded-full bg-primary px-6 text-sm font-semibold text-primary-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white">Done</button>
          </div>
        </div>
      ) : (
        <>
          {multi ? (
            <Parts edit={edit} selected={selected} playingPart={playing ? part : null}
              onSelect={(id) => setSelected(selected === id ? null : id)}
              onPreview={(i) => play(i, true)}
              onMove={(id, to) => apply({ type: "moveSegment", id, toIndex: to })}
              onRemove={(id) => { if (selected === id) setSelected(null); pause(); setPart(0); apply({ type: "removeSegment", id }); }}
              onReset={() => { setSelected(null); apply({ type: "resetSegments" }); }} />
          ) : null}
          <div className={cn(tool === "sticker" && "invisible", chrome)}>
            {footer ? <div className="pb-[max(env(safe-area-inset-bottom),0px)]">{footer}</div> : <div className="pb-[env(safe-area-inset-bottom)]" />}
          </div>
        </>
      )}
    </div>
  );
}

/** The crop for a preset, centred on the current crop when possible. */
function cropFor(aspect: CropAspect, frame: Size, probe: VideoProbe, current: NormalizedCrop | null): NormalizedCrop | null {
  if (aspect === "free") return current;
  const ratio = aspect === "original" ? probe.width / probe.height : CROP_ASPECTS.find((a) => a.key === aspect)!.ratio!;
  return aspectCrop(frame.width, frame.height, ratio);
}

function RoundButton({ label, onClick, disabled, pressed, children }: { label: string; onClick: () => void; disabled?: boolean; pressed?: boolean; children: ReactNode }) {
  return (
    <button type="button" aria-label={label} title={label} onClick={onClick} disabled={disabled} aria-pressed={pressed}
      className={cn("flex size-11 shrink-0 items-center justify-center rounded-full backdrop-blur transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary disabled:opacity-40",
        pressed ? "bg-primary text-primary-foreground" : "bg-black/50 hover:bg-white/20")}>
      {children}
    </button>
  );
}

// ---------------------------------------------------------------- timeline

function Thumb({ bitmap }: { bitmap: ImageBitmap | null }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const c = ref.current;
    if (!c || !bitmap || !bitmap.width) return;
    c.width = bitmap.width;
    c.height = bitmap.height;
    c.getContext("2d")?.drawImage(bitmap, 0, 0);
  }, [bitmap]);
  return <canvas ref={ref} className="h-full min-w-0 flex-1 object-cover" aria-hidden />;
}

type TimelineProps = {
  edit: VideoEdit;
  durationMs: number;
  timeMs: number;
  thumbs: (ImageBitmap | null)[];
  /** When set, the handles trim this part instead of the whole selection. */
  selected: { id: string; startMs: number; endMs: number } | null;
  onTrim: (startMs: number, endMs: number, mergeKey: string) => void;
  onSeek: (ms: number) => void;
  newDragKey: () => string;
  latest: () => VideoEdit;
};

function Timeline({ edit, durationMs, timeMs, thumbs, selected, onTrim, onSeek, newDragKey, latest }: TimelineProps) {
  const track = useRef<HTMLDivElement>(null);
  const drag = useRef<{ kind: "start" | "end" | "seek"; key: string } | null>(null);
  const target = selected ?? edit.range;
  const pct = (ms: number) => `${(Math.min(durationMs, Math.max(0, ms)) / durationMs) * 100}%`;
  const msAt = (clientX: number) => {
    const r = track.current!.getBoundingClientRect();
    return Math.round((Math.min(r.width, Math.max(0, clientX - r.left)) / r.width) * durationMs);
  };
  const current = () => {
    const e = latest();
    const s = selected ? e.segments.find((x) => x.id === selected.id) : null;
    return s ?? e.range;
  };

  const down = (kind: "start" | "end" | "seek", e: ReactPointerEvent<HTMLElement>) => {
    e.preventDefault();
    e.stopPropagation();
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { kind, key: newDragKey() };
    if (kind === "seek") onSeek(msAt(e.clientX));
  };
  const move = (e: ReactPointerEvent<HTMLElement>) => {
    const d = drag.current;
    if (!d) return;
    const ms = msAt(e.clientX);
    const cur = current();
    if (d.kind === "seek") onSeek(ms);
    else if (d.kind === "start") { onTrim(ms, cur.endMs, d.key); onSeek(Math.min(ms, cur.endMs)); }
    else { onTrim(cur.startMs, ms, d.key); onSeek(Math.max(ms, cur.startMs)); }
  };
  const up = (e: ReactPointerEvent<HTMLElement>) => {
    if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId);
    drag.current = null;
  };
  const key = (which: "start" | "end", e: ReactKeyboardEvent<HTMLElement>) => {
    const step = e.shiftKey ? 1000 : STEP_MS;
    const cur = current();
    let ms: number | null = null;
    const v = which === "start" ? cur.startMs : cur.endMs;
    if (e.key === "ArrowLeft" || e.key === "ArrowDown") ms = v - step;
    else if (e.key === "ArrowRight" || e.key === "ArrowUp") ms = v + step;
    else if (e.key === "Home") ms = 0;
    else if (e.key === "End") ms = durationMs;
    if (ms === null) return;
    e.preventDefault();
    e.stopPropagation();
    const k = `key-${which}-${e.repeat ? "held" : newDragKey()}`;
    if (which === "start") onTrim(ms, cur.endMs, k);
    else onTrim(cur.startMs, ms, k);
  };

  const handle = (which: "start" | "end") => (
    <div role="slider" tabIndex={0} aria-label={which === "start" ? (selected ? "Part start" : "Trim start") : (selected ? "Part end" : "Trim end")}
      aria-valuemin={0} aria-valuemax={Math.round(durationMs / 1000)} aria-valuenow={Math.round((which === "start" ? target.startMs : target.endMs) / 1000)}
      aria-valuetext={formatDuration(which === "start" ? target.startMs : target.endMs)}
      onPointerDown={(e) => down(which, e)} onPointerMove={move} onPointerUp={up} onPointerCancel={up} onKeyDown={(e) => key(which, e)}
      className={cn("absolute inset-y-0 z-10 flex w-5 cursor-ew-resize touch-none items-center justify-center bg-white focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary",
        which === "start" ? "-translate-x-full rounded-l-md" : "rounded-r-md")}
      style={{ left: pct(which === "start" ? target.startMs : target.endMs) }}>
      <span className="h-5 w-1 rounded-full bg-black/60" />
    </div>
  );

  return (
    <div className="px-5" data-testid="video-timeline">
      <div ref={track} className="relative h-14 touch-none rounded-md bg-white/10"
        onPointerDown={(e) => down("seek", e)} onPointerMove={move} onPointerUp={up} onPointerCancel={up}>
        <div className="absolute inset-0 flex overflow-hidden rounded-md">
          {Array.from({ length: THUMBS }, (_, i) => <Thumb key={i} bitmap={thumbs[i] ?? null} />)}
        </div>
        {/* Outside the selection: dimmed */}
        <div className="pointer-events-none absolute inset-y-0 left-0 bg-black/60" style={{ width: pct(edit.range.startMs) }} />
        <div className="pointer-events-none absolute inset-y-0 right-0 bg-black/60" style={{ left: pct(edit.range.endMs) }} />
        {/* Parts */}
        {edit.segments.length > 1 ? edit.segments.map((s, i) => (
          <div key={s.id} className={cn("pointer-events-none absolute bottom-0 h-4 border-x border-black/60 text-[10px] font-bold leading-4",
            selected?.id === s.id ? "bg-primary text-primary-foreground" : "bg-black/55 text-white")}
            style={{ left: pct(s.startMs), width: `calc(${pct(s.endMs)} - ${pct(s.startMs)})` }}>
            <span className="px-1">{i + 1}</span>
          </div>
        )) : null}
        {/* Selection frame */}
        <div className="pointer-events-none absolute inset-y-0 border-y-[3px] border-white" style={{ left: pct(target.startMs), width: `calc(${pct(target.endMs)} - ${pct(target.startMs)})` }} />
        {handle("start")}
        {handle("end")}
        {/* Playhead */}
        <div className="pointer-events-none absolute -inset-y-1 z-20 w-0.5 -translate-x-1/2 rounded-full bg-white shadow-[0_0_4px_rgba(0,0,0,0.8)]" style={{ left: pct(timeMs) }} />
      </div>
    </div>
  );
}

// ---------------------------------------------------------------- parts

function Parts({ edit, selected, playingPart, onSelect, onPreview, onMove, onRemove, onReset }: {
  edit: VideoEdit;
  selected: string | null;
  playingPart: number | null;
  onSelect: (id: string) => void;
  onPreview: (index: number) => void;
  onMove: (id: string, toIndex: number) => void;
  onRemove: (id: string) => void;
  onReset: () => void;
}) {
  return (
    <div className="px-3 pb-2" data-testid="video-parts">
      <div className="mb-1.5 flex items-center justify-between text-xs text-white/70">
        <span>{edit.segments.length} statuses of up to 1 minute · tap one to trim it</span>
        <button type="button" onClick={onReset} className="rounded-full px-2 py-1 font-semibold text-white/80 hover:bg-white/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary">Reset parts</button>
      </div>
      <ol className="flex gap-2 overflow-x-auto pb-1">
        {edit.segments.map((s, i) => (
          <li key={s.id} className={cn("flex shrink-0 items-center gap-1 rounded-2xl p-1 pl-3 ring-1", selected === s.id ? "bg-primary/25 ring-primary" : "bg-white/10 ring-white/10")}>
            <button type="button" onClick={() => onSelect(s.id)} aria-pressed={selected === s.id} aria-label={`Part ${i + 1}, ${formatDuration(s.startMs)} to ${formatDuration(s.endMs)}`}
              className="mr-1 text-left text-xs leading-tight focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary">
              <span className="block font-semibold">Part {i + 1}</span>
              <span className="block tabular-nums text-white/70">{formatDuration(s.startMs)}–{formatDuration(s.endMs)}</span>
            </button>
            <SmallButton label={`Preview part ${i + 1}`} onClick={() => onPreview(i)} active={playingPart === i}><Play className="size-4" /></SmallButton>
            <SmallButton label={`Move part ${i + 1} earlier`} onClick={() => onMove(s.id, i - 1)} disabled={i === 0}><ChevronLeft className="size-4" /></SmallButton>
            <SmallButton label={`Move part ${i + 1} later`} onClick={() => onMove(s.id, i + 1)} disabled={i === edit.segments.length - 1}><ChevronRight className="size-4" /></SmallButton>
            <SmallButton label={`Remove part ${i + 1}`} onClick={() => onRemove(s.id)}><Trash2 className="size-4" /></SmallButton>
          </li>
        ))}
      </ol>
    </div>
  );
}

function SmallButton({ label, onClick, disabled, active, children }: { label: string; onClick: () => void; disabled?: boolean; active?: boolean; children: ReactNode }) {
  return (
    <button type="button" aria-label={label} title={label} onClick={onClick} disabled={disabled}
      className={cn("flex size-9 items-center justify-center rounded-full hover:bg-white/15 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary disabled:opacity-30", active && "bg-primary text-primary-foreground")}>
      {children}
    </button>
  );
}

// ---------------------------------------------------------------- crop

function CropOverlay({ crop, frame, aspect, onChange, newDragKey }: {
  crop: NormalizedCrop;
  frame: Size;
  /** Locked width / height ratio in pixels, or null for free. */
  aspect: number | null;
  onChange: (crop: NormalizedCrop, mergeKey: string) => void;
  newDragKey: () => string;
}) {
  const area = useRef<HTMLDivElement>(null);
  const drag = useRef<{ mode: "move" | "nw" | "ne" | "sw" | "se"; key: string; x: number; y: number; start: NormalizedCrop } | null>(null);

  const down = (mode: "move" | "nw" | "ne" | "sw" | "se", e: ReactPointerEvent<HTMLElement>) => {
    e.preventDefault();
    e.stopPropagation();
    e.currentTarget.setPointerCapture(e.pointerId);
    const r = area.current!.getBoundingClientRect();
    drag.current = { mode, key: newDragKey(), x: (e.clientX - r.left) / r.width, y: (e.clientY - r.top) / r.height, start: crop };
  };
  const move = (e: ReactPointerEvent<HTMLElement>) => {
    const d = drag.current;
    if (!d) return;
    const r = area.current!.getBoundingClientRect();
    const x = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
    const y = Math.min(1, Math.max(0, (e.clientY - r.top) / r.height));
    const s = d.start;
    if (d.mode === "move") {
      onChange(clampCrop({ ...s, x: s.x + x - d.x, y: s.y + y - d.y }), d.key);
      return;
    }
    // The corner opposite the dragged one stays put.
    const ax = d.mode.includes("w") ? s.x + s.width : s.x;
    const ay = d.mode.includes("n") ? s.y + s.height : s.y;
    let w = Math.max(0.1, Math.abs(x - ax));
    let h = Math.max(0.1, Math.abs(y - ay));
    if (aspect) {
      // normalized h for a locked pixel ratio: (w * frameW) / (h * frameH) = aspect
      const hFromW = (w * frame.width) / (aspect * frame.height);
      if (hFromW <= h) h = hFromW; else w = (h * aspect * frame.height) / frame.width;
      const maxW = d.mode.includes("w") ? ax : 1 - ax;
      const maxH = d.mode.includes("n") ? ay : 1 - ay;
      const k = Math.min(1, maxW / w, maxH / h);
      w *= k; h *= k;
    }
    const nx = d.mode.includes("w") ? ax - w : ax;
    const ny = d.mode.includes("n") ? ay - h : ay;
    onChange(clampCrop({ x: nx, y: ny, width: w, height: h }, 0.05), d.key);
  };
  const up = (e: ReactPointerEvent<HTMLElement>) => {
    if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId);
    drag.current = null;
  };
  const nudge = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const step = e.shiftKey ? 0.05 : 0.01;
    const d = { ArrowLeft: [-step, 0], ArrowRight: [step, 0], ArrowUp: [0, -step], ArrowDown: [0, step] }[e.key];
    if (!d) return;
    e.preventDefault();
    onChange(clampCrop({ ...crop, x: crop.x + d[0], y: crop.y + d[1] }), `crop-key-${e.repeat ? "held" : newDragKey()}`);
  };

  const corners = ["nw", "ne", "sw", "se"] as const;
  return (
    <div ref={area} className="absolute inset-0 touch-none" data-testid="video-crop">
      <div role="group" tabIndex={0} aria-label="Crop area: drag to move, arrow keys to nudge" onKeyDown={nudge}
        onPointerDown={(e) => down("move", e)} onPointerMove={move} onPointerUp={up} onPointerCancel={up}
        className="absolute cursor-move border-2 border-white shadow-[0_0_0_9999px_rgba(0,0,0,0.55)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary"
        style={{ left: `${crop.x * 100}%`, top: `${crop.y * 100}%`, width: `${crop.width * 100}%`, height: `${crop.height * 100}%` }}>
        <div className="pointer-events-none absolute inset-0 grid grid-cols-3 grid-rows-3">
          {Array.from({ length: 9 }, (_, i) => <div key={i} className="border border-white/25" />)}
        </div>
        {corners.map((c) => (
          <div key={c} aria-hidden onPointerDown={(e) => down(c, e)} onPointerMove={move} onPointerUp={up} onPointerCancel={up}
            className={cn("absolute size-7 touch-none", c[0] === "n" ? "-top-3.5" : "-bottom-3.5", c[1] === "w" ? "-left-3.5" : "-right-3.5",
              c === "nw" || c === "se" ? "cursor-nwse-resize" : "cursor-nesw-resize")}>
            <span className={cn("absolute size-4 border-white", c[0] === "n" ? "top-3 border-t-4" : "bottom-3 border-b-4", c[1] === "w" ? "left-3 border-l-4" : "right-3 border-r-4")} />
          </div>
        ))}
      </div>
    </div>
  );
}
