"use client";

import { useEffect, useRef, useState } from "react";
import { AlignCenter, AlignLeft, AlignRight, Check } from "lucide-react";
import { cn } from "@/lib/utils";
import { EDITOR_FONTS, fontStyle } from "@/lib/media-editor/fonts";
import { contrastInk, OVERLAY_COLORS, TEXT_SIZE_DEFAULT, type TextAlign, type TextBackground, type TextOverlay } from "@/lib/media-editor/overlays";
import { ColorRow, RoundButton } from "./ui";
import "./fonts";

export type TextDraft = Pick<TextOverlay, "text" | "font" | "color" | "background" | "align" | "size">;

export const DEFAULT_TEXT_DRAFT: TextDraft = { text: "", font: 0, color: "#ffffff", background: "none", align: "center", size: TEXT_SIZE_DEFAULT };

const BG_CYCLE: TextBackground[] = ["none", "solid", "translucent"];
const ALIGN_CYCLE: TextAlign[] = ["center", "left", "right"];

/**
 * Full-screen text entry for an overlay (dimmed media behind): font, colour, background style,
 * alignment and size. Done with empty text deletes / cancels.
 */
export default function TextOverlayEditor({ initial, onDone, onCancel }: { initial?: TextDraft; onDone: (draft: TextDraft) => void; onCancel: () => void }) {
  const [draft, setDraft] = useState<TextDraft>(initial ?? DEFAULT_TEXT_DRAFT);
  const area = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    const el = area.current;
    if (!el) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") { e.stopPropagation(); e.preventDefault(); onCancel(); } };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onCancel]);

  const set = (patch: Partial<TextDraft>) => setDraft((d) => ({ ...d, ...patch }));
  const AlignIcon = draft.align === "left" ? AlignLeft : draft.align === "right" ? AlignRight : AlignCenter;
  const ink = draft.background === "solid" ? contrastInk(draft.color) : draft.color;
  const lines = Math.min(8, Math.max(1, draft.text.split("\n").length + Math.floor(draft.text.length / 22)));

  return (
    <div className="absolute inset-0 z-40 flex flex-col bg-black/65 backdrop-blur-[2px]" role="dialog" aria-label="Edit text" data-testid="text-overlay-editor">
      <div className="flex items-center justify-between gap-2 px-3 pt-[max(env(safe-area-inset-top),0.75rem)]">
        <div className="flex items-center gap-2">
          <RoundButton label={`Text background: ${draft.background}`} onClick={() => set({ background: BG_CYCLE[(BG_CYCLE.indexOf(draft.background) + 1) % BG_CYCLE.length] })}>
            <span className={cn("rounded px-1 text-sm font-bold", draft.background === "solid" && "bg-white text-black", draft.background === "translucent" && "bg-white/40")}>A</span>
          </RoundButton>
          <RoundButton label={`Align ${draft.align}`} onClick={() => set({ align: ALIGN_CYCLE[(ALIGN_CYCLE.indexOf(draft.align) + 1) % ALIGN_CYCLE.length] })}>
            <AlignIcon className="size-5" />
          </RoundButton>
        </div>
        <button type="button" onClick={() => (draft.text.trim() ? onDone({ ...draft, text: draft.text.replace(/\s+$/, "") }) : onCancel())} data-testid="text-overlay-done"
          className="flex h-10 items-center gap-1.5 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground shadow focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white">
          <Check className="size-4" />Done
        </button>
      </div>

      <div className="flex min-h-0 flex-1 items-center gap-2 px-2">
        <input type="range" min={0.04} max={0.2} step={0.005} value={draft.size} aria-label="Text size" onChange={(e) => set({ size: Number(e.target.value) })}
          className="h-40 w-6 shrink-0 cursor-pointer accent-primary [direction:rtl] [writing-mode:vertical-lr]" />
        <div className={cn("flex min-w-0 flex-1", draft.align === "left" ? "justify-start" : draft.align === "right" ? "justify-end" : "justify-center")}>
          <textarea ref={area} value={draft.text} onChange={(e) => set({ text: e.target.value })} rows={lines} maxLength={500} aria-label="Text" data-testid="text-overlay-input" placeholder="Type something"
            className="w-full resize-none rounded-xl bg-transparent px-2 leading-tight placeholder:text-white/50 focus:outline-none"
            style={{ ...fontStyle(draft.font), color: ink, textAlign: draft.align, fontSize: `clamp(18px, ${draft.size * 100 * 0.9}vw, ${draft.size * 520}px)`, backgroundColor: draft.background === "solid" ? draft.color : draft.background === "translucent" ? "rgba(0,0,0,0.55)" : "transparent", textShadow: draft.background === "none" ? "0 1px 4px rgba(0,0,0,.35)" : undefined }} />
        </div>
      </div>

      <div className="space-y-2 px-3 pb-[max(env(safe-area-inset-bottom),0.75rem)]">
        <div role="radiogroup" aria-label="Font" className="flex gap-2 overflow-x-auto [scrollbar-width:none]">
          {EDITOR_FONTS.map((f) => (
            <button key={f.id} type="button" role="radio" aria-checked={draft.font === f.id} onClick={() => set({ font: f.id })}
              className={cn("h-9 shrink-0 rounded-full border border-white/30 px-4 text-sm text-white focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary", draft.font === f.id && "border-white bg-white text-black")}
              style={{ ...fontStyle(f.id), fontSize: 15 }}>
              {f.label}
            </button>
          ))}
        </div>
        <ColorRow label="Text colour" colors={OVERLAY_COLORS} value={draft.color} onChange={(color) => set({ color })} />
      </div>
    </div>
  );
}
