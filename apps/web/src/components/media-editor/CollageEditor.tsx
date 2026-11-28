"use client";

import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent, type ReactNode, type WheelEvent as ReactWheelEvent } from "react";
import { ArrowLeftRight, Check, Eraser, Frame, ImagePlus, LayoutGrid, Loader2, Pencil, Redo2, Replace, RotateCcw, Send, Smile, Trash2, Type, Undo2, X } from "lucide-react";
import { cn } from "@/lib/utils";
import {
  cellAt, cellRects, COLLAGE_COLORS, COLLAGE_HEIGHT, COLLAGE_WIDTH, DEFAULT_VIEW, drawCollage, layoutForCount, layoutsFor, MAX_PHOTOS, MIN_PHOTOS,
  panView, swapSlots, zoomView, type CellView, type CollageLayout, type CollagePhoto, type CollageState,
} from "@/lib/media-editor/collage";
import { containSize, fitLongSide } from "@/lib/media-editor/crop";
import { loadEditorFonts } from "@/lib/media-editor/fonts";
import { BRUSH_SIZES, createSticker, createText, OVERLAY_COLORS, updateOverlay, type TextOverlay } from "@/lib/media-editor/overlays";
import { canvasEncoder, encodeSmallest, makeCanvas, type ExportedImage } from "@/lib/media-editor/photo";
import EmojiPicker from "./EmojiPicker";
import OverlayLayer from "./OverlayLayer";
import TextOverlayEditor, { DEFAULT_TEXT_DRAFT, type TextDraft } from "./TextOverlayEditor";
import { ColorRow, EditorSlider, pixelRatio, RoundButton, useElementSize, useHistory } from "./ui";

export interface CollageEditorResult extends ExportedImage {
  state: CollageState;
}

export interface CollageEditorProps {
  /** Photos to start with (2-6 are used; fewer opens the picker). */
  files?: (File | Blob)[];
  onDone: (result: CollageEditorResult) => void | Promise<void>;
  onCancel: () => void;
  /** Output size (default 1080 x 1920 portrait). */
  width?: number;
  height?: number;
  toolbarExtra?: ReactNode;
  footer?: ReactNode;
  doneLabel?: string;
  doneIcon?: ReactNode;
}

type Mode = "photos" | "layout" | "style" | "draw" | "sticker";
type Loaded = CollagePhoto & { close(): void };

const INITIAL: CollageState = { layoutId: "", slots: [], gap: 12, background: "#ffffff", border: 0, borderColor: "#ffffff", radius: 0, overlays: [] };
/** Photos are decoded to at most this size (enough for a 1080 x 1920 collage cell with zoom). */
const PHOTO_SIDE = 2048;

let seq = 0;
const photoId = () => `photo-${++seq}-${Date.now().toString(36)}`;

async function decode(file: Blob): Promise<Loaded> {
  const probe = await createImageBitmap(file, { imageOrientation: "from-image" });
  const fit = fitLongSide(probe.width, probe.height, PHOTO_SIDE);
  if (fit.width === probe.width) return { image: probe, width: probe.width, height: probe.height, close: () => probe.close() };
  probe.close();
  const bmp = await createImageBitmap(file, { imageOrientation: "from-image", resizeWidth: fit.width, resizeHeight: fit.height, resizeQuality: "high" });
  return { image: bmp, width: bmp.width, height: bmp.height, close: () => bmp.close() };
}

type CellGesture =
  | { kind: "pan"; cell: number; start: CellView; x: number; y: number; moved: boolean }
  | { kind: "pinch"; cell: number; start: CellView; d0: number };

/**
 * Collage maker: 2-6 photos in a choice of layouts, pan / pinch / scroll to place each photo in
 * its cell, swap / replace / remove, spacing, outline, corner radius and colours, plus text,
 * stickers and drawing. Renders one image (1080 x 1920 by default).
 */
export default function CollageEditor({ files, onDone, onCancel, width = COLLAGE_WIDTH, height = COLLAGE_HEIGHT, toolbarExtra, footer, doneLabel = "Done", doneIcon }: CollageEditorProps) {
  const [photos, setPhotos] = useState<ReadonlyMap<string, Loaded>>(new Map());
  const history = useHistory<CollageState>(INITIAL);
  const { present, commit, undo, redo, reset } = history;
  const [draft, setDraft] = useState<CollageState | null>(null);
  const [mode, setMode] = useState<Mode>("photos");
  const [selected, setSelected] = useState<number | null>(null);
  const [swapFrom, setSwapFrom] = useState<number | null>(null);
  const [text, setText] = useState<{ id?: string; draft: TextDraft } | null>(null);
  const [brushColor, setBrushColor] = useState(OVERLAY_COLORS[0]);
  const [brushSize, setBrushSize] = useState(1);
  const [eraser, setEraser] = useState(false);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [gestureOn, setGestureOn] = useState(false);
  const [setStage, stage] = useElementSize<HTMLDivElement>();
  const canvas = useRef<HTMLCanvasElement>(null);
  const picker = useRef<HTMLInputElement>(null);
  const replaceAt = useRef<number | null>(null);
  const cellGesture = useRef<CellGesture | null>(null);
  const cellPointers = useRef(new Map<number, { x: number; y: number }>());
  const wheelTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const draftRef = useRef<CollageState | null>(null);
  const owned = useRef(new Set<Loaded>());

  const state = draft ?? present;
  const layout: CollageLayout | undefined = state.slots.length >= MIN_PHOTOS ? layoutForCount(state.slots.length, state.layoutId) : undefined;

  // Release every decoded bitmap when the editor closes.
  useEffect(() => {
    const set = owned.current;
    return () => { set.forEach((p) => p.close()); set.clear(); };
  }, []);

  const addFiles = async (list: (File | Blob)[], replace: number | null) => {
    setLoading(true);
    setError(null);
    try {
      const room = replace !== null ? 1 : MAX_PHOTOS - present.slots.length;
      const decoded: { id: string; photo: Loaded }[] = [];
      for (const f of list.slice(0, Math.max(0, room))) {
        try {
          const photo = await decode(f);
          owned.current.add(photo);
          decoded.push({ id: photoId(), photo });
        } catch {
          setError("Some photos couldn't be opened.");
        }
      }
      if (!decoded.length) return;
      setPhotos((m) => { const n = new Map(m); decoded.forEach((d) => n.set(d.id, d.photo)); return n; });
      const slots = [...present.slots];
      if (replace !== null && slots[replace]) slots[replace] = { photoId: decoded[0].id, view: DEFAULT_VIEW };
      else decoded.forEach((d) => slots.push({ photoId: d.id, view: DEFAULT_VIEW }));
      commit({ ...present, slots, layoutId: layoutForCount(Math.max(MIN_PHOTOS, slots.length), present.layoutId).id });
    } finally {
      setLoading(false);
    }
  };

  // Initial files (decoded once).
  const [initialFiles] = useState(files);
  useEffect(() => {
    if (!initialFiles?.length) return;
    let live = true;
    void Promise.all(initialFiles.slice(0, MAX_PHOTOS).map((f) => decode(f).then((p) => ({ id: photoId(), photo: p })).catch(() => null))).then((all) => {
      const ok = all.filter((x): x is { id: string; photo: Loaded } => !!x);
      if (!live) { ok.forEach((x) => x.photo.close()); return; }
      ok.forEach((x) => owned.current.add(x.photo));
      if (ok.length < initialFiles.length) setError("Some photos couldn't be opened.");
      setPhotos(new Map(ok.map((x) => [x.id, x.photo])));
      const slots = ok.map((x) => ({ photoId: x.id, view: DEFAULT_VIEW }));
      reset({ ...INITIAL, slots, layoutId: layoutForCount(Math.max(MIN_PHOTOS, slots.length)).id });
    });
    return () => { live = false; };
  }, [initialFiles, reset]);

  const disp = containSize(width / height, Math.max(0, stage.width - 24), Math.max(0, stage.height - 24));
  const dispLeft = (stage.width - disp.width) / 2;
  const dispTop = (stage.height - disp.height) / 2;
  const rects = layout ? cellRects(layout, disp.width, disp.height, state.gap * (disp.width / COLLAGE_WIDTH)) : [];

  // Preview: the same renderer as the export (overlays are painted by the OverlayLayer).
  useEffect(() => {
    const c = canvas.current;
    if (!c || !disp.width) return;
    const r = pixelRatio(2);
    c.width = Math.round(disp.width * r);
    c.height = Math.round(disp.height * r);
    const ctx = c.getContext("2d");
    if (!ctx) return;
    drawCollage(ctx, { ...state, overlays: [] }, photos, c.width, c.height, false);
  }, [state, photos, disp.width, disp.height]);

  const setLive = (s: CollageState | null) => { draftRef.current = s; setDraft(s); };
  const viewOf = (i: number) => (draftRef.current ?? present).slots[i]?.view ?? DEFAULT_VIEW;
  const withView = (i: number, view: CellView): CollageState => {
    const base = draftRef.current ?? present;
    return { ...base, slots: base.slots.map((s, j) => (j === i ? { ...s, view } : s)) };
  };
  const photoAt = (i: number) => photos.get(state.slots[i]?.photoId ?? "");
  const local = (e: { clientX: number; clientY: number }, el: Element) => {
    const r = el.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };

  const onCellDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (mode === "draw" || !layout) return;
    const p = local(e, e.currentTarget);
    const g = cellGesture.current;
    if (g) {
      cellPointers.current.set(e.pointerId, p);
      e.currentTarget.setPointerCapture(e.pointerId);
      if (cellPointers.current.size === 2) {
        const [a, b] = [...cellPointers.current.values()];
        cellGesture.current = { kind: "pinch", cell: g.cell, start: viewOf(g.cell), d0: Math.hypot(b.x - a.x, b.y - a.y) || 1 };
      }
      return;
    }
    const i = cellAt(rects, p.x, p.y);
    if (i < 0) return;
    cellPointers.current.set(e.pointerId, p);
    e.currentTarget.setPointerCapture(e.pointerId);
    cellGesture.current = { kind: "pan", cell: i, start: viewOf(i), x: p.x, y: p.y, moved: false };
  };
  const onCellMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!cellPointers.current.has(e.pointerId)) return;
    const p = local(e, e.currentTarget);
    cellPointers.current.set(e.pointerId, p);
    const g = cellGesture.current;
    const photo = g ? photoAt(g.cell) : undefined;
    if (!g || !photo) return;
    const c = rects[g.cell];
    if (g.kind === "pan") {
      if (!g.moved && Math.hypot(p.x - g.x, p.y - g.y) < 5) return;
      g.moved = true;
      setLive(withView(g.cell, panView(photo.width, photo.height, c.w, c.h, g.start, p.x - g.x, p.y - g.y)));
    } else {
      const [a, b] = [...cellPointers.current.values()];
      if (!a || !b) return;
      setLive(withView(g.cell, zoomView(photo.width, photo.height, c.w, c.h, g.start, Math.hypot(b.x - a.x, b.y - a.y) / g.d0)));
    }
  };
  const onCellUp = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!cellPointers.current.delete(e.pointerId)) return;
    const g = cellGesture.current;
    if (!g || (cellPointers.current.size > 0 && g.kind === "pan")) return;
    cellPointers.current.clear();
    cellGesture.current = null;
    if (g.kind === "pan" && !g.moved) {
      // Tap: finish a swap, or select the cell.
      if (swapFrom !== null && swapFrom !== g.cell) { commit({ ...present, slots: swapSlots(present.slots, swapFrom, g.cell) }); setSwapFrom(null); setSelected(g.cell); }
      else setSelected(selected === g.cell ? null : g.cell);
      setLive(null);
      return;
    }
    const d = draftRef.current;
    setLive(null);
    if (d) commit(d);
  };
  const onWheel = (e: ReactWheelEvent<HTMLDivElement>) => {
    if (!layout) return;
    const p = local(e, e.currentTarget);
    const i = cellAt(rects, p.x, p.y);
    const photo = photoAt(i);
    if (i < 0 || !photo) return;
    setLive(withView(i, zoomView(photo.width, photo.height, rects[i].w, rects[i].h, viewOf(i), Math.exp(-e.deltaY * 0.0015))));
    if (wheelTimer.current) clearTimeout(wheelTimer.current);
    wheelTimer.current = setTimeout(() => { wheelTimer.current = null; const d = draftRef.current; setLive(null); if (d) commit(d); }, 250);
  };

  const removeCell = (i: number) => {
    const slots = present.slots.filter((_, j) => j !== i);
    commit({ ...present, slots, layoutId: layoutForCount(Math.max(MIN_PHOTOS, slots.length), present.layoutId).id });
    setSelected(null);
  };
  const resetCell = (i: number) => commit({ ...present, slots: present.slots.map((s, j) => (j === i ? { ...s, view: DEFAULT_VIEW } : s)) });

  const saveText = (d: TextDraft) => {
    const id = text?.id;
    setText(null);
    if (id) commit({ ...present, overlays: updateOverlay<TextOverlay>(present.overlays, id, (t) => ({ ...t, ...d })) });
    else commit({ ...present, overlays: [...present.overlays, createText(d)] });
  };

  const finish = async () => {
    if (busy || present.slots.length < MIN_PHOTOS) return;
    setBusy(true);
    setError(null);
    try {
      await loadEditorFonts();
      const { canvas: out, ctx } = makeCanvas(width, height);
      drawCollage(ctx, present, photos, width, height, true);
      const blob = await encodeSmallest(canvasEncoder(out));
      await onDone({ blob, width, height, state: present });
    } catch {
      setError("The collage couldn't be saved. Try again.");
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        if (swapFrom !== null) setSwapFrom(null);
        else if (mode !== "photos") { setMode("photos"); setEraser(false); }
        else onCancel();
        return;
      }
      const typing = e.target instanceof HTMLElement && /^(INPUT|TEXTAREA)$/.test(e.target.tagName) && (e.target as HTMLInputElement).type !== "range";
      if (typing || !(e.ctrlKey || e.metaKey)) return;
      const key = e.key.toLowerCase();
      if (key === "z" && !e.shiftKey) { e.preventDefault(); undo(); }
      else if ((key === "z" && e.shiftKey) || key === "y") { e.preventDefault(); redo(); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mode, swapFrom, onCancel, undo, redo]);

  const count = present.slots.length;
  const chrome = cn("transition-opacity duration-150", gestureOn && "pointer-events-none opacity-0");
  const shownOverlays = text?.id ? state.overlays.filter((o) => o.id !== text.id) : state.overlays;
  const sel = selected !== null && selected < count ? selected : null;

  return (
    <div className="fixed inset-0 z-[80] flex flex-col overflow-hidden bg-neutral-950 text-white" role="dialog" aria-modal="true" aria-label="Make a collage" data-testid="collage-editor">
      <input ref={picker} type="file" accept="image/*" multiple className="hidden" aria-hidden tabIndex={-1}
        onChange={(e) => { const list = [...(e.target.files ?? [])]; e.target.value = ""; const at = replaceAt.current; replaceAt.current = null; if (list.length) void addFiles(list, at); }} />

      <div className={cn("z-20 flex items-center justify-between gap-2 px-3 pb-2 pt-[max(env(safe-area-inset-top),0.75rem)]", chrome)}>
        {mode === "draw" ? (
          <>
            <RoundButton label="Undo" disabled={!history.canUndo} onClick={history.undo}><Undo2 className="size-5" /></RoundButton>
            <div className="flex items-center gap-2">
              <RoundButton label="Eraser" active={eraser} onClick={() => setEraser(!eraser)}><Eraser className="size-5" /></RoundButton>
              <DoneChip onClick={() => { setMode("photos"); setEraser(false); }} />
            </div>
          </>
        ) : (
          <>
            <RoundButton label="Cancel" onClick={onCancel}><X className="size-6" /></RoundButton>
            <div className="flex items-center gap-1.5 sm:gap-2">
              <RoundButton label="Undo" disabled={!history.canUndo} onClick={history.undo}><Undo2 className="size-5" /></RoundButton>
              <RoundButton label="Redo" disabled={!history.canRedo} onClick={history.redo}><Redo2 className="size-5" /></RoundButton>
              <RoundButton label="Layouts" active={mode === "layout"} disabled={count < MIN_PHOTOS} onClick={() => setMode(mode === "layout" ? "photos" : "layout")}><LayoutGrid className="size-5" /></RoundButton>
              <RoundButton label="Borders and colours" active={mode === "style"} disabled={count < MIN_PHOTOS} onClick={() => setMode(mode === "style" ? "photos" : "style")}><Frame className="size-5" /></RoundButton>
              <RoundButton label="Stickers" disabled={count < MIN_PHOTOS} onClick={() => setMode("sticker")}><Smile className="size-5" /></RoundButton>
              <RoundButton label="Add text" disabled={count < MIN_PHOTOS} onClick={() => setText({ draft: DEFAULT_TEXT_DRAFT })}><Type className="size-5" /></RoundButton>
              <RoundButton label="Draw" disabled={count < MIN_PHOTOS} onClick={() => setMode("draw")}><Pencil className="size-5" /></RoundButton>
              {toolbarExtra}
            </div>
          </>
        )}
      </div>

      <div ref={setStage} className="relative min-h-0 flex-1">
        {count < MIN_PHOTOS ? (
          <div className="absolute inset-0 flex flex-col items-center justify-center gap-4 px-8 text-center">
            {loading ? <Loader2 className="size-10 animate-spin text-white/70" aria-label="Opening photos" /> : (
              <>
                <p className="text-white/80">Choose {MIN_PHOTOS} to {MAX_PHOTOS} photos for your collage.{count ? ` ${count} chosen.` : ""}</p>
                <button type="button" onClick={() => picker.current?.click()} data-testid="collage-pick"
                  className="flex h-12 items-center gap-2 rounded-full bg-primary px-6 font-semibold text-primary-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white">
                  <ImagePlus className="size-5" />Choose photos
                </button>
              </>
            )}
          </div>
        ) : (
          <div className="absolute touch-none" style={{ left: dispLeft, top: dispTop, width: disp.width, height: disp.height }}
            onPointerDown={onCellDown} onPointerMove={onCellMove} onPointerUp={onCellUp} onPointerCancel={onCellUp} onWheel={onWheel} data-testid="collage-stage">
            <canvas ref={canvas} className="absolute inset-0 size-full shadow-2xl" role="img" aria-label="Collage preview" data-testid="collage-canvas" />
            {rects.map((c, i) => (
              <button key={i} type="button" aria-label={`Photo ${i + 1}${sel === i ? ", selected" : ""}`} aria-pressed={sel === i}
                onClick={(e) => { if (e.detail === 0) setSelected(sel === i ? null : i); }}
                onKeyDown={(e) => {
                  const photo = photoAt(i);
                  if (!photo) return;
                  const step = e.shiftKey ? 40 : 10;
                  const moves: Record<string, [number, number]> = { ArrowLeft: [step, 0], ArrowRight: [-step, 0], ArrowUp: [0, step], ArrowDown: [0, -step] };
                  let v: CellView | null = null;
                  if (moves[e.key]) v = panView(photo.width, photo.height, c.w, c.h, viewOf(i), ...moves[e.key]);
                  else if (e.key === "+" || e.key === "=") v = zoomView(photo.width, photo.height, c.w, c.h, viewOf(i), 1.15);
                  else if (e.key === "-") v = zoomView(photo.width, photo.height, c.w, c.h, viewOf(i), 1 / 1.15);
                  if (v) { e.preventDefault(); commit(withView(i, v)); }
                }}
                className={cn("absolute rounded-[2px] outline-none focus-visible:ring-2 focus-visible:ring-primary", sel === i && "ring-2 ring-primary", swapFrom !== null && swapFrom !== i && "animate-pulse ring-2 ring-white/70")}
                style={{ left: c.x, top: c.y, width: c.w, height: c.h, pointerEvents: "none" }} data-testid={`collage-cell-${i}`} />
            ))}
            <OverlayLayer overlays={shownOverlays} onChange={(overlays) => commit({ ...present, overlays })}
              tool={mode === "draw" ? (eraser ? "erase" : "draw") : text || mode === "sticker" ? "none" : "move"}
              brush={{ color: brushColor, width: BRUSH_SIZES[brushSize] }}
              onEditText={(t) => setText({ id: t.id, draft: { text: t.text, font: t.font, color: t.color, background: t.background, align: t.align, size: t.size } })}
              onGestureChange={setGestureOn} />
          </div>
        )}
        {error ? <p role="alert" className="absolute inset-x-4 top-3 z-30 rounded-xl bg-red-600/90 px-4 py-3 text-center text-sm">{error}</p> : null}
        {text ? <TextOverlayEditor initial={text.draft} onDone={saveText} onCancel={() => setText(null)} /> : null}
        {mode === "sticker" ? (
          <div className="absolute inset-x-0 bottom-0 z-30"><EmojiPicker onPick={(e) => { commit({ ...present, overlays: [...present.overlays, createSticker(e)] }); setMode("photos"); }} onClose={() => setMode("photos")} /></div>
        ) : null}
      </div>

      <div className={cn("z-20 space-y-3 px-3 pt-3 pb-[max(env(safe-area-inset-bottom),0.75rem)]", chrome, mode === "sticker" && "hidden")}>
        {mode === "layout" ? (
          <div role="radiogroup" aria-label="Layouts" className="flex gap-3 overflow-x-auto pb-1 [scrollbar-width:none]" data-testid="collage-layouts">
            {layoutsFor(count).map((l) => (
              <button key={l.id} type="button" role="radio" aria-checked={layout?.id === l.id} aria-label={l.label} onClick={() => commit({ ...present, layoutId: l.id })}
                className={cn("relative h-20 w-[45px] shrink-0 rounded-md bg-white/10 p-[3px] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary", layout?.id === l.id && "ring-2 ring-primary")}>
                <span className="relative block size-full">
                  {l.cells.map((c, i) => <span key={i} className="absolute rounded-[2px] bg-white/70" style={{ left: `calc(${c.x * 100}% + 1px)`, top: `calc(${c.y * 100}% + 1px)`, width: `calc(${c.w * 100}% - 2px)`, height: `calc(${c.h * 100}% - 2px)` }} />)}
                </span>
              </button>
            ))}
          </div>
        ) : mode === "style" ? (
          <div className="space-y-2" data-testid="collage-style">
            <EditorSlider label="Spacing" min={0} max={60} value={state.gap} onInput={(gap) => setLive({ ...present, gap })} onCommit={(gap) => { setLive(null); commit({ ...present, gap }); }} />
            <EditorSlider label="Outline" min={0} max={30} value={state.border} onInput={(border) => setLive({ ...present, border })} onCommit={(border) => { setLive(null); commit({ ...present, border }); }} />
            <EditorSlider label="Corners" min={0} max={80} value={state.radius} onInput={(radius) => setLive({ ...present, radius })} onCommit={(radius) => { setLive(null); commit({ ...present, radius }); }} />
            <div className="flex items-center gap-3 text-sm"><span className="w-24 shrink-0 text-white/80">Background</span><ColorRow label="Background colour" colors={COLLAGE_COLORS} value={state.background} onChange={(background) => commit({ ...present, background })} /></div>
            <div className="flex items-center gap-3 text-sm"><span className="w-24 shrink-0 text-white/80">Outline</span><ColorRow label="Outline colour" colors={COLLAGE_COLORS} value={state.borderColor} onChange={(borderColor) => commit({ ...present, borderColor, border: present.border || 6 })} /></div>
          </div>
        ) : mode === "draw" ? (
          <div className="space-y-2">
            <ColorRow label="Brush colour" colors={OVERLAY_COLORS} value={brushColor} onChange={(c) => { setBrushColor(c); setEraser(false); }} />
            <div role="radiogroup" aria-label="Brush size" className="flex gap-2">
              {BRUSH_SIZES.map((_, i) => (
                <button key={i} type="button" role="radio" aria-checked={brushSize === i} aria-label={`Brush size ${i + 1}`} onClick={() => { setBrushSize(i); setEraser(false); }}
                  className={cn("flex size-10 items-center justify-center rounded-full focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary", brushSize === i ? "bg-white text-black" : "bg-white/10")}>
                  <span className="rounded-full bg-current" style={{ width: 6 + i * 5, height: 6 + i * 5 }} />
                </button>
              ))}
            </div>
          </div>
        ) : (
          <>
            {sel !== null ? (
              <div className="flex flex-wrap items-center justify-center gap-2" role="toolbar" aria-label={`Photo ${sel + 1}`} data-testid="collage-cell-actions">
                <Chip onClick={() => { replaceAt.current = sel; picker.current?.click(); }}><Replace className="size-4" />Replace</Chip>
                <Chip active={swapFrom === sel} onClick={() => setSwapFrom(swapFrom === sel ? null : sel)}><ArrowLeftRight className="size-4" />{swapFrom === sel ? "Tap a photo…" : "Swap"}</Chip>
                <Chip onClick={() => resetCell(sel)}><RotateCcw className="size-4" />Fit</Chip>
                <Chip disabled={count <= MIN_PHOTOS} onClick={() => removeCell(sel)}><Trash2 className="size-4" />Remove</Chip>
              </div>
            ) : count >= MIN_PHOTOS ? (
              <p className="text-center text-xs text-white/60">Drag a photo to move it, pinch or scroll to zoom, tap to select it.</p>
            ) : null}
            {footer}
            <div className="flex items-center justify-between gap-2">
              <Chip disabled={count >= MAX_PHOTOS || loading} onClick={() => { replaceAt.current = null; picker.current?.click(); }}><ImagePlus className="size-4" />Add photo</Chip>
              <button type="button" aria-label={doneLabel} disabled={count < MIN_PHOTOS || busy} onClick={() => void finish()} data-testid="collage-done"
                className="flex size-14 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-lg focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white focus-visible:ring-offset-2 focus-visible:ring-offset-black disabled:opacity-50">
                {busy ? <Loader2 className="size-6 animate-spin" /> : doneIcon ?? <Send className="size-6" />}
              </button>
            </div>
          </>
        )}
        {mode === "layout" || mode === "style" ? <div className="flex justify-end"><DoneChip onClick={() => { setLive(null); setMode("photos"); }} /></div> : null}
      </div>
    </div>
  );
}

function Chip({ children, onClick, disabled, active }: { children: ReactNode; onClick: () => void; disabled?: boolean; active?: boolean }) {
  return (
    <button type="button" disabled={disabled} onClick={onClick}
      className={cn("flex h-10 items-center gap-1.5 rounded-full bg-white/10 px-3.5 text-sm font-medium hover:bg-white/20 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary disabled:opacity-40", active && "bg-white text-black hover:bg-white")}>
      {children}
    </button>
  );
}

function DoneChip({ onClick }: { onClick: () => void }) {
  return (
    <button type="button" onClick={onClick} data-testid="panel-done"
      className="flex h-11 items-center gap-1.5 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground shadow focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white">
      <Check className="size-4" />Done
    </button>
  );
}
