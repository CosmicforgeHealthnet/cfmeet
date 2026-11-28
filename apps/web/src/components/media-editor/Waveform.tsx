"use client";

import { useRef, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent } from "react";
import { cn } from "@/lib/utils";
import { formatClock, type AudioRange } from "@/lib/media-editor/audio";

/**
 * Waveform bars (0..1 peaks) with an optional played fraction, and optionally a draggable
 * selection (trim handles + drag the window) over a clip of `totalMs`. Keyboard: the handles
 * are sliders (arrows ± 0.1 s, Shift ± 1 s); the window moves with arrows when focused.
 */
export default function Waveform({
  peaks, progress, totalMs, selection, onSelection, onSeek, maxSelectionMs, className, tone = "light", label = "Waveform", moveWindow = true,
}: {
  peaks: readonly number[];
  /** Played fraction 0..1 of the whole clip (bars before it are highlighted). */
  progress?: number;
  totalMs?: number;
  selection?: AudioRange | null;
  /** Called while a handle or the window is dragged (`done` once released). */
  onSelection?: (r: AudioRange, done: boolean) => void;
  onSeek?: (ms: number) => void;
  maxSelectionMs?: number;
  className?: string;
  tone?: "light" | "dark";
  label?: string;
  /** Dragging inside the selection slides it (music); off for voice trimming. */
  moveWindow?: boolean;
}) {
  const track = useRef<HTMLDivElement>(null);
  const drag = useRef<{ kind: "start" | "end" | "move" | "seek"; x0: number; r0: AudioRange } | null>(null);
  const total = Math.max(1, totalMs ?? 1);
  const pct = (ms: number) => `${(Math.min(total, Math.max(0, ms)) / total) * 100}%`;
  const msAt = (clientX: number) => {
    const r = track.current!.getBoundingClientRect();
    return Math.round((Math.min(r.width, Math.max(0, clientX - r.left)) / r.width) * total);
  };
  const fix = (r: AudioRange): AudioRange => {
    const max = Math.min(total, maxSelectionMs ?? total);
    let { startMs, endMs } = r;
    startMs = Math.max(0, Math.min(startMs, total));
    endMs = Math.max(0, Math.min(endMs, total));
    if (endMs - startMs < Math.min(1000, total)) return drag.current?.r0 ?? r;
    if (endMs - startMs > max) {
      if (drag.current?.kind === "start") startMs = endMs - max; else endMs = startMs + max;
    }
    return { startMs: Math.round(startMs), endMs: Math.round(endMs) };
  };

  const down = (kind: "start" | "end" | "move" | "seek", e: ReactPointerEvent<HTMLElement>) => {
    if (kind !== "seek" && (!selection || !onSelection)) return;
    if (kind === "seek" && !onSeek && !(selection && onSelection && moveWindow)) return;
    e.preventDefault();
    e.stopPropagation();
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { kind, x0: msAt(e.clientX), r0: selection ?? { startMs: 0, endMs: total } };
    if (kind === "seek") onSeek?.(msAt(e.clientX));
  };
  const move = (e: ReactPointerEvent<HTMLElement>) => {
    const d = drag.current;
    if (!d) return;
    const ms = msAt(e.clientX);
    if (d.kind === "seek") { onSeek?.(ms); return; }
    if (!onSelection) return;
    if (d.kind === "start") onSelection(fix({ startMs: ms, endMs: d.r0.endMs }), false);
    else if (d.kind === "end") onSelection(fix({ startMs: d.r0.startMs, endMs: ms }), false);
    else {
      const len = d.r0.endMs - d.r0.startMs;
      const s = Math.max(0, Math.min(total - len, d.r0.startMs + ms - d.x0));
      onSelection({ startMs: Math.round(s), endMs: Math.round(s + len) }, false);
    }
  };
  const up = (e: ReactPointerEvent<HTMLElement>) => {
    if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId);
    const d = drag.current;
    drag.current = null;
    if (d && d.kind !== "seek" && selection && onSelection) onSelection(selection, true);
  };
  const key = (which: "start" | "end" | "move", e: ReactKeyboardEvent<HTMLElement>) => {
    if (!selection || !onSelection) return;
    const step = e.shiftKey ? 1000 : 100;
    const dir = e.key === "ArrowLeft" || e.key === "ArrowDown" ? -1 : e.key === "ArrowRight" || e.key === "ArrowUp" ? 1 : 0;
    if (!dir) return;
    e.preventDefault();
    e.stopPropagation();
    const r = selection;
    if (which === "move") {
      const len = r.endMs - r.startMs;
      const s = Math.max(0, Math.min(total - len, r.startMs + dir * step));
      onSelection({ startMs: s, endMs: s + len }, true);
    } else if (which === "start") onSelection(fix({ startMs: r.startMs + dir * step, endMs: r.endMs }), true);
    else onSelection(fix({ startMs: r.startMs, endMs: r.endMs + dir * step }), true);
  };

  const bar = tone === "light" ? "bg-white" : "bg-foreground";
  const played = progress ?? -1;
  return (
    <div ref={track} role="group" aria-label={label} className={cn("relative h-14 touch-none select-none", className)}
      onPointerDown={(e) => down(selection && onSelection && moveWindow ? "move" : "seek", e)} onPointerMove={move} onPointerUp={up} onPointerCancel={up}>
      <div className="absolute inset-0 flex items-center gap-[2px]" aria-hidden>
        {peaks.map((p, i) => {
          const at = (i + 0.5) / peaks.length;
          const inSel = !selection || (at * total >= selection.startMs && at * total <= selection.endMs);
          return (
            <span key={i} className={cn("min-w-[2px] flex-1 rounded-full transition-colors", bar, !inSel ? "opacity-25" : played >= 0 && at > played ? "opacity-50" : "opacity-100")}
              style={{ height: `${Math.max(8, p * 100)}%` }} />
          );
        })}
      </div>
      {selection && onSelection ? (
        <>
          <div tabIndex={0} role="group" aria-label={`Selected ${formatClock(selection.startMs)} to ${formatClock(selection.endMs)}${moveWindow ? ": drag or use arrow keys to move" : ""}`}
            onKeyDown={(e) => moveWindow && key("move", e)}
            className="pointer-events-none absolute inset-y-0 rounded-md border-2 border-primary bg-primary/15 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary"
            style={{ left: pct(selection.startMs), width: `calc(${pct(selection.endMs)} - ${pct(selection.startMs)})` }} />
          {(["start", "end"] as const).map((which) => (
            <div key={which} role="slider" tabIndex={0} aria-label={which === "start" ? "Start" : "End"}
              aria-valuemin={0} aria-valuemax={Math.round(total / 1000)} aria-valuenow={Math.round((which === "start" ? selection.startMs : selection.endMs) / 1000)}
              aria-valuetext={formatClock(which === "start" ? selection.startMs : selection.endMs)}
              onPointerDown={(e) => down(which, e)} onPointerMove={move} onPointerUp={up} onPointerCancel={up} onKeyDown={(e) => key(which, e)}
              className={cn("absolute inset-y-0 z-10 flex w-5 cursor-ew-resize touch-none items-center justify-center bg-primary focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white",
                which === "start" ? "-translate-x-full rounded-l-md" : "rounded-r-md")}
              style={{ left: pct(which === "start" ? selection.startMs : selection.endMs) }}>
              <span className="h-5 w-1 rounded-full bg-primary-foreground/70" />
            </div>
          ))}
        </>
      ) : null}
      {played >= 0 ? <div className="pointer-events-none absolute -inset-y-1 z-20 w-0.5 -translate-x-1/2 rounded-full bg-white shadow" style={{ left: `${Math.min(1, played) * 100}%` }} /> : null}
    </div>
  );
}
