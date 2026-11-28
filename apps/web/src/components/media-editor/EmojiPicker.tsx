"use client";

import { useEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import { cn } from "@/lib/utils";
import { EMOJI_CATEGORIES } from "@/lib/media-editor/emoji";
import { EMOJI_FONT } from "@/lib/media-editor/fonts";

/**
 * Emoji grid with category tabs (system glyphs). Rendered as a bottom sheet when `onClose` is
 * given; Escape closes it.
 */
export default function EmojiPicker({ onPick, onClose, title = "Stickers", className }: { onPick: (emoji: string) => void; onClose?: () => void; title?: string; className?: string }) {
  const [category, setCategory] = useState(EMOJI_CATEGORIES[0].id);
  const grid = useRef<HTMLDivElement>(null);
  const sections = useRef(new Map<string, HTMLElement>());

  useEffect(() => {
    if (!onClose) return;
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") { e.stopPropagation(); onClose(); } };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  const jump = (id: string) => {
    setCategory(id);
    const el = sections.current.get(id);
    if (el && grid.current) grid.current.scrollTo({ top: el.offsetTop - grid.current.offsetTop, behavior: "smooth" });
  };

  const onScroll = () => {
    const g = grid.current;
    if (!g) return;
    let current = EMOJI_CATEGORIES[0].id;
    for (const c of EMOJI_CATEGORIES) {
      const el = sections.current.get(c.id);
      if (el && el.offsetTop - g.offsetTop <= g.scrollTop + 8) current = c.id;
    }
    if (current !== category) setCategory(current);
  };

  return (
    <div role="dialog" aria-label={title} className={cn("flex max-h-[60vh] min-h-[45vh] flex-col rounded-t-3xl bg-neutral-900/95 text-white shadow-2xl backdrop-blur", className)} data-testid="emoji-picker">
      <div className="flex items-center justify-between px-4 pt-3">
        <span className="mx-auto h-1 w-10 rounded-full bg-white/30" aria-hidden />
        {onClose ? (
          <button type="button" aria-label="Close stickers" onClick={onClose} className="absolute right-3 flex size-9 items-center justify-center rounded-full hover:bg-white/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary"><X className="size-5" /></button>
        ) : null}
      </div>
      <div role="tablist" aria-label="Emoji categories" className="flex gap-1 overflow-x-auto px-3 py-2 [scrollbar-width:none]">
        {EMOJI_CATEGORIES.map((c) => (
          <button key={c.id} type="button" role="tab" aria-selected={category === c.id} aria-label={c.label} title={c.label} onClick={() => jump(c.id)}
            className={cn("flex size-10 shrink-0 items-center justify-center rounded-full text-xl focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary", category === c.id ? "bg-white/20" : "opacity-70 hover:bg-white/10")}
            style={{ fontFamily: EMOJI_FONT }}>
            {c.icon}
          </button>
        ))}
      </div>
      <div ref={grid} onScroll={onScroll} className="min-h-0 flex-1 overflow-y-auto px-2 pb-[max(env(safe-area-inset-bottom),0.75rem)]">
        {EMOJI_CATEGORIES.map((c) => (
          <section key={c.id} ref={(el) => { if (el) sections.current.set(c.id, el); else sections.current.delete(c.id); }} aria-label={c.label}>
            <h3 className="sticky top-0 z-10 bg-neutral-900/95 px-2 py-1.5 text-xs font-semibold uppercase tracking-wide text-white/60">{c.label}</h3>
            <div className="grid grid-cols-8 gap-0.5 sm:grid-cols-10">
              {c.emoji.map((e) => (
                <button key={e} type="button" aria-label={e} onClick={() => onPick(e)}
                  className="flex aspect-square items-center justify-center rounded-xl text-[1.7rem] leading-none hover:bg-white/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary"
                  style={{ fontFamily: EMOJI_FONT }}>
                  {e}
                </button>
              ))}
            </div>
          </section>
        ))}
      </div>
    </div>
  );
}
