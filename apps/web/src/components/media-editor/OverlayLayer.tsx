"use client";

import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent } from "react";
import { RotateCw, Trash2, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { loadEditorFonts } from "@/lib/media-editor/fonts";
import {
  bringToFront, clamp, drawOverlays, eraseAt, extendStroke, handleTransform, hitTest, moveBy, overlayBox, overlayId, pinchTransform,
  removeOverlay, scaleTo, updateOverlay, type Overlay, type PlacedOverlay, type Pt, type StrokeOverlay, type TextOverlay,
} from "@/lib/media-editor/overlays";
import { EDITOR_FONT_CLASSES } from "./fonts";
import { pixelRatio, useElementSize } from "./ui";

export type OverlayTool = "move" | "draw" | "erase" | "none";

export interface OverlayLayerProps {
  /** Committed overlays (the layer keeps live gesture state itself). */
  overlays: Overlay[];
  /** Called once per finished edit (drag, pinch, stroke, erase, delete) with the new list. */
  onChange: (next: Overlay[]) => void;
  /** "move" selects and transforms items, "draw" / "erase" paint, "none" is display only. */
  tool?: OverlayTool;
  /** Brush for "draw": colour and diameter as a fraction of the media width. */
  brush?: { color: string; width: number };
  /** Tap on a text item. */
  onEditText?: (item: TextOverlay) => void;
  /** True while a finger / mouse gesture is in progress (editors hide their chrome). */
  onGestureChange?: (active: boolean) => void;
  className?: string;
}

type Gesture =
  | { kind: "drag"; id: string; start: PlacedOverlay; origin: Pt; moved: boolean; text: boolean }
  | { kind: "pinch"; id: string; start: PlacedOverlay; a0: Pt; b0: Pt; ids: [number, number] }
  | { kind: "handle"; id: string; start: PlacedOverlay; from: Pt }
  | { kind: "draw"; stroke: StrokeOverlay }
  | { kind: "erase" };

const ERASER_RADIUS = 14;

/**
 * Interactive overlay layer: place it exactly over the media (absolute inset-0 inside a box
 * with the media's aspect). The canvas paints with the same `drawOverlays` used for exports,
 * so the export matches the screen. Gestures: drag to move, pinch or the corner handle to
 * resize / rotate, tap text to edit, drag onto the bin (or the x) to delete. Keyboard: arrows
 * nudge (Shift: more), +/- scale, [ ] rotate, Delete removes, Enter edits text.
 */
export default function OverlayLayer({ overlays, onChange, tool = "move", brush = { color: "#ffffff", width: 0.016 }, onEditText, onGestureChange, className }: OverlayLayerProps) {
  const [setBox, size] = useElementSize<HTMLDivElement>();
  const root = useRef<HTMLDivElement | null>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const trash = useRef<HTMLDivElement>(null);
  const pointers = useRef(new Map<number, Pt>());
  const gesture = useRef<Gesture | null>(null);
  const draftRef = useRef<Overlay[] | null>(null);
  const [draft, setDraftState] = useState<Overlay[] | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false);
  const [overTrash, setOverTrash] = useState(false);
  const [fontsTick, setFontsTick] = useState(0);

  const list = draft ?? overlays;
  const W = size.width;
  const H = size.height;

  useEffect(() => {
    let live = true;
    void loadEditorFonts().then(() => { if (live) setFontsTick((t) => t + 1); });
    return () => { live = false; };
  }, []);

  // Paint (same renderer as the export), at device resolution.
  useEffect(() => {
    const c = canvas.current;
    if (!c || !W || !H) return;
    const dpr = pixelRatio(2.5);
    const cw = Math.round(W * dpr);
    const ch = Math.round(H * dpr);
    if (c.width !== cw) c.width = cw;
    if (c.height !== ch) c.height = ch;
    const ctx = c.getContext("2d");
    if (!ctx) return;
    ctx.clearRect(0, 0, cw, ch);
    drawOverlays(ctx, list, cw, ch);
  }, [list, W, H, fontsTick]);

  const setDraft = (next: Overlay[] | null) => { draftRef.current = next; setDraftState(next); };
  const current = () => draftRef.current ?? overlays;
  const local = (e: { clientX: number; clientY: number }): Pt => {
    const r = root.current?.getBoundingClientRect();
    return { x: e.clientX - (r?.left ?? 0), y: e.clientY - (r?.top ?? 0) };
  };
  const inTrash = (e: { clientX: number; clientY: number }) => {
    const r = trash.current?.getBoundingClientRect();
    return !!r && e.clientX >= r.left - 12 && e.clientX <= r.right + 12 && e.clientY >= r.top - 12 && e.clientY <= r.bottom + 12;
  };
  const begin = () => onGestureChange?.(true);
  const finish = (commit: Overlay[] | null) => {
    gesture.current = null;
    if (commit && commit !== overlays) onChange(commit);
    setDraft(null);
    setDragging(false);
    setOverTrash(false);
    onGestureChange?.(false);
  };
  const itemOf = (id: string) => current().find((o) => o.id === id) as PlacedOverlay | undefined;

  const startPinch = (id: string) => {
    const ids = [...pointers.current.keys()].slice(0, 2) as [number, number];
    const item = itemOf(id);
    if (!item || ids.length < 2) return;
    gesture.current = { kind: "pinch", id, start: item, a0: pointers.current.get(ids[0])!, b0: pointers.current.get(ids[1])!, ids };
    setDragging(false);
  };

  const onPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (tool === "none" || !W || !H) return;
    if (e.pointerType === "mouse" && e.button !== 0) return;
    const p = local(e);
    const g = gesture.current;
    // In "move", a press that misses every item (with no gesture running) is not ours: it
    // bubbles to the editor underneath (e.g. collage cells pan).
    const hit = tool === "move" && !g ? hitTest(current(), p.x, p.y, W, H) : null;
    if (tool === "move" && !g && !hit) { setSelectedId(null); return; }
    e.stopPropagation();
    e.currentTarget.setPointerCapture(e.pointerId);
    pointers.current.set(e.pointerId, p);

    if (tool === "draw") {
      if (g) return;
      const stroke: StrokeOverlay = { id: overlayId(), type: "stroke", color: brush.color, width: brush.width, points: [clamp(p.x / W, 0, 1), clamp(p.y / H, 0, 1)] };
      gesture.current = { kind: "draw", stroke };
      setDraft([...overlays, stroke]);
      begin();
      return;
    }
    if (tool === "erase") {
      if (g) return;
      gesture.current = { kind: "erase" };
      setDraft(eraseAt(overlays, p.x, p.y, W, H, ERASER_RADIUS));
      begin();
      return;
    }
    // move
    if (g) {
      if (pointers.current.size >= 2 && "id" in g) startPinch(g.id);
      return;
    }
    if (!hit) return;
    setSelectedId(hit.id);
    const front = bringToFront(current(), hit.id);
    setDraft(front);
    gesture.current = { kind: "drag", id: hit.id, start: hit, origin: p, moved: false, text: hit.type === "text" };
    begin();
  };

  const onPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!pointers.current.has(e.pointerId)) return;
    const p = local(e);
    pointers.current.set(e.pointerId, p);
    const g = gesture.current;
    if (!g) return;
    const base = current();
    if (g.kind === "draw") {
      const next = extendStroke(g.stroke, p.x, p.y, W, H);
      if (next !== g.stroke) { g.stroke = next; setDraft(base.map((o) => (o.id === next.id ? next : o))); }
    } else if (g.kind === "erase") {
      const next = eraseAt(base, p.x, p.y, W, H, ERASER_RADIUS);
      if (next !== base) setDraft(next);
    } else if (g.kind === "drag") {
      const dx = p.x - g.origin.x;
      const dy = p.y - g.origin.y;
      if (!g.moved && Math.hypot(dx, dy) < 5) return;
      if (!g.moved) { g.moved = true; setDragging(true); }
      setDraft(updateOverlay<PlacedOverlay>(base, g.id, () => moveBy(g.start, dx, dy, W, H)));
      setOverTrash(inTrash(e));
    } else if (g.kind === "pinch") {
      const a1 = pointers.current.get(g.ids[0]);
      const b1 = pointers.current.get(g.ids[1]);
      if (!a1 || !b1) return;
      setDraft(updateOverlay<PlacedOverlay>(base, g.id, () => pinchTransform(g.start, g.a0, g.b0, a1, b1, W, H)));
    } else if (g.kind === "handle") {
      setDraft(updateOverlay<PlacedOverlay>(base, g.id, () => handleTransform(g.start, g.from, p, W, H)));
    }
  };

  const onPointerUp = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!pointers.current.has(e.pointerId)) return;
    pointers.current.delete(e.pointerId);
    const g = gesture.current;
    if (!g) return;
    if (g.kind === "pinch") {
      if (pointers.current.size > 0 && !g.ids.includes(e.pointerId)) return;
      pointers.current.clear();
      finish(draftRef.current);
      return;
    }
    if (pointers.current.size > 0) return;
    if (g.kind === "drag") {
      if (g.moved && inTrash(e)) { setSelectedId(null); finish(removeOverlay(current(), g.id)); return; }
      if (!g.moved) {
        // A tap: text opens for editing; nothing else changes (not even the stacking order).
        finish(null);
        const item = overlays.find((o) => o.id === g.id);
        if (g.text && item?.type === "text") onEditText?.(item);
        return;
      }
    }
    if (g.kind === "draw" && g.stroke.points.length === 2) {
      // A dot.
      finish(current());
      return;
    }
    finish(draftRef.current);
  };

  const onHandleDown = (e: ReactPointerEvent<HTMLButtonElement>, id: string) => {
    e.stopPropagation();
    e.preventDefault();
    const item = itemOf(id);
    if (!item) return;
    root.current?.setPointerCapture(e.pointerId);
    const p = local(e);
    pointers.current.set(e.pointerId, p);
    gesture.current = { kind: "handle", id, start: item, from: p };
    begin();
  };

  const remove = (id: string) => {
    setSelectedId(null);
    onChange(removeOverlay(overlays, id));
  };

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (tool !== "move" || !selectedId) return;
    const found = overlays.find((o) => o.id === selectedId);
    const item = found && found.type !== "stroke" ? found : undefined;
    if (!item) return;
    const step = e.shiftKey ? 0.05 : 0.01;
    let next: PlacedOverlay | null = null;
    switch (e.key) {
      case "ArrowLeft": next = moveBy(item, -step * W, 0, W, H); break;
      case "ArrowRight": next = moveBy(item, step * W, 0, W, H); break;
      case "ArrowUp": next = moveBy(item, 0, -step * H, W, H); break;
      case "ArrowDown": next = moveBy(item, 0, step * H, W, H); break;
      case "+": case "=": next = scaleTo(item, item.scale * 1.1); break;
      case "-": case "_": next = scaleTo(item, item.scale / 1.1); break;
      case "]": next = { ...item, rotation: item.rotation + Math.PI / 36 }; break;
      case "[": next = { ...item, rotation: item.rotation - Math.PI / 36 }; break;
      case "Delete": case "Backspace": e.preventDefault(); remove(item.id); return;
      case "Enter": if (item.type === "text") { e.preventDefault(); onEditText?.(item); } return;
      case "Escape": e.stopPropagation(); setSelectedId(null); return;
      default: return;
    }
    e.preventDefault();
    onChange(updateOverlay(overlays, item.id, () => next!));
  };

  const selected = tool === "move" && selectedId ? (list.find((o) => o.id === selectedId) as PlacedOverlay | undefined) : undefined;
  const box = selected && W ? overlayBox(selected, W, H) : null;

  return (
    <div
      ref={(el) => { root.current = el; setBox(el); }}
      className={cn("absolute inset-0 touch-none select-none outline-none focus-visible:ring-2 focus-visible:ring-primary/70", tool === "none" ? "pointer-events-none" : tool === "move" ? "cursor-default" : "cursor-crosshair", className)}
      tabIndex={tool === "none" ? -1 : 0}
      role="application"
      aria-label={tool === "draw" ? "Drawing area" : tool === "erase" ? "Eraser area" : "Stickers and text. Select an item, then use the arrow keys to move it."}
      onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp} onPointerCancel={onPointerUp}
      onKeyDown={onKeyDown}
      data-testid="overlay-layer"
    >
      <span aria-hidden className={cn("pointer-events-none absolute size-0 overflow-hidden", EDITOR_FONT_CLASSES)}>.</span>
      <canvas ref={canvas} className="pointer-events-none absolute inset-0 size-full" aria-hidden data-testid="overlay-canvas" />
      {box && selected && !dragging ? (
        <div className="pointer-events-none absolute rounded-md border-2 border-dashed border-white/90 shadow-[0_0_0_1px_rgba(0,0,0,.35)]"
          style={{ left: box.cx, top: box.cy, width: box.width + 12, height: box.height + 12, transform: `translate(-50%, -50%) rotate(${box.rotation}rad)` }}
          data-testid="overlay-selection">
          <button type="button" aria-label="Delete item" onPointerDown={(e) => e.stopPropagation()} onClick={() => remove(selected.id)}
            className="pointer-events-auto absolute -left-4 -top-4 flex size-8 items-center justify-center rounded-full bg-white text-black shadow focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary">
            <X className="size-4" />
          </button>
          <button type="button" aria-label="Resize and rotate" onPointerDown={(e) => onHandleDown(e, selected.id)} data-testid="overlay-handle"
            className="pointer-events-auto absolute -bottom-4 -right-4 flex size-8 touch-none items-center justify-center rounded-full bg-white text-black shadow focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary">
            <RotateCw className="size-4" />
          </button>
        </div>
      ) : null}
      {dragging ? (
        <div ref={trash} className={cn("pointer-events-none absolute bottom-6 left-1/2 flex size-14 -translate-x-1/2 items-center justify-center rounded-full border-2 border-white/80 bg-black/50 text-white transition-transform", overTrash && "scale-125 border-red-400 bg-red-500")} aria-hidden data-testid="overlay-trash">
          <Trash2 className="size-6" />
        </div>
      ) : null}
    </div>
  );
}
