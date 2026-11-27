"use client";

import { useRef, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent, type WheelEvent as ReactWheelEvent } from "react";
import { moveCrop, resizeCrop, zoomCrop, type Crop, type CropHandle } from "@/lib/media-editor/crop";

interface Props {
  crop: Crop;
  /** Pixel aspect to keep (null = free). */
  ratio: number | null;
  /** Oriented image size in pixels (for the aspect maths). */
  imageWidth: number;
  imageHeight: number;
  /** Live change while dragging. */
  onInput: (c: Crop) => void;
  /** Final value at the end of a gesture. */
  onCommit: (c: Crop) => void;
}

const HANDLES: { h: CropHandle; cls: string; label: string }[] = [
  { h: "nw", cls: "-left-2 -top-2 cursor-nwse-resize", label: "top left" },
  { h: "ne", cls: "-right-2 -top-2 cursor-nesw-resize", label: "top right" },
  { h: "sw", cls: "-bottom-2 -left-2 cursor-nesw-resize", label: "bottom left" },
  { h: "se", cls: "-bottom-2 -right-2 cursor-nwse-resize", label: "bottom right" },
  { h: "n", cls: "left-1/2 -top-2 -translate-x-1/2 cursor-ns-resize", label: "top" },
  { h: "s", cls: "left-1/2 -bottom-2 -translate-x-1/2 cursor-ns-resize", label: "bottom" },
  { h: "w", cls: "-left-2 top-1/2 -translate-y-1/2 cursor-ew-resize", label: "left" },
  { h: "e", cls: "-right-2 top-1/2 -translate-y-1/2 cursor-ew-resize", label: "right" },
];

type Drag =
  | { kind: "move" | CropHandle; start: Crop; x: number; y: number; w: number; h: number }
  | { kind: "pinch"; start: Crop; d0: number };

/**
 * Crop rectangle over the full oriented image (fills its positioned parent). Drag inside to
 * move, drag the handles to resize, pinch or scroll to zoom; arrow keys move it (Shift +
 * arrows resize).
 */
export default function CropBox({ crop, ratio, imageWidth, imageHeight, onInput, onCommit }: Props) {
  const area = useRef<HTMLDivElement>(null);
  const drag = useRef<Drag | null>(null);
  const pointers = useRef(new Map<number, { x: number; y: number }>());
  const live = useRef<Crop>(crop);
  const wheelTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const emit = (c: Crop) => { live.current = c; onInput(c); };

  const down = (e: ReactPointerEvent, kind: "move" | CropHandle) => {
    e.stopPropagation();
    e.preventDefault();
    const r = area.current?.getBoundingClientRect();
    if (!r) return;
    area.current?.setPointerCapture(e.pointerId);
    pointers.current.set(e.pointerId, { x: e.clientX, y: e.clientY });
    live.current = crop;
    if (pointers.current.size === 2) {
      const [a, b] = [...pointers.current.values()];
      drag.current = { kind: "pinch", start: crop, d0: Math.hypot(b.x - a.x, b.y - a.y) || 1 };
      return;
    }
    drag.current = { kind, start: crop, x: e.clientX, y: e.clientY, w: r.width, h: r.height };
  };

  const move = (e: ReactPointerEvent) => {
    if (!pointers.current.has(e.pointerId)) return;
    pointers.current.set(e.pointerId, { x: e.clientX, y: e.clientY });
    const d = drag.current;
    if (!d) return;
    if (d.kind === "pinch") {
      const [a, b] = [...pointers.current.values()];
      if (!a || !b) return;
      emit(zoomCrop(d.start, Math.hypot(b.x - a.x, b.y - a.y) / d.d0));
      return;
    }
    const dx = (e.clientX - d.x) / d.w;
    const dy = (e.clientY - d.y) / d.h;
    emit(d.kind === "move" ? moveCrop(d.start, dx, dy) : resizeCrop(d.start, d.kind, dx, dy, ratio, imageWidth, imageHeight));
  };

  const up = (e: ReactPointerEvent) => {
    if (!pointers.current.delete(e.pointerId)) return;
    if (pointers.current.size > 0 && drag.current?.kind !== "pinch") return;
    pointers.current.clear();
    if (drag.current) { drag.current = null; onCommit(live.current); }
  };

  const wheel = (e: ReactWheelEvent) => {
    const base = wheelTimer.current ? live.current : crop;
    emit(zoomCrop(base, Math.exp(-e.deltaY * 0.0015)));
    if (wheelTimer.current) clearTimeout(wheelTimer.current);
    wheelTimer.current = setTimeout(() => { wheelTimer.current = null; onCommit(live.current); }, 250);
  };

  const key = (e: ReactKeyboardEvent) => {
    const step = 0.01;
    const map: Record<string, [number, number]> = { ArrowLeft: [-step, 0], ArrowRight: [step, 0], ArrowUp: [0, -step], ArrowDown: [0, step] };
    const v = map[e.key];
    if (!v) return;
    e.preventDefault();
    onCommit(e.shiftKey ? resizeCrop(crop, "se", v[0], v[1], ratio, imageWidth, imageHeight) : moveCrop(crop, v[0], v[1]));
  };

  return (
    <div ref={area} className="absolute inset-0 touch-none overflow-hidden" onPointerMove={move} onPointerUp={up} onPointerCancel={up} onWheel={wheel}
      onPointerDown={(e) => down(e, "move")} data-testid="crop-area">
      <div role="slider" tabIndex={0} aria-label="Crop area. Arrow keys move it, Shift and arrow keys resize it." aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(crop.w * 100)} aria-valuetext={`${Math.round(crop.w * 100)}% by ${Math.round(crop.h * 100)}%`}
        onKeyDown={key} onPointerDown={(e) => down(e, "move")}
        className="absolute cursor-move border-2 border-white shadow-[0_0_0_9999px_rgba(0,0,0,0.6)] outline-none focus-visible:border-primary"
        style={{ left: `${crop.x * 100}%`, top: `${crop.y * 100}%`, width: `${crop.w * 100}%`, height: `${crop.h * 100}%` }} data-testid="crop-box">
        <div aria-hidden className="pointer-events-none absolute inset-0 grid grid-cols-3 grid-rows-3">
          {Array.from({ length: 9 }, (_, i) => <span key={i} className="border border-white/25" />)}
        </div>
        {HANDLES.map(({ h, cls, label }) => (
          <span key={h} aria-hidden onPointerDown={(e) => down(e, h)} data-handle={h}
            className={`absolute size-5 rounded-full border-2 border-white bg-primary shadow ${h.length === 1 ? "size-4" : ""} ${cls}`} title={`Resize from ${label}`} />
        ))}
      </div>
    </div>
  );
}
