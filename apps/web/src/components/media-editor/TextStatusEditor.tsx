"use client";

import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { AlignCenter, AlignLeft, AlignRight, Loader2, Palette, Send, Smile, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { EDITOR_FONTS, fontStyle, TEXT_STATUS_FONT_COUNT } from "@/lib/media-editor/fonts";
import {
  defaultTextStatus, GRADIENT_BACKGROUNDS, gradientCss, normalizeTextStatus, SOLID_BACKGROUNDS, TEXT_COLORS, TEXT_SIZE_STEPS, TEXT_STATUS_MAX_LENGTH,
  type TextStatus, type TextStatusAlign,
} from "@/lib/media-editor/text-status";
import EmojiPicker from "./EmojiPicker";
import TextStatusView, { textStatusTextStyle, type TextStatusInput } from "./TextStatusView";
import { RoundButton } from "./ui";

export interface TextStatusEditorProps {
  initial?: TextStatusInput;
  /** Called with the finished status (text trimmed); a returned promise keeps Done busy. */
  onDone: (status: TextStatus) => void | Promise<void>;
  onCancel: () => void;
  toolbarExtra?: ReactNode;
  /** Shown left of the Done button (e.g. mode tabs). */
  footer?: ReactNode;
  doneLabel?: string;
  doneIcon?: ReactNode;
  maxLength?: number;
}

type Panel = "none" | "background" | "color" | "emoji";
const ALIGNS: TextStatusAlign[] = ["center", "left", "right"];

/**
 * Full-screen text status editor: solid or gradient backgrounds, five fonts, alignment, five
 * size steps, text colour and emoji. Renders through TextStatusView, so the result looks the
 * same in the viewer.
 */
export default function TextStatusEditor({ initial, onDone, onCancel, toolbarExtra, footer, doneLabel = "Post status", doneIcon, maxLength = TEXT_STATUS_MAX_LENGTH }: TextStatusEditorProps) {
  const [status, setStatus] = useState<TextStatus>(() => (initial ? normalizeTextStatus(initial) : defaultTextStatus()));
  const [panel, setPanel] = useState<Panel>("none");
  const [busy, setBusy] = useState(false);
  const area = useRef<HTMLTextAreaElement>(null);
  const caret = useRef<number | null>(null);

  const set = (patch: Partial<TextStatus>) => setStatus((s) => ({ ...s, ...patch }));

  // Grow the text box with its content (the text stays vertically centred).
  useLayoutEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = "0px";
    el.style.height = `${el.scrollHeight}px`;
    if (caret.current !== null) { el.setSelectionRange(caret.current, caret.current); caret.current = null; }
  }, [status]);

  useEffect(() => { area.current?.focus(); }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      if (panel !== "none") setPanel("none"); else onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [panel, onCancel]);

  const insert = (emoji: string) => {
    const el = area.current;
    const start = el?.selectionStart ?? status.text.length;
    const end = el?.selectionEnd ?? start;
    const next = (status.text.slice(0, start) + emoji + status.text.slice(end)).slice(0, maxLength);
    caret.current = Math.min(next.length, start + emoji.length);
    set({ text: next });
  };

  const done = async () => {
    const text = status.text.trim();
    if (!text || busy) return;
    setBusy(true);
    const { background, backgroundGradient, ...rest } = status;
    try { await onDone({ ...rest, text, ...(backgroundGradient ? { backgroundGradient } : { background }) }); } finally { setBusy(false); }
  };

  const AlignIcon = status.textAlign === "left" ? AlignLeft : status.textAlign === "right" ? AlignRight : AlignCenter;
  const fonts = EDITOR_FONTS.slice(0, TEXT_STATUS_FONT_COUNT);

  return (
    <div className="fixed inset-0 z-[80] flex flex-col bg-black text-white" role="dialog" aria-modal="true" aria-label="Text status" data-testid="text-status-editor">
      <div className="relative min-h-0 flex-1">
        <TextStatusView status={status} className="absolute inset-0">
          <textarea ref={area} value={status.text} maxLength={maxLength} rows={1} aria-label="Status text" placeholder="Type a status" data-testid="status-text"
            onChange={(e) => set({ text: e.target.value })}
            className="block max-h-full w-full resize-none overflow-y-auto bg-transparent px-[8cqw] placeholder:text-current placeholder:opacity-60 focus:outline-none"
            style={textStatusTextStyle(status)} />
        </TextStatusView>

        <div className="absolute inset-x-0 top-0 z-10 flex items-center justify-between gap-2 px-3 pt-[max(env(safe-area-inset-top),0.75rem)]">
          <RoundButton label="Cancel" onClick={onCancel}><X className="size-6" /></RoundButton>
          <div className="flex items-center gap-1.5 sm:gap-2">
            <RoundButton label="Emoji" active={panel === "emoji"} onClick={() => setPanel(panel === "emoji" ? "none" : "emoji")}><Smile className="size-5" /></RoundButton>
            <RoundButton label={`Font: ${EDITOR_FONTS[status.font].label}`} onClick={() => set({ font: (status.font + 1) % TEXT_STATUS_FONT_COUNT })} data-testid="status-font">
              <span className="text-base" style={{ ...fontStyle(status.font), textTransform: "none" }}>Aa</span>
            </RoundButton>
            <RoundButton label={`Text size ${status.textSize + 1} of ${TEXT_SIZE_STEPS}`} onClick={() => set({ textSize: (status.textSize + 1) % TEXT_SIZE_STEPS })} data-testid="status-size">
              <span className="font-semibold leading-none" style={{ fontSize: 11 + status.textSize * 2.5 }}>T</span>
            </RoundButton>
            <RoundButton label={`Align ${status.textAlign}`} onClick={() => set({ textAlign: ALIGNS[(ALIGNS.indexOf(status.textAlign) + 1) % ALIGNS.length] })}><AlignIcon className="size-5" /></RoundButton>
            <RoundButton label="Text colour" active={panel === "color"} onClick={() => setPanel(panel === "color" ? "none" : "color")}>
              <span className="size-5 rounded-full border-2 border-white" style={{ backgroundColor: status.textColor }} />
            </RoundButton>
            <RoundButton label="Background" active={panel === "background"} onClick={() => setPanel(panel === "background" ? "none" : "background")}><Palette className="size-5" /></RoundButton>
            {toolbarExtra}
          </div>
        </div>

        {panel === "emoji" ? <div className="absolute inset-x-0 bottom-0 z-20"><EmojiPicker title="Emoji" onPick={insert} onClose={() => setPanel("none")} /></div> : null}
      </div>

      <div className="space-y-3 bg-black/90 px-3 pt-3 pb-[max(env(safe-area-inset-bottom),0.75rem)]">
        {panel === "background" ? (
          <div role="radiogroup" aria-label="Background" className="flex gap-2 overflow-x-auto pb-1 [scrollbar-width:none]" data-testid="status-backgrounds">
            {SOLID_BACKGROUNDS.map((c) => (
              <button key={c} type="button" role="radio" aria-checked={!status.backgroundGradient && status.background === c} aria-label={`Colour ${c}`}
                onClick={() => setStatus((s) => ({ ...s, background: c, backgroundGradient: undefined, textColor: c === "#f5f5f4" && s.textColor === "#ffffff" ? "#000000" : s.textColor }))}
                className={cn("size-9 shrink-0 rounded-full border-2 border-white/70 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary", !status.backgroundGradient && status.background === c && "ring-2 ring-white ring-offset-2 ring-offset-black")}
                style={{ backgroundColor: c }} />
            ))}
            {GRADIENT_BACKGROUNDS.map((g, i) => {
              const on = status.backgroundGradient?.from === g.from && status.backgroundGradient?.to === g.to && status.backgroundGradient?.angle === g.angle;
              return (
                <button key={i} type="button" role="radio" aria-checked={on} aria-label={`Gradient ${g.from} to ${g.to}`} onClick={() => setStatus((s) => ({ ...s, backgroundGradient: g, background: undefined }))}
                  className={cn("size-9 shrink-0 rounded-full border-2 border-white/70 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary", on && "ring-2 ring-white ring-offset-2 ring-offset-black")}
                  style={{ background: gradientCss(g) }} />
              );
            })}
          </div>
        ) : panel === "color" ? (
          <div role="radiogroup" aria-label="Text colour" className="flex gap-2 overflow-x-auto pb-1 [scrollbar-width:none]">
            {TEXT_COLORS.map((c) => (
              <button key={c} type="button" role="radio" aria-checked={status.textColor === c} aria-label={`Colour ${c}`} onClick={() => set({ textColor: c })}
                className={cn("size-9 shrink-0 rounded-full border-2 border-white/70 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary", status.textColor === c && "ring-2 ring-white ring-offset-2 ring-offset-black")}
                style={{ backgroundColor: c }} />
            ))}
          </div>
        ) : (
          <div role="radiogroup" aria-label="Font" className="flex gap-2 overflow-x-auto pb-1 [scrollbar-width:none]">
            {fonts.map((f) => (
              <button key={f.id} type="button" role="radio" aria-checked={status.font === f.id} onClick={() => set({ font: f.id })}
                className={cn("h-9 shrink-0 rounded-full border border-white/30 px-4 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary", status.font === f.id && "border-white bg-white text-black")}
                style={{ ...fontStyle(f.id), fontSize: 15 }}>{f.label}</button>
            ))}
          </div>
        )}
        <div className="flex items-center gap-2">
          <div className="min-w-0 flex-1">{footer}</div>
          <button type="button" aria-label={doneLabel} disabled={!status.text.trim() || busy} onClick={() => void done()} data-testid="status-send"
            className="flex size-14 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-lg focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white focus-visible:ring-offset-2 focus-visible:ring-offset-black disabled:opacity-50">
            {busy ? <Loader2 className="size-6 animate-spin" /> : doneIcon ?? <Send className="size-6" />}
          </button>
        </div>
      </div>
    </div>
  );
}
