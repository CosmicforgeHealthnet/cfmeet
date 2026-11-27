"use client";

import type { CSSProperties, ReactNode } from "react";
import { cn } from "@/lib/utils";
import { fontStyle } from "@/lib/media-editor/fonts";
import { backgroundCss, normalizeTextStatus, textStatusFontSize, type TextStatus, type TextStatusInput } from "@/lib/media-editor/text-status";
import { EDITOR_FONT_CLASSES } from "./fonts";

export type { TextStatusInput };

/** Text style of a status: sizes are % of the status width (cqw), so any size looks the same. */
export function textStatusTextStyle(s: TextStatus): CSSProperties {
  return {
    ...fontStyle(s.font),
    color: s.textColor,
    textAlign: s.textAlign,
    fontSize: `${textStatusFontSize(s.text, s.textSize)}cqw`,
    lineHeight: 1.25,
  };
}

/**
 * A text status as everyone sees it (viewer, previews, the editor's backdrop). Fills its
 * parent; give the parent the status aspect (e.g. full screen or aspect-[9/16]).
 */
export default function TextStatusView({ status, className, children }: { status: TextStatusInput; className?: string; children?: ReactNode }) {
  const s = normalizeTextStatus(status);
  return (
    <div className={cn("@container relative flex size-full items-center justify-center overflow-hidden", className)} style={{ background: backgroundCss(s) }} data-testid="text-status-view">
      <span aria-hidden className={cn("pointer-events-none absolute size-0 overflow-hidden", EDITOR_FONT_CLASSES)}>.</span>
      {children ?? (
        <p className="w-full whitespace-pre-wrap break-words px-[8cqw]" style={textStatusTextStyle(s)}>{s.text}</p>
      )}
    </div>
  );
}
