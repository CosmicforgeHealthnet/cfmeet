// Text statuses: background colours and gradients, fonts, alignment, size steps and the rules
// the editor and every viewer share so a text status looks the same everywhere. Framework-free.

import { TEXT_STATUS_FONT_COUNT } from "./fonts";

export interface Gradient {
  /** #RRGGBB */
  from: string;
  /** #RRGGBB */
  to: string;
  /** CSS linear-gradient angle in degrees, 0-359. */
  angle: number;
}

export type TextStatusAlign = "left" | "center" | "right";

export interface TextStatus {
  text: string;
  /** #RRGGBB solid background (absent when a gradient is used). */
  background?: string;
  backgroundGradient?: Gradient;
  /** Font 0-4 (EDITOR_FONTS). */
  font: number;
  /** #RRGGBB */
  textColor: string;
  textAlign: TextStatusAlign;
  /** Size step 0-4. */
  textSize: number;
}

export const TEXT_STATUS_MAX_LENGTH = 700;
export const TEXT_SIZE_STEPS = 5;
export const DEFAULT_TEXT_SIZE = 2;

export const SOLID_BACKGROUNDS = ["#128C7E", "#2563eb", "#7c3aed", "#db2777", "#ea580c", "#0f766e", "#334155", "#b45309", "#be123c", "#4338ca", "#111827", "#f5f5f4"];

export const GRADIENT_BACKGROUNDS: readonly Gradient[] = [
  { from: "#6366f1", to: "#ec4899", angle: 135 },
  { from: "#0ea5e9", to: "#22c55e", angle: 135 },
  { from: "#f97316", to: "#e11d48", angle: 160 },
  { from: "#7c3aed", to: "#0ea5e9", angle: 180 },
  { from: "#111827", to: "#4b5563", angle: 180 },
  { from: "#facc15", to: "#f97316", angle: 135 },
  { from: "#14b8a6", to: "#1e3a8a", angle: 200 },
  { from: "#f472b6", to: "#fde68a", angle: 45 },
];

export const TEXT_COLORS = ["#ffffff", "#000000", "#fde68a", "#fecaca", "#bfdbfe", "#bbf7d0", "#facc15", "#f472b6"];

const HEX = /^#[0-9a-fA-F]{6}$/;

export const isHexColor = (v: unknown): v is string => typeof v === "string" && HEX.test(v);

/** Whether `g` is a well-formed gradient ({from, to} #RRGGBB, angle 0-359 integer). */
export function isValidGradient(g: unknown): g is Gradient {
  if (!g || typeof g !== "object") return false;
  const { from, to, angle } = g as Record<string, unknown>;
  return isHexColor(from) && isHexColor(to) && typeof angle === "number" && Number.isInteger(angle) && angle >= 0 && angle < 360;
}

/** A gradient made valid (angle wrapped and rounded), or null when the colours are invalid. */
export function normalizeGradient(g: unknown): Gradient | null {
  if (!g || typeof g !== "object") return null;
  const { from, to, angle } = g as Record<string, unknown>;
  if (!isHexColor(from) || !isHexColor(to)) return null;
  const a = typeof angle === "number" && Number.isFinite(angle) ? ((Math.round(angle) % 360) + 360) % 360 : 180;
  return { from: from.toLowerCase(), to: to.toLowerCase(), angle: a };
}

export function gradientCss(g: Gradient): string {
  return `linear-gradient(${g.angle}deg, ${g.from}, ${g.to})`;
}

/** CSS background for a status (gradient first, then colour, then the default). */
export function backgroundCss(s: Pick<TextStatus, "background" | "backgroundGradient">): string {
  const g = normalizeGradient(s.backgroundGradient);
  if (g) return gradientCss(g);
  return isHexColor(s.background) ? s.background : SOLID_BACKGROUNDS[0];
}

/** A single representative colour (for clients or fields that only take one). */
export function representativeColor(s: Pick<TextStatus, "background" | "backgroundGradient">): string {
  const g = normalizeGradient(s.backgroundGradient);
  if (g) return g.from;
  return isHexColor(s.background) ? s.background : SOLID_BACKGROUNDS[0];
}

/** Font size per step, in % of the status width (cqw). */
export const TEXT_SIZE_CQW = [5.5, 7, 8.75, 11, 14] as const;

/**
 * Font size (cqw) for `text` at size step `step`: long texts shrink so they fit, the same way
 * in the editor and in every viewer.
 */
export function textStatusFontSize(text: string, step: number): number {
  const base = TEXT_SIZE_CQW[clampStep(step)];
  const chars = text.length;
  const lines = text.split("\n").length;
  const weight = chars + lines * 12;
  const factor = weight > 520 ? 0.5 : weight > 360 ? 0.58 : weight > 220 ? 0.68 : weight > 120 ? 0.8 : weight > 60 ? 0.9 : 1;
  return Math.max(3.6, Math.round(base * factor * 100) / 100);
}

export const clampStep = (s: number) => (Number.isFinite(s) ? Math.max(0, Math.min(TEXT_SIZE_STEPS - 1, Math.round(s))) : DEFAULT_TEXT_SIZE);
export const clampFont = (f: number) => (Number.isFinite(f) ? Math.max(0, Math.min(TEXT_STATUS_FONT_COUNT - 1, Math.round(f))) : 0);

export function defaultTextStatus(): TextStatus {
  return { text: "", background: SOLID_BACKGROUNDS[0], font: 0, textColor: "#ffffff", textAlign: "center", textSize: DEFAULT_TEXT_SIZE };
}

/** A possibly partial status (e.g. from an API that has only some of the fields). */
export type TextStatusInput = Omit<Partial<TextStatus>, "text" | "background"> & { text?: string | null; background?: string | null };

/** A status made safe to render (unknown or missing fields defaulted). */
export function normalizeTextStatus(s: TextStatusInput): TextStatus {
  const g = normalizeGradient(s.backgroundGradient);
  const align: TextStatusAlign = s.textAlign === "left" || s.textAlign === "right" ? s.textAlign : "center";
  return {
    text: s.text ?? "",
    ...(g ? { backgroundGradient: g } : { background: isHexColor(s.background) ? s.background : SOLID_BACKGROUNDS[0] }),
    font: clampFont(s.font ?? 0),
    textColor: isHexColor(s.textColor) ? s.textColor : "#ffffff",
    textAlign: align,
    textSize: clampStep(s.textSize ?? DEFAULT_TEXT_SIZE),
  };
}
