"use client";

import { useEffect, useRef, useState } from "react";
import { Camera, Check, ChevronLeft, ChevronRight, FolderOpen, Images, LayoutGrid, Loader2, Maximize2, Mic, Music2, Play, Type, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { formatClock } from "@/lib/media-editor/audio";
import {
  canPickFolderInput, canRememberFolder, chooseFolder, newestMedia, recentKindOf, rememberedFolder, reopenFolder, thumbnailOf, type Thumb,
} from "@/lib/media-editor/recent-media";

export type CreatorAction = "text" | "music" | "collage" | "voice" | "camera";

/**
 * The status creation screen (WhatsApp's "Add status"): tiles for Text, Music, Layout, Voice
 * and Camera, then recent photos and videos (thumbnails, video durations), multi-select with
 * numbered order, and a full-screen preview. "Next" sends the selection to the editors, one
 * after another.
 */
export default function StatusCreator({ onClose, onAction, onPick }: {
  onClose: () => void;
  onAction: (action: CreatorAction, files?: File[]) => void;
  /** The chosen photos / videos, in the order they were selected. */
  onPick: (files: File[]) => void;
}) {
  const [recent, setRecent] = useState<File[]>([]);
  const [selected, setSelected] = useState<File[]>([]);
  const [preview, setPreview] = useState<number | null>(null);
  const [folder, setFolder] = useState<{ name: string; granted: boolean; reopen?: () => Promise<void> } | null>(null);
  const [loadingFolder, setLoadingFolder] = useState(false);
  const gallery = useRef<HTMLInputElement>(null);
  const dirInput = useRef<HTMLInputElement>(null);

  const addRecent = (files: File[]) => setRecent((r) => newestMedia([...files, ...r], 120));

  // A remembered folder (File System Access) fills the grid again when still permitted.
  useEffect(() => {
    let live = true;
    void rememberedFolder().then(async (f) => {
      if (!live || !f) return;
      const reopen = async () => {
        setLoadingFolder(true);
        const files = await reopenFolder(f.handle);
        setLoadingFolder(false);
        if (files) { addRecent(files); setFolder({ name: f.name, granted: true }); }
      };
      setFolder({ name: f.name, granted: f.granted, reopen });
      if (f.granted) await reopen();
    });
    return () => { live = false; };
  }, []);

  const pickFolder = async () => {
    if (canRememberFolder()) {
      setLoadingFolder(true);
      const r = await chooseFolder();
      setLoadingFolder(false);
      if (r) { addRecent(r.files); setFolder({ name: r.name, granted: true }); }
    } else dirInput.current?.click();
  };

  const fromInput = (list: FileList | null, selectAll: boolean) => {
    const files = [...(list ?? [])].filter((f) => recentKindOf(f));
    if (!files.length) return;
    addRecent(files);
    if (selectAll) setSelected((s) => [...s, ...files.filter((f) => !s.includes(f))].slice(0, 30));
  };

  const toggle = (f: File) => setSelected((s) => (s.includes(f) ? s.filter((x) => x !== f) : s.length >= 30 ? s : [...s, f]));
  const photos = selected.filter((f) => recentKindOf(f) === "IMAGE");

  const tiles: { action: CreatorAction; label: string; Icon: typeof Type; hint: string }[] = [
    { action: "text", label: "Text", Icon: Type, hint: "Words on a colour" },
    { action: "music", label: "Music", Icon: Music2, hint: "Your own audio" },
    { action: "collage", label: "Layout", Icon: LayoutGrid, hint: "Photo collage" },
    { action: "voice", label: "Voice", Icon: Mic, hint: "Record a voice note" },
    { action: "camera", label: "Camera", Icon: Camera, hint: "Take a photo or video" },
  ];

  return (
    <div className="fixed inset-0 z-[70] flex flex-col bg-background text-foreground" role="dialog" aria-modal="true" aria-label="Add status" data-testid="status-creator">
      <input ref={gallery} type="file" accept="image/*,video/*" multiple className="hidden" data-testid="status-gallery-input"
        onChange={(e) => { fromInput(e.target.files, true); e.target.value = ""; }} />
      <input ref={dirInput} type="file" multiple className="hidden" {...{ webkitdirectory: "" }}
        onChange={(e) => { fromInput(e.target.files, false); e.target.value = ""; }} />

      <header className="flex items-center gap-2 border-b border-border px-2 pb-2 pt-[max(env(safe-area-inset-top),0.5rem)]">
        <button type="button" aria-label="Close" onClick={onClose} className="flex size-11 items-center justify-center rounded-full hover:bg-muted focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary"><X className="size-6" /></button>
        <h1 className="text-lg font-bold">Add status</h1>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="grid grid-cols-5 gap-2 p-3" role="group" aria-label="Create">
          {tiles.map(({ action, label, Icon, hint }) => (
            <button key={action} type="button" data-testid={`status-create-${action}`} title={hint}
              onClick={() => onAction(action, action === "collage" && photos.length >= 2 ? photos : undefined)}
              className="flex min-h-[5.5rem] flex-col items-center justify-center gap-2 rounded-2xl bg-muted px-1 py-3 text-xs font-semibold transition-colors hover:bg-muted/70 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary sm:text-sm">
              <span className="flex size-11 items-center justify-center rounded-full bg-primary/15 text-primary"><Icon className="size-5" /></span>
              {label}
            </button>
          ))}
        </div>

        <div className="flex items-center justify-between gap-2 px-3 pb-2 pt-1">
          <h2 className="text-sm font-semibold text-muted-foreground">{folder?.granted ? `Recent · ${folder.name}` : "Recent"}</h2>
          <div className="flex items-center gap-1">
            {canRememberFolder() || canPickFolderInput() ? (
              <button type="button" onClick={() => void pickFolder()} className="flex h-9 items-center gap-1.5 rounded-full px-3 text-sm font-semibold text-primary hover:bg-primary/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary" data-testid="status-pick-folder">
                {loadingFolder ? <Loader2 className="size-4 animate-spin" /> : <FolderOpen className="size-4" />}Folder
              </button>
            ) : null}
            <button type="button" onClick={() => gallery.current?.click()} className="flex h-9 items-center gap-1.5 rounded-full bg-primary px-3 text-sm font-semibold text-primary-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary" data-testid="status-pick-gallery">
              <Images className="size-4" />Gallery
            </button>
          </div>
        </div>

        {folder && !folder.granted && folder.reopen ? (
          <button type="button" onClick={() => void folder.reopen?.()} className="mx-3 mb-2 flex w-[calc(100%-1.5rem)] items-center justify-center gap-2 rounded-xl border border-dashed border-border p-3 text-sm font-medium hover:bg-muted">
            <FolderOpen className="size-4" />Show photos from &ldquo;{folder.name}&rdquo;
          </button>
        ) : null}

        {recent.length ? (
          <ul className="grid grid-cols-3 gap-0.5 px-0.5 pb-28 sm:grid-cols-4 md:grid-cols-5" data-testid="status-recent-grid">
            {recent.map((f, i) => (
              <RecentTile key={`${f.name}-${f.size}-${f.lastModified}`} file={f} order={selected.indexOf(f)} onToggle={() => toggle(f)} onPreview={() => setPreview(i)} />
            ))}
          </ul>
        ) : (
          <div className="flex flex-col items-center gap-3 px-6 py-12 text-center text-sm text-muted-foreground">
            <Images className="size-10 text-muted-foreground/60" />
            <p>Choose photos and videos from your gallery{canRememberFolder() ? ", or open a folder to see its newest ones here every time" : ""}.</p>
            <button type="button" onClick={() => gallery.current?.click()} className="h-11 rounded-full bg-primary px-6 font-semibold text-primary-foreground">Open gallery</button>
          </div>
        )}
      </div>

      {selected.length ? (
        <div className="absolute inset-x-0 bottom-0 flex items-center gap-2 border-t border-border bg-background/95 px-3 pt-3 pb-[max(env(safe-area-inset-bottom),0.75rem)] backdrop-blur" data-testid="status-selection-bar">
          <p className="min-w-0 flex-1 text-sm font-semibold">{selected.length} selected</p>
          {photos.length >= 2 ? (
            <button type="button" onClick={() => onAction("collage", photos.slice(0, 6))} className="flex h-11 items-center gap-1.5 rounded-full bg-muted px-4 text-sm font-semibold">
              <LayoutGrid className="size-4" />Layout
            </button>
          ) : null}
          <button type="button" onClick={() => onPick(selected)} data-testid="status-selection-next"
            className="flex h-11 items-center gap-1.5 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary">
            Next<ChevronRight className="size-4" />
          </button>
        </div>
      ) : null}

      {preview !== null && recent[preview] ? (
        <FullPreview files={recent} index={preview} selected={selected} onIndex={setPreview} onToggle={toggle} onClose={() => setPreview(null)} />
      ) : null}
    </div>
  );
}

function useThumb(file: File): Thumb | null | "failed" {
  const [thumb, setThumb] = useState<Thumb | null | "failed">(null);
  useEffect(() => {
    let live = true;
    thumbnailOf(file).then((t) => { if (live) setThumb(t); }, () => { if (live) setThumb("failed"); });
    return () => { live = false; };
  }, [file]);
  return thumb;
}

function RecentTile({ file, order, onToggle, onPreview }: { file: File; order: number; onToggle: () => void; onPreview: () => void }) {
  const thumb = useThumb(file);
  const video = recentKindOf(file) === "VIDEO";
  const label = `${video ? "Video" : "Photo"} ${file.name}${thumb && thumb !== "failed" && thumb.durationMs ? `, ${formatClock(thumb.durationMs)}` : ""}`;
  return (
    <li className="relative aspect-square overflow-hidden bg-muted">
      <button type="button" onClick={onToggle} aria-pressed={order >= 0} aria-label={order >= 0 ? `${label}, selected ${order + 1}` : label} data-testid="status-recent-item"
        className="absolute inset-0 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-primary">
        {thumb && thumb !== "failed" && thumb.url ? (
          // eslint-disable-next-line @next/next/no-img-element
          <img src={thumb.url} alt="" className={cn("size-full object-cover transition-transform", order >= 0 && "scale-90 rounded-lg")} draggable={false} />
        ) : thumb === null ? (
          <span className="block size-full animate-pulse bg-muted-foreground/10" />
        ) : (
          <span className="flex size-full items-center justify-center p-2 text-center text-[10px] text-muted-foreground">{file.name}</span>
        )}
      </button>
      <span aria-hidden className={cn("pointer-events-none absolute right-1.5 top-1.5 flex size-6 items-center justify-center rounded-full border-2 border-white text-xs font-bold shadow",
        order >= 0 ? "bg-primary text-primary-foreground" : "bg-black/25")}>{order >= 0 ? order + 1 : null}</span>
      {video ? (
        <span className="pointer-events-none absolute bottom-1 left-1 flex items-center gap-0.5 rounded bg-black/60 px-1 text-[11px] font-semibold tabular-nums text-white">
          <Play className="size-3 fill-current" />{thumb && thumb !== "failed" && thumb.durationMs ? formatClock(thumb.durationMs) : ""}
        </span>
      ) : null}
      <button type="button" aria-label={`Preview ${file.name}`} onClick={onPreview}
        className="absolute bottom-1 right-1 flex size-8 items-center justify-center rounded-full bg-black/45 text-white focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary">
        <Maximize2 className="size-4" />
      </button>
    </li>
  );
}

/** Full-screen preview of the recent media (swipe with the arrows), with select / unselect. */
function FullPreview({ files, index, selected, onIndex, onToggle, onClose }: {
  files: File[]; index: number; selected: File[]; onIndex: (i: number) => void; onToggle: (f: File) => void; onClose: () => void;
}) {
  const file = files[index];
  const touch = useRef<number | null>(null);
  const media = useRef<HTMLImageElement & HTMLVideoElement>(null);
  useEffect(() => {
    const el = media.current;
    if (!el) return;
    const u = URL.createObjectURL(file);
    el.src = u;
    return () => { el.removeAttribute("src"); URL.revokeObjectURL(u); };
  }, [file]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowLeft" && index > 0) onIndex(index - 1);
      else if (e.key === "ArrowRight" && index < files.length - 1) onIndex(index + 1);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [index, files.length, onIndex, onClose]);
  const order = selected.indexOf(file);
  const video = recentKindOf(file) === "VIDEO";
  return (
    <div className="fixed inset-0 z-[80] flex flex-col bg-black text-white" role="dialog" aria-modal="true" aria-label="Preview" data-testid="status-full-preview"
      onTouchStart={(e) => { touch.current = e.touches[0].clientX; }}
      onTouchEnd={(e) => {
        const x0 = touch.current;
        touch.current = null;
        if (x0 === null) return;
        const dx = e.changedTouches[0].clientX - x0;
        if (dx > 60 && index > 0) onIndex(index - 1);
        else if (dx < -60 && index < files.length - 1) onIndex(index + 1);
      }}>
      <div className="flex items-center justify-between px-3 pb-2 pt-[max(env(safe-area-inset-top),0.75rem)]">
        <button type="button" aria-label="Close preview" onClick={onClose} className="flex size-11 items-center justify-center rounded-full bg-white/10 hover:bg-white/20"><X className="size-6" /></button>
        <span className="text-sm tabular-nums text-white/70">{index + 1} / {files.length}</span>
        <button type="button" onClick={() => onToggle(file)} aria-pressed={order >= 0} data-testid="status-preview-select"
          className={cn("flex h-11 items-center gap-1.5 rounded-full px-4 text-sm font-semibold", order >= 0 ? "bg-primary text-primary-foreground" : "bg-white/10 hover:bg-white/20")}>
          {order >= 0 ? <><Check className="size-4" />Selected {order + 1}</> : "Select"}
        </button>
      </div>
      <div className="relative flex min-h-0 flex-1 items-center justify-center">
        {video ? (
          <video key={`v-${index}`} ref={media} controls playsInline autoPlay className="max-h-full max-w-full" />
        ) : (
          // eslint-disable-next-line @next/next/no-img-element
          <img key={`i-${index}`} ref={media} alt={file.name} className="max-h-full max-w-full object-contain" />
        )}
        {index > 0 ? (
          <button type="button" aria-label="Previous" onClick={() => onIndex(index - 1)} className="absolute left-2 top-1/2 flex size-11 -translate-y-1/2 items-center justify-center rounded-full bg-black/50 hover:bg-black/70"><ChevronLeft className="size-6" /></button>
        ) : null}
        {index < files.length - 1 ? (
          <button type="button" aria-label="Next" onClick={() => onIndex(index + 1)} className="absolute right-2 top-1/2 flex size-11 -translate-y-1/2 items-center justify-center rounded-full bg-black/50 hover:bg-black/70"><ChevronRight className="size-6" /></button>
        ) : null}
      </div>
      <p className="truncate px-4 pb-[max(env(safe-area-inset-bottom),0.75rem)] pt-2 text-center text-xs text-white/60">{file.name}</p>
    </div>
  );
}
