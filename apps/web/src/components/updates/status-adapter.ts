// Text statuses between the editor (TextStatus) and the status API: text, a #RRGGBB background
// and a font 0-4, plus the richer look (text colour, alignment, size step, gradient).
import type { CreateStatus, StatusPost } from "@/services/v2/social.service";
import { clampFont, representativeColor, type Gradient, type TextStatus, type TextStatusInput } from "@/lib/media-editor/text-status";

export type CreateTextStatus = Extract<CreateStatus, { kind: "TEXT" }>;

/** A text status's richer look. */
export interface TextStatusExtras {
  textColor: string;
  textAlign: TextStatus["textAlign"];
  textSize: number;
  backgroundGradient?: Gradient;
}

/** The request body the current API accepts (a gradient falls back to its first colour). */
export function textStatusToCreate(s: TextStatus): CreateTextStatus {
  return { kind: "TEXT", text: s.text, background: representativeColor(s), font: clampFont(s.font) };
}

/**
 * The queue item for a text status with its full look (contract 1.25+: textColor, textAlign,
 * textSize 0-4, backgroundGradient; `background` stays the representative colour for cards).
 */
export function textStatusItem(s: TextStatus): { kind: "TEXT"; text: string; background: string; font: number } & TextStatusExtras {
  return { kind: "TEXT", text: s.text, background: representativeColor(s), font: clampFont(s.font), ...textStatusExtras(s), textSize: Math.max(0, Math.min(4, Math.round(s.textSize))) };
}

/** The richer fields: `{ ...textStatusToCreate(s), ...textStatusExtras(s) }`. */
export function textStatusExtras(s: TextStatus): TextStatusExtras {
  return { textColor: s.textColor, textAlign: s.textAlign, textSize: s.textSize, ...(s.backgroundGradient ? { backgroundGradient: s.backgroundGradient } : {}) };
}

/** A posted text status for TextStatusView (richer fields used when the API returns them). */
export function textStatusOfPost(post: StatusPost): TextStatusInput {
  const extra = post as StatusPost & Partial<TextStatusExtras>;
  return {
    text: post.text ?? "",
    background: post.background,
    font: post.font,
    textColor: extra.textColor,
    textAlign: extra.textAlign,
    textSize: extra.textSize,
    backgroundGradient: extra.backgroundGradient,
  };
}
