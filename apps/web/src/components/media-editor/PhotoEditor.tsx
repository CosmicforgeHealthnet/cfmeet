"use client";

import { useEffect, useRef, useState, type ReactNode } from "react";
import {
  Check, Crop as CropIcon, Eraser, FlipHorizontal2, FlipVertical2, Loader2, Pencil, Redo2, RotateCcw, Send, SlidersHorizontal, Smile, Sparkles, Type, Undo2, X,
} from "lucide-react";
import { cn } from "@/lib/utils";
import {
  ASPECT_PRESETS, containSize, cropPixels, FULL_CROP, fitAspect, flipCropH, flipCropV, orientedSize, presetRatio, rotateBy, rotateCrop, zoomCrop, type Crop,
} from "@/lib/media-editor/crop";
import { FILTER_PRESETS, NO_ADJUSTMENTS, type Adjustments } from "@/lib/media-editor/filters";
import { BRUSH_SIZES, createSticker, createText, OVERLAY_COLORS, updateOverlay, type TextOverlay } from "@/lib/media-editor/overlays";
import {
  decodePhoto, DEFAULT_EDITS, drawPhoto, exportPhoto, initialPhotoState, PHOTO_SIDES, type ExportedImage, type PhotoEdits, type PhotoEditorState, type Source,
} from "@/lib/media-editor/photo";
import CropBox from "./CropBox";
import EmojiPicker from "./EmojiPicker";
import OverlayLayer from "./OverlayLayer";
import TextOverlayEditor, { DEFAULT_TEXT_DRAFT, type TextDraft } from "./TextOverlayEditor";
import { ColorRow, EditorSlider, pixelRatio, RoundButton, Segmented, useElementSize, useHistory } from "./ui";

export interface PhotoEditorResult extends ExportedImage {
  /** The edit state that produced the image (to re-open or persist the edit). */
  state: PhotoEditorState;
}

export interface PhotoEditorProps {
  file: File | Blob;
  /** Called with the rendered image; a returned promise keeps the Done button busy. */
  onDone: (result: PhotoEditorResult) => void | Promise<void>;
  onCancel: () => void;
  /** Extra round buttons at the end of the top toolbar. */
  toolbarExtra?: ReactNode;
  /** Shown above the Done button (e.g. a caption field). */
  footer?: ReactNode;
  /** Start from a saved edit. */
  initialState?: PhotoEditorState;
  /** Every committed change (undo / redo included). */
  onStateChange?: (state: PhotoEditorState) => void;
  /** Accessible name and icon of the Done button. */
  doneLabel?: string;
  /** Shown just before the done button (the status audience button). */
  doneExtra?: ReactNode;
  doneIcon?: ReactNode;
}

type Mode = "main" | "crop" | "filter" | "adjust" | "draw" | "sticker";

const DEFAULT_BRUSH = 1;

/**
 * Immersive photo editor: crop (free / presets), resize, rotate, flip, zoom & reposition,
 * filters, brightness / contrast / saturation, text, emoji stickers and drawing, with undo /
 * redo across everything. Outputs a JPEG or WebP (smaller wins) of at most 2560 px.
 */
export default function PhotoEditor({ file, onDone, onCancel, toolbarExtra, footer, initialState, onStateChange, doneLabel = "Done", doneIcon, doneExtra }: PhotoEditorProps) {
  const [src, setSrc] = useState<Source | null>(null);
  const [thumb, setThumb] = useState<Source | null>(null);
  const [error, setError] = useState<string | null>(null);
  const history = useHistory<PhotoEditorState>(() => initialState ?? initialPhotoState());
  const { present, commit, undo, redo } = history;
  const [draftEdits, setDraftEdits] = useState<PhotoEdits | null>(null);
  const [mode, setMode] = useState<Mode>("main");
  const [aspectId, setAspectId] = useState("free");
  const [text, setText] = useState<{ id?: string; draft: TextDraft } | null>(null);
  const [brushColor, setBrushColor] = useState(OVERLAY_COLORS[2]);
  const [brushSize, setBrushSize] = useState(DEFAULT_BRUSH);
  const [eraser, setEraser] = useState(false);
  const [gesture, setGesture] = useState(false);
  const [busy, setBusy] = useState(false);
  const [setStage, stage] = useElementSize<HTMLDivElement>();
  const canvasRef = useRef<HTMLCanvasElement>(null);

  const edits = draftEdits ?? present.edits;

  // Decode once per file (EXIF orientation applied), plus a tiny copy for filter thumbnails.
  useEffect(() => {
    let live = true;
    const opened: { close(): void }[] = [];
    decodePhoto(file)
      .then(async (s) => {
        opened.push(s);
        if (!live) return;
        setSrc(s);
        const k = 160 / Math.max(s.width, s.height);
        const t = await createImageBitmap(s.image as ImageBitmap, { resizeWidth: Math.max(1, Math.round(s.width * k)), resizeHeight: Math.max(1, Math.round(s.height * k)), resizeQuality: "medium" });
        opened.push({ close: () => t.close() });
        if (live) setThumb({ image: t, width: t.width, height: t.height });
      })
      .catch(() => { if (live) setError("This photo can't be opened here. Try a JPEG or PNG."); });
    return () => { live = false; opened.forEach((o) => o.close()); };
  }, [file]);

  useEffect(() => { onStateChange?.(present); }, [present, onStateChange]);

  const o = src ? orientedSize(src.width, src.height, edits.rotation) : { width: 1, height: 1 };
  const cp = cropPixels(edits.crop, o.width, o.height);
  const cropping = mode === "crop";
  const aspect = cropping ? o.width / o.height : cp.width / cp.height;
  const inset = cropping ? { top: 76, bottom: 20, x: 24 } : { top: 0, bottom: 0, x: 0 };
  const availH = Math.max(0, stage.height - inset.top - inset.bottom);
  const disp = containSize(aspect, Math.max(0, stage.width - inset.x * 2), availH);
  const dispLeft = (stage.width - disp.width) / 2;
  const dispTop = inset.top + (availH - disp.height) / 2;

  // Paint the photo with the same pipeline the export uses.
  useEffect(() => {
    const canvasEl = canvasRef.current;
    if (!canvasEl || !src || !disp.width || !disp.height) return;
    const k = Math.min(pixelRatio(2), 2048 / Math.max(disp.width, disp.height));
    const w = Math.max(1, Math.round(disp.width * k));
    const h = Math.max(1, Math.round(disp.height * k));
    canvasEl.width = w;
    canvasEl.height = h;
    const ctx = canvasEl.getContext("2d", { willReadFrequently: true });
    if (!ctx) return;
    drawPhoto(ctx, src, cropping ? { ...edits, crop: FULL_CROP } : edits, w, h);
  }, [src, edits, cropping, disp.width, disp.height]);

  const commitEdits = (patch: Partial<PhotoEdits>) => {
    setDraftEdits(null);
    commit({ ...present, edits: { ...present.edits, ...patch } });
  };
  const ratio = (() => {
    const p = ASPECT_PRESETS.find((a) => a.id === aspectId) ?? ASPECT_PRESETS[0];
    return presetRatio(p, o.width, o.height);
  })();

  const closePanel = () => { setDraftEdits(null); setMode("main"); setEraser(false); };

  const finish = async () => {
    if (!src || busy) return;
    setBusy(true);
    setError(null);
    try {
      const out = await exportPhoto(src, present);
      await onDone({ ...out, state: present });
    } catch {
      setError("The photo couldn't be saved. Try again.");
    } finally {
      setBusy(false);
    }
  };

  // Escape backs out of a tool, then cancels; Ctrl/Cmd+Z undoes, +Shift (or Y) redoes.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const typing = e.target instanceof HTMLElement && (e.target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName) && (e.target as HTMLInputElement).type !== "range");
      if (e.key === "Escape") {
        e.preventDefault();
        if (mode !== "main") { setDraftEdits(null); setMode("main"); setEraser(false); } else onCancel();
        return;
      }
      if (typing || !(e.ctrlKey || e.metaKey)) return;
      const k = e.key.toLowerCase();
      if (k === "z" && !e.shiftKey) { e.preventDefault(); setDraftEdits(null); undo(); }
      else if ((k === "z" && e.shiftKey) || k === "y") { e.preventDefault(); setDraftEdits(null); redo(); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mode, onCancel, undo, redo]);

  const rotate = () => commitEdits({ rotation: rotateBy(edits.rotation, -1), crop: rotateCrop(edits.crop, -1) });
  const flip = (axis: "h" | "v") => commitEdits(axis === "h" ? { flipH: !edits.flipH, crop: flipCropH(edits.crop) } : { flipV: !edits.flipV, crop: flipCropV(edits.crop) });
  const chooseAspect = (id: string) => {
    setAspectId(id);
    const p = ASPECT_PRESETS.find((a) => a.id === id);
    const r = p ? presetRatio(p, o.width, o.height) : null;
    if (r) commitEdits({ crop: fitAspect(edits.crop, r, o.width, o.height) });
  };
  const zoom = Math.min(1 / edits.crop.w, 1 / edits.crop.h);

  const addSticker = (emoji: string) => {
    commit({ ...present, overlays: [...present.overlays, createSticker(emoji)] });
    setMode("main");
  };
  const saveText = (draft: TextDraft) => {
    const id = text?.id;
    setText(null);
    if (id) commit({ ...present, overlays: updateOverlay<TextOverlay>(present.overlays, id, (t) => ({ ...t, ...draft })) });
    else commit({ ...present, overlays: [...present.overlays, createText(draft)] });
  };
  const editText = (t: TextOverlay) => setText({ id: t.id, draft: { text: t.text, font: t.font, color: t.color, background: t.background, align: t.align, size: t.size } });

  const shownOverlays = text?.id ? present.overlays.filter((x) => x.id !== text.id) : present.overlays;
  const chrome = cn("transition-opacity duration-150", gesture && "pointer-events-none opacity-0");
  const nativeLong = Math.max(cp.width, cp.height);

  return (
    <div className="fixed inset-0 z-[80] flex flex-col overflow-hidden bg-black text-white" role="dialog" aria-modal="true" aria-label="Edit photo" data-testid="photo-editor">
      <div ref={setStage} className="relative min-h-0 flex-1">
        {!src && !error ? <Loader2 className="absolute left-1/2 top-1/2 size-10 -translate-x-1/2 -translate-y-1/2 animate-spin text-white/70" aria-label="Opening photo" /> : null}
        {src ? (
          <div className="absolute" style={{ left: dispLeft, top: dispTop, width: disp.width, height: disp.height }} data-testid="photo-stage">
            <canvas ref={canvasRef} className="absolute inset-0 size-full" aria-label="Photo preview" role="img" data-testid="photo-canvas" />
            {cropping ? (
              <CropBox crop={edits.crop} ratio={ratio} imageWidth={o.width} imageHeight={o.height}
                onInput={(crop) => setDraftEdits({ ...present.edits, crop })} onCommit={(crop: Crop) => commitEdits({ crop })} />
            ) : (
              <OverlayLayer overlays={shownOverlays} onChange={(overlays) => commit({ ...present, overlays })}
                tool={mode === "draw" ? (eraser ? "erase" : "draw") : text || mode === "sticker" ? "none" : "move"}
                brush={{ color: brushColor, width: BRUSH_SIZES[brushSize] }} onEditText={editText} onGestureChange={setGesture} />
            )}
          </div>
        ) : null}

        {/* Top toolbar */}
        <div className={cn("absolute inset-x-0 top-0 z-20 flex items-start justify-between gap-2 bg-gradient-to-b from-black/50 to-transparent px-3 pb-6 pt-[max(env(safe-area-inset-top),0.75rem)]", chrome)}>
          {mode === "main" ? (
            <>
              <RoundButton label="Cancel" onClick={onCancel}><X className="size-6" /></RoundButton>
              <div className="flex items-center justify-end gap-1.5 sm:gap-2">
                <RoundButton label="Undo" disabled={!history.canUndo} onClick={() => { setDraftEdits(null); history.undo(); }}><Undo2 className="size-5" /></RoundButton>
                <RoundButton label="Redo" disabled={!history.canRedo} onClick={() => { setDraftEdits(null); history.redo(); }}><Redo2 className="size-5" /></RoundButton>
                <RoundButton label="Crop and rotate" disabled={!src} onClick={() => setMode("crop")}><CropIcon className="size-5" /></RoundButton>
                <RoundButton label="Stickers" disabled={!src} onClick={() => setMode("sticker")}><Smile className="size-5" /></RoundButton>
                <RoundButton label="Add text" disabled={!src} onClick={() => setText({ draft: DEFAULT_TEXT_DRAFT })}><Type className="size-5" /></RoundButton>
                <RoundButton label="Draw" disabled={!src} onClick={() => setMode("draw")}><Pencil className="size-5" /></RoundButton>
                {toolbarExtra}
              </div>
            </>
          ) : mode === "draw" ? (
            <>
              <RoundButton label="Undo" disabled={!history.canUndo} onClick={history.undo}><Undo2 className="size-5" /></RoundButton>
              <div className="flex items-center gap-2">
                <RoundButton label="Eraser" active={eraser} onClick={() => setEraser(!eraser)}><Eraser className="size-5" /></RoundButton>
                <PanelDone onClick={closePanel} />
              </div>
            </>
          ) : mode === "crop" ? (
            <>
              <RoundButton label="Reset crop" onClick={() => { setAspectId("free"); commitEdits({ crop: FULL_CROP, rotation: 0, flipH: false, flipV: false, maxSide: DEFAULT_EDITS.maxSide }); }}><span className="text-xs font-semibold">Reset</span></RoundButton>
              <PanelDone onClick={closePanel} />
            </>
          ) : (
            <>
              <span />
              <PanelDone onClick={closePanel} />
            </>
          )}
        </div>

        {error ? <p role="alert" className="absolute inset-x-4 top-20 z-30 rounded-xl bg-red-600/90 px-4 py-3 text-center text-sm">{error}</p> : null}

        {text ? <TextOverlayEditor initial={text.draft} onDone={saveText} onCancel={() => setText(null)} /> : null}
        {mode === "sticker" ? (
          <div className="absolute inset-x-0 bottom-0 z-30"><EmojiPicker onPick={addSticker} onClose={() => setMode("main")} /></div>
        ) : null}
      </div>

      {/* Bottom panels */}
      <div className={cn("relative z-20 space-y-3 bg-black px-3 pt-3 pb-[max(env(safe-area-inset-bottom),0.75rem)]", chrome, mode === "sticker" && "hidden")}>
        {mode === "crop" ? (
          <div className="space-y-3" data-testid="crop-panel">
            <Segmented label="Aspect ratio" value={aspectId} onChange={chooseAspect} options={ASPECT_PRESETS.map((p) => ({ value: p.id, label: p.label }))} />
            <div className="flex items-center gap-2">
              <RoundButton label="Rotate 90 degrees" onClick={rotate}><RotateCcw className="size-5" /></RoundButton>
              <RoundButton label="Flip horizontally" active={edits.flipH} onClick={() => flip("h")}><FlipHorizontal2 className="size-5" /></RoundButton>
              <RoundButton label="Flip vertically" active={edits.flipV} onClick={() => flip("v")}><FlipVertical2 className="size-5" /></RoundButton>
              <div className="min-w-0 flex-1">
                <EditorSlider label="Zoom" min={1} max={8} step={0.01} value={Math.round(zoom * 100) / 100} format={(v) => `${v.toFixed(1)}×`}
                  onInput={(v) => setDraftEdits({ ...present.edits, crop: zoomCrop(present.edits.crop, v / Math.min(1 / present.edits.crop.w, 1 / present.edits.crop.h)) })}
                  onCommit={() => { if (draftEdits) commitEdits({ crop: draftEdits.crop }); }} />
              </div>
            </div>
            <Segmented label="Output size (longest side)" value={edits.maxSide} onChange={(maxSide) => commitEdits({ maxSide })}
              options={[
                { value: PHOTO_SIDES[0] as number, label: `Full · ${Math.min(PHOTO_SIDES[0], nativeLong)} px`, aria: "Full size" },
                ...PHOTO_SIDES.slice(1).filter((s) => s < Math.min(PHOTO_SIDES[0], nativeLong)).map((s) => ({ value: s as number, label: `${s} px`, aria: `${s} pixels` })),
              ]} />
          </div>
        ) : mode === "filter" ? (
          <div role="radiogroup" aria-label="Filters" className="flex gap-3 overflow-x-auto pb-1 [scrollbar-width:none]" data-testid="filter-strip">
            {FILTER_PRESETS.map((f) => (
              <button key={f.id} type="button" role="radio" aria-checked={edits.filter === f.id} onClick={() => commitEdits({ filter: f.id })}
                className="flex shrink-0 flex-col items-center gap-1 rounded-xl p-1 text-xs focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary">
                <FilterThumb src={thumb} edits={{ ...edits, filter: f.id }} active={edits.filter === f.id} />
                <span className={cn(edits.filter === f.id ? "font-semibold text-white" : "text-white/70")}>{f.label}</span>
              </button>
            ))}
          </div>
        ) : mode === "adjust" ? (
          <div className="space-y-3" data-testid="adjust-panel">
            {(["brightness", "contrast", "saturation"] as const).map((k) => (
              <EditorSlider key={k} label={k[0].toUpperCase() + k.slice(1)} min={-100} max={100} value={edits.adjust[k]}
                onInput={(v) => setDraftEdits({ ...present.edits, adjust: { ...(draftEdits ?? present.edits).adjust, [k]: v } as Adjustments })}
                onCommit={() => { if (draftEdits) commitEdits({ adjust: draftEdits.adjust }); }} />
            ))}
            <button type="button" onClick={() => commitEdits({ adjust: NO_ADJUSTMENTS })} className="text-sm text-white/70 underline-offset-2 hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary">Reset adjustments</button>
          </div>
        ) : mode === "draw" ? (
          <div className="space-y-2" data-testid="draw-panel">
            <ColorRow label="Brush colour" colors={OVERLAY_COLORS} value={brushColor} onChange={(c) => { setBrushColor(c); setEraser(false); }} />
            <Segmented label="Brush size" value={brushSize} onChange={(s) => { setBrushSize(s); setEraser(false); }}
              options={BRUSH_SIZES.map((_, i) => ({ value: i, aria: `Brush size ${i + 1}`, label: <span className="block rounded-full bg-current" style={{ width: 6 + i * 5, height: 6 + i * 5 }} /> }))} />
          </div>
        ) : (
          <>
            {footer}
            <div className="flex items-center justify-between gap-2">
              <div className="flex items-center gap-2">
                <button type="button" disabled={!src} onClick={() => setMode("filter")} className={pill}><Sparkles className="size-4" />Filters</button>
                <button type="button" disabled={!src} onClick={() => setMode("adjust")} className={pill}><SlidersHorizontal className="size-4" />Adjust</button>
              </div>
              <div className="flex min-w-0 items-center gap-2">
              {doneExtra}
              <button type="button" aria-label={doneLabel} disabled={!src || busy} onClick={() => void finish()} data-testid="photo-done"
                className="flex size-14 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-lg focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white focus-visible:ring-offset-2 focus-visible:ring-offset-black disabled:opacity-50">
                {busy ? <Loader2 className="size-6 animate-spin" /> : doneIcon ?? <Send className="size-6" />}
              </button>
              </div>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

const pill = "flex h-10 items-center gap-1.5 rounded-full bg-white/10 px-3.5 text-sm font-medium text-white hover:bg-white/20 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary disabled:opacity-40";

function PanelDone({ onClick }: { onClick: () => void }) {
  return (
    <button type="button" onClick={onClick} data-testid="panel-done"
      className="flex h-11 items-center gap-1.5 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground shadow focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white">
      <Check className="size-4" />Done
    </button>
  );
}

function FilterThumb({ src, edits, active }: { src: Source | null; edits: PhotoEdits; active: boolean }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el || !src) return;
    const o = orientedSize(src.width, src.height, edits.rotation);
    const c = cropPixels(edits.crop, o.width, o.height);
    const k = 72 / Math.min(c.width, c.height);
    el.width = Math.max(1, Math.round(c.width * k));
    el.height = Math.max(1, Math.round(c.height * k));
    const ctx = el.getContext("2d", { willReadFrequently: true });
    if (ctx) drawPhoto(ctx, src, edits, el.width, el.height);
  }, [src, edits]);
  return <canvas ref={ref} aria-hidden className={cn("size-16 rounded-xl bg-white/10 object-cover", active && "ring-2 ring-primary ring-offset-2 ring-offset-black")} />;
}
