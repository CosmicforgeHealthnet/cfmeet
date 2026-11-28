// Text statuses between the editor (TextStatus) and the status API. Today's API stores text,
// one #RRGGBB background and a font 0-4; the richer fields (text colour, alignment, size step,
// gradient) are carried by TextStatusExtras and sent once CreateStatus / StatusPost have them.
import type { CreateStatus, StatusPost } from "@/services/v2/social.service";
import { clampFont, representativeColor, type Gradient, type TextStatus, type TextStatusInput } from "@/lib/media-editor/text-status";

export type CreateTextStatus = Extract<CreateStatus, { kind: "TEXT" }>;

/** Fields the status API is gaining; not sent yet. */
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

/** The richer fields, for when the API takes them: `{ ...textStatusToCreate(s), ...textStatusExtras(s) }`. */
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
