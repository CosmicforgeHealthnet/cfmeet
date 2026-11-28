"use client";

import { forwardRef, useCallback, useEffect, useReducer, useState, type ButtonHTMLAttributes, type ReactNode } from "react";
import { cn } from "@/lib/utils";
import { canRedo, canUndo, historyReducer, initHistory, type History, type HistoryAction } from "@/lib/media-editor/history";

/** Floating round icon button used across the immersive editors. */
export const RoundButton = forwardRef<HTMLButtonElement, ButtonHTMLAttributes<HTMLButtonElement> & { label: string; active?: boolean }>(
  function RoundButton({ label, active, className, children, ...rest }, ref) {
    return (
      <button ref={ref} type="button" aria-label={label} title={label} aria-pressed={active === undefined ? undefined : active} {...rest}
        className={cn(
          "flex size-10 shrink-0 items-center sm:size-11 justify-center rounded-full bg-black/45 text-white backdrop-blur transition-colors hover:bg-black/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary focus-visible:ring-offset-2 focus-visible:ring-offset-black disabled:opacity-40",
          active && "bg-white text-black hover:bg-white/90",
          className,
        )}>
        {children}
      </button>
    );
  },
);

/** A horizontal row of colour swatches. */
export function ColorRow({ colors, value, onChange, label, className }: { colors: readonly string[]; value: string; onChange: (c: string) => void; label: string; className?: string }) {
  return (
    <div role="radiogroup" aria-label={label} className={cn("flex items-center gap-2 overflow-x-auto px-1 py-1 [scrollbar-width:none]", className)}>
      {colors.map((c) => (
        <button key={c} type="button" role="radio" aria-checked={c.toLowerCase() === value.toLowerCase()} aria-label={`Colour ${c}`} onClick={() => onChange(c)}
          className={cn("size-8 shrink-0 rounded-full border-2 border-white/80 shadow focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary", c.toLowerCase() === value.toLowerCase() && "scale-110 ring-2 ring-white ring-offset-2 ring-offset-black")}
          style={{ backgroundColor: c }} />
      ))}
    </div>
  );
}

/** A labelled range slider that reports live changes and the final value separately. */
export function EditorSlider({ label, value, min, max, step = 1, onInput, onCommit, format }: { label: string; value: number; min: number; max: number; step?: number; onInput: (v: number) => void; onCommit: (v: number) => void; format?: (v: number) => string }) {
  return (
    <label className="flex items-center gap-3 text-sm text-white">
      <span className="w-24 shrink-0 text-white/80">{label}</span>
      <input type="range" min={min} max={max} step={step} value={value} aria-label={label}
        onChange={(e) => onInput(Number(e.target.value))}
        onPointerUp={(e) => onCommit(Number(e.currentTarget.value))}
        onKeyUp={(e) => onCommit(Number(e.currentTarget.value))}
        onBlur={(e) => onCommit(Number(e.currentTarget.value))}
        className="h-2 min-w-0 flex-1 cursor-pointer accent-primary" />
      <span className="w-10 shrink-0 text-right tabular-nums text-white/80">{format ? format(value) : value}</span>
    </label>
  );
}

/** A segmented row of choices. */
export function Segmented<T extends string | number>({ options, value, onChange, label }: { options: readonly { value: T; label: ReactNode; aria?: string }[]; value: T; onChange: (v: T) => void; label: string }) {
  return (
    <div role="radiogroup" aria-label={label} className="flex items-center gap-1 overflow-x-auto [scrollbar-width:none]">
      {options.map((o) => (
        <button key={String(o.value)} type="button" role="radio" aria-checked={o.value === value} aria-label={o.aria} onClick={() => onChange(o.value)}
          className={cn("flex h-9 shrink-0 items-center justify-center gap-1 rounded-full px-3 text-sm font-medium text-white/85 hover:bg-white/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary", o.value === value && "bg-white text-black hover:bg-white")}>
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** Undo / redo over immutable snapshots. */
export function useHistory<T>(initial: T | (() => T)) {
  const [state, dispatch] = useReducer(historyReducer as (s: History<T>, a: HistoryAction<T>) => History<T>, undefined, () => initHistory(typeof initial === "function" ? (initial as () => T)() : initial));
  const commit = useCallback((value: T) => dispatch({ type: "commit", value }), []);
  const undo = useCallback(() => dispatch({ type: "undo" }), []);
  const redo = useCallback(() => dispatch({ type: "redo" }), []);
  const reset = useCallback((value: T) => dispatch({ type: "reset", value }), []);
  return { present: state.present, commit, undo, redo, reset, canUndo: canUndo(state), canRedo: canRedo(state) };
}

/** Size of an element, tracked with ResizeObserver. */
export function useElementSize<E extends HTMLElement>(): [(el: E | null) => void, { width: number; height: number }] {
  const [el, setEl] = useState<E | null>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
  useEffect(() => {
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => {
      const r = entry.contentRect;
      setSize((s) => (s.width === r.width && s.height === r.height ? s : { width: r.width, height: r.height }));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [el]);
  return [setEl, size];
}

/** Device pixel ratio, capped (canvas memory on phones). */
export function pixelRatio(max = 2): number {
  return typeof window === "undefined" ? 1 : Math.min(max, window.devicePixelRatio || 1);
}
