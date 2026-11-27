// Overlays for photos, videos and collages: text, emoji stickers and freehand strokes.
// Framework-free. Positions are normalised to the media (x: 0-1 of the width, y: 0-1 of the
// height) and sizes are fractions of the media width, so one state renders identically at
// any resolution (the on-screen layer, a full-size photo export, every video frame).
//
// Text and stickers are laid out in a fixed reference space (REF units across the media
// width) and then scaled, so line wrapping never depends on the output resolution.

import { displayText, EMOJI_FONT, fontCss } from "./fonts";

/** Reference width of the layout space. */
export const REF = 1000;

export type TextBackground = "none" | "solid" | "translucent";
export type TextAlign = "left" | "center" | "right";

interface Placed {
  id: string;
  /** Centre, 0-1 of the media width / height. */
  x: number;
  y: number;
  /** Multiplier on the item's base size. */
  scale: number;
  /** Radians, clockwise. */
  rotation: number;
}

export interface TextOverlay extends Placed {
  type: "text";
  text: string;
  /** Index into EDITOR_FONTS. */
  font: number;
  color: string;
  background: TextBackground;
  align: TextAlign;
  /** Font size as a fraction of the media width (before `scale`). */
  size: number;
}

export interface StickerOverlay extends Placed {
  type: "sticker";
  emoji: string;
  /** Glyph size as a fraction of the media width (before `scale`). */
  size: number;
}

export interface StrokeOverlay {
  id: string;
  type: "stroke";
  color: string;
  /** Brush diameter as a fraction of the media width. */
  width: number;
  /** Flattened normalised points: x0, y0, x1, y1, ... */
  points: number[];
}

export type PlacedOverlay = TextOverlay | StickerOverlay;
export type Overlay = PlacedOverlay | StrokeOverlay;

export const isPlaced = (o: Overlay): o is PlacedOverlay => o.type !== "stroke";

/** Palette offered for text and drawing. */
export const OVERLAY_COLORS = ["#ffffff", "#000000", "#ef4444", "#f97316", "#facc15", "#22c55e", "#06b6d4", "#3b82f6", "#8b5cf6", "#ec4899", "#a16207", "#94a3b8"];
/** Brush diameters (fractions of the media width). */
export const BRUSH_SIZES = [0.008, 0.016, 0.028, 0.045];
export const TEXT_SIZE_DEFAULT = 0.085;
export const STICKER_SIZE_DEFAULT = 0.22;
export const MIN_SCALE = 0.2;
export const MAX_SCALE = 8;
/** Line height as a multiple of the font size. */
export const LINE_HEIGHT = 1.25;
/** Text wraps at this width (REF units, before scale). */
export const WRAP_WIDTH = 860;
/** Padding of a text background (fractions of the font size). */
const PAD_X = 0.35;
const PAD_Y = 0.12;

let counter = 0;
/** A fresh overlay id (event handlers only). */
export function overlayId(): string {
  const c = globalThis.crypto as Crypto | undefined;
  if (c?.randomUUID) return c.randomUUID();
  counter += 1;
  return `ov-${Date.now().toString(36)}-${counter}`;
}

export function createText(partial: Partial<TextOverlay> & { text: string }): TextOverlay {
  return { id: overlayId(), type: "text", x: 0.5, y: 0.45, scale: 1, rotation: 0, font: 0, color: "#ffffff", background: "none", align: "center", size: TEXT_SIZE_DEFAULT, ...partial };
}

export function createSticker(emoji: string, partial: Partial<StickerOverlay> = {}): StickerOverlay {
  return { id: overlayId(), type: "sticker", emoji, x: 0.5, y: 0.45, scale: 1, rotation: 0, size: STICKER_SIZE_DEFAULT, ...partial };
}

// ---------------------------------------------------------------- measuring & layout

/** Width of `text` drawn with canvas font `font` (CSS shorthand). */
export type TextMeasurer = (text: string, font: string) => number;

let measureCtx: CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D | null = null;

/** A canvas-backed measurer (approximate without a DOM). */
export function canvasMeasurer(): TextMeasurer {
  return (text, font) => {
    if (!measureCtx) {
      if (typeof OffscreenCanvas !== "undefined") measureCtx = new OffscreenCanvas(8, 8).getContext("2d");
      else if (typeof document !== "undefined") measureCtx = document.createElement("canvas").getContext("2d");
    }
    if (!measureCtx) return text.length * (parseFloat(/(\d+(?:\.\d+)?)px/.exec(font)?.[1] ?? "16") * 0.55);
    measureCtx.font = font;
    return measureCtx.measureText(text).width;
  };
}

export interface TextLayout {
  lines: { text: string; width: number }[];
  fontPx: number;
  lineHeight: number;
  /** Box size in REF units (before scale), backgrounds included. */
  width: number;
  height: number;
}

/** Break `text` into lines no wider than `max` (explicit newlines kept, long words split). */
export function wrapText(text: string, max: number, measure: (s: string) => number): string[] {
  const out: string[] = [];
  for (const para of text.split("\n")) {
    const words = para.split(/(\s+)/).filter((w) => w.length);
    let line = "";
    for (const word of words) {
      const candidate = line + word;
      if (!line || measure(candidate.trimEnd()) <= max) {
        line = candidate;
        // A single word wider than the line is split by characters.
        while (measure(line.trimEnd()) > max && line.trim().length > 1) {
          const chars = [...line];
          let cut = chars.length - 1;
          while (cut > 1 && measure(chars.slice(0, cut).join("")) > max) cut--;
          out.push(chars.slice(0, cut).join(""));
          line = chars.slice(cut).join("");
        }
        continue;
      }
      out.push(line.trimEnd());
      line = /^\s+$/.test(word) ? "" : word;
    }
    out.push(line.trimEnd());
  }
  return out;
}

export function layoutText(o: Pick<TextOverlay, "text" | "font" | "size" | "background">, measure: TextMeasurer = canvasMeasurer()): TextLayout {
  const fontPx = o.size * REF;
  const font = fontCss(o.font, fontPx);
  const shown = displayText(o.font, o.text || " ");
  const m = (s: string) => measure(s, font);
  const lines = wrapText(shown, WRAP_WIDTH, m).map((text) => ({ text, width: m(text) }));
  const lineHeight = fontPx * LINE_HEIGHT;
  const padX = o.background === "none" ? fontPx * 0.1 : fontPx * PAD_X;
  const padY = o.background === "none" ? 0 : fontPx * PAD_Y;
  const width = Math.max(fontPx * 0.5, ...lines.map((l) => l.width)) + padX * 2;
  const height = lines.length * lineHeight + padY * 2;
  return { lines, fontPx, lineHeight, width, height };
}

/** Unscaled box of a placed overlay in REF units. */
export function overlaySize(o: PlacedOverlay, measure?: TextMeasurer): { width: number; height: number } {
  if (o.type === "sticker") {
    const s = o.size * REF * 1.15;
    return { width: s, height: s };
  }
  const l = layoutText(o, measure);
  return { width: l.width, height: l.height };
}

// ---------------------------------------------------------------- geometry & hit-testing

export interface Box {
  /** Centre in pixels. */
  cx: number;
  cy: number;
  /** Scaled size in pixels. */
  width: number;
  height: number;
  rotation: number;
}

/** The on-media box of a placed overlay at `width` x `height` pixels. */
export function overlayBox(o: PlacedOverlay, width: number, height: number, measure?: TextMeasurer): Box {
  const size = overlaySize(o, measure);
  const k = (width / REF) * o.scale;
  return { cx: o.x * width, cy: o.y * height, width: size.width * k, height: size.height * k, rotation: o.rotation };
}

/** Point (px) in the box's own frame (origin at the centre, unrotated). */
export function toLocal(box: Box, px: number, py: number): { x: number; y: number } {
  const dx = px - box.cx;
  const dy = py - box.cy;
  const c = Math.cos(-box.rotation);
  const s = Math.sin(-box.rotation);
  return { x: dx * c - dy * s, y: dx * s + dy * c };
}

export function pointInBox(box: Box, px: number, py: number, slop = 0): boolean {
  const p = toLocal(box, px, py);
  return Math.abs(p.x) <= box.width / 2 + slop && Math.abs(p.y) <= box.height / 2 + slop;
}

/** Corners of a box (px), clockwise from top-left. */
export function boxCorners(box: Box): { x: number; y: number }[] {
  const c = Math.cos(box.rotation);
  const s = Math.sin(box.rotation);
  return [[-1, -1], [1, -1], [1, 1], [-1, 1]].map(([sx, sy]) => {
    const lx = (sx * box.width) / 2;
    const ly = (sy * box.height) / 2;
    return { x: box.cx + lx * c - ly * s, y: box.cy + lx * s + ly * c };
  });
}

/** Topmost placed overlay under (px, py), or null. Strokes are not selectable. */
export function hitTest(overlays: readonly Overlay[], px: number, py: number, width: number, height: number, measure?: TextMeasurer, slop = 8): PlacedOverlay | null {
  for (let i = overlays.length - 1; i >= 0; i--) {
    const o = overlays[i];
    if (isPlaced(o) && pointInBox(overlayBox(o, width, height, measure), px, py, slop)) return o;
  }
  return null;
}

/** Distance from P to segment AB. */
export function distanceToSegment(px: number, py: number, ax: number, ay: number, bx: number, by: number): number {
  const dx = bx - ax;
  const dy = by - ay;
  const len2 = dx * dx + dy * dy;
  const t = len2 === 0 ? 0 : Math.max(0, Math.min(1, ((px - ax) * dx + (py - ay) * dy) / len2));
  return Math.hypot(px - (ax + t * dx), py - (ay + t * dy));
}

/** Whether a stroke passes within `radius` px of (px, py) at `width` x `height`. */
export function strokeHit(s: StrokeOverlay, px: number, py: number, width: number, height: number, radius: number): boolean {
  const reach = radius + (s.width * width) / 2;
  const p = s.points;
  if (p.length < 2) return false;
  if (p.length < 4) return Math.hypot(px - p[0] * width, py - p[1] * height) <= reach;
  for (let i = 0; i + 3 < p.length; i += 2) {
    if (distanceToSegment(px, py, p[i] * width, p[i + 1] * height, p[i + 2] * width, p[i + 3] * height) <= reach) return true;
  }
  return false;
}

/** Overlays with every stroke touched by the eraser at (px, py) removed. */
export function eraseAt(overlays: readonly Overlay[], px: number, py: number, width: number, height: number, radius: number): Overlay[] {
  const kept = overlays.filter((o) => o.type !== "stroke" || !strokeHit(o, px, py, width, height, radius));
  return kept.length === overlays.length ? (overlays as Overlay[]) : kept;
}

// ---------------------------------------------------------------- transforms

export const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

export function updateOverlay<T extends Overlay>(overlays: readonly Overlay[], id: string, patch: (o: T) => T): Overlay[] {
  return overlays.map((o) => (o.id === id ? patch(o as T) : o));
}

export function removeOverlay(overlays: readonly Overlay[], id: string): Overlay[] {
  return overlays.filter((o) => o.id !== id);
}

export function bringToFront(overlays: readonly Overlay[], id: string): Overlay[] {
  const item = overlays.find((o) => o.id === id);
  if (!item || overlays[overlays.length - 1] === item) return overlays as Overlay[];
  return [...overlays.filter((o) => o.id !== id), item];
}

/** Move by a pixel delta (centre kept inside the media). */
export function moveBy<T extends PlacedOverlay>(o: T, dx: number, dy: number, width: number, height: number): T {
  return { ...o, x: clamp(o.x + dx / width, 0, 1), y: clamp(o.y + dy / height, 0, 1) };
}

export function scaleTo<T extends PlacedOverlay>(o: T, scale: number): T {
  return { ...o, scale: clamp(scale, MIN_SCALE, MAX_SCALE) };
}

/** Normalise an angle to (-PI, PI]. */
export function normalizeAngle(a: number): number {
  let r = a % (2 * Math.PI);
  if (r <= -Math.PI) r += 2 * Math.PI;
  if (r > Math.PI) r -= 2 * Math.PI;
  return r;
}

/** Snap near-right angles (within `tolerance` radians) to the right angle. */
export function snapAngle(a: number, tolerance = 0.06): number {
  const q = Math.PI / 2;
  const nearest = Math.round(a / q) * q;
  return Math.abs(a - nearest) <= tolerance ? normalizeAngle(nearest) : normalizeAngle(a);
}

export interface Pt { x: number; y: number }

/**
 * Two-finger gesture: from the start pair (a0, b0) to the current pair (a1, b1), the item's
 * scale follows the finger distance, its rotation the finger angle and its centre the midpoint.
 */
export function pinchTransform<T extends PlacedOverlay>(start: T, a0: Pt, b0: Pt, a1: Pt, b1: Pt, width: number, height: number): T {
  const d0 = Math.hypot(b0.x - a0.x, b0.y - a0.y) || 1;
  const d1 = Math.hypot(b1.x - a1.x, b1.y - a1.y);
  const r0 = Math.atan2(b0.y - a0.y, b0.x - a0.x);
  const r1 = Math.atan2(b1.y - a1.y, b1.x - a1.x);
  const m0 = { x: (a0.x + b0.x) / 2, y: (a0.y + b0.y) / 2 };
  const m1 = { x: (a1.x + b1.x) / 2, y: (a1.y + b1.y) / 2 };
  const moved = moveBy(start, m1.x - m0.x, m1.y - m0.y, width, height);
  return { ...scaleTo(moved, start.scale * (d1 / d0)), rotation: snapAngle(start.rotation + r1 - r0) };
}

/**
 * Corner-handle gesture: dragging the handle from `from` to `to` (px) scales the item by the
 * change of distance to its centre and rotates it by the change of angle.
 */
export function handleTransform<T extends PlacedOverlay>(start: T, from: Pt, to: Pt, width: number, height: number): T {
  const c = { x: start.x * width, y: start.y * height };
  const d0 = Math.hypot(from.x - c.x, from.y - c.y) || 1;
  const d1 = Math.hypot(to.x - c.x, to.y - c.y);
  const r0 = Math.atan2(from.y - c.y, from.x - c.x);
  const r1 = Math.atan2(to.y - c.y, to.x - c.x);
  return { ...scaleTo(start, start.scale * (d1 / d0)), rotation: snapAngle(start.rotation + r1 - r0) };
}

/** Append a normalised point to a stroke unless it is within `minPx` of the last one. */
export function extendStroke(s: StrokeOverlay, px: number, py: number, width: number, height: number, minPx = 2): StrokeOverlay {
  const p = s.points;
  if (p.length >= 2) {
    const lx = p[p.length - 2] * width;
    const ly = p[p.length - 1] * height;
    if (Math.hypot(px - lx, py - ly) < minPx) return s;
  }
  return { ...s, points: [...p, clamp(px / width, 0, 1), clamp(py / height, 0, 1)] };
}

// ---------------------------------------------------------------- drawing

type Ctx = CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;

/** Readable ink on a solid background of colour `c`. */
export function contrastInk(c: string): string {
  const m = /^#?([0-9a-f]{6})$/i.exec(c.trim());
  if (!m) return "#000000";
  const n = parseInt(m[1], 16);
  const lum = (0.299 * ((n >> 16) & 255) + 0.587 * ((n >> 8) & 255) + 0.114 * (n & 255)) / 255;
  return lum > 0.6 ? "#000000" : "#ffffff";
}

function roundRect(ctx: Ctx, x: number, y: number, w: number, h: number, r: number) {
  const rr = Math.min(r, w / 2, h / 2);
  ctx.beginPath();
  ctx.moveTo(x + rr, y);
  ctx.arcTo(x + w, y, x + w, y + h, rr);
  ctx.arcTo(x + w, y + h, x, y + h, rr);
  ctx.arcTo(x, y + h, x, y, rr);
  ctx.arcTo(x, y, x + w, y, rr);
  ctx.closePath();
}

function drawText(ctx: Ctx, o: TextOverlay, k: number, measure: TextMeasurer) {
  const l = layoutText(o, measure);
  const { fontPx, lineHeight } = l;
  const padX = o.background === "none" ? fontPx * 0.1 : fontPx * PAD_X;
  const padY = o.background === "none" ? 0 : fontPx * PAD_Y;
  const top = -l.height / 2 + padY;
  const left = -l.width / 2 + padX;
  const inner = l.width - padX * 2;
  const xOf = (w: number) => (o.align === "left" ? left : o.align === "right" ? left + inner - w : -w / 2);
  if (o.background !== "none") {
    ctx.fillStyle = o.background === "solid" ? o.color : "rgba(0,0,0,0.55)";
    l.lines.forEach((line, i) => {
      if (!line.text) return;
      const x = xOf(line.width) - fontPx * PAD_X;
      const y = top + i * lineHeight - padY;
      roundRect(ctx, x, y, line.width + fontPx * PAD_X * 2, lineHeight + padY * 2, fontPx * 0.28);
      ctx.fill();
    });
  }
  ctx.font = fontCss(o.font, fontPx);
  ctx.textBaseline = "middle";
  ctx.textAlign = "left";
  ctx.fillStyle = o.background === "solid" ? contrastInk(o.color) : o.color;
  if (o.background === "none") {
    // Canvas shadows ignore the transform, so they are given in device pixels.
    ctx.shadowColor = "rgba(0,0,0,0.35)";
    ctx.shadowBlur = fontPx * 0.12 * k;
    ctx.shadowOffsetY = fontPx * 0.03 * k;
  }
  l.lines.forEach((line, i) => ctx.fillText(line.text, xOf(line.width), top + i * lineHeight + lineHeight / 2));
  ctx.shadowColor = "transparent";
  ctx.shadowBlur = 0;
  ctx.shadowOffsetY = 0;
}

function drawStroke(ctx: Ctx, s: StrokeOverlay, width: number, height: number) {
  const p = s.points;
  if (p.length < 2) return;
  ctx.strokeStyle = s.color;
  ctx.fillStyle = s.color;
  ctx.lineWidth = s.width * width;
  ctx.lineCap = "round";
  ctx.lineJoin = "round";
  if (p.length < 4) {
    ctx.beginPath();
    ctx.arc(p[0] * width, p[1] * height, (s.width * width) / 2, 0, Math.PI * 2);
    ctx.fill();
    return;
  }
  ctx.beginPath();
  ctx.moveTo(p[0] * width, p[1] * height);
  // Quadratic curves through the midpoints smooth the polyline.
  for (let i = 2; i + 3 < p.length; i += 2) {
    const mx = ((p[i] + p[i + 2]) / 2) * width;
    const my = ((p[i + 1] + p[i + 3]) / 2) * height;
    ctx.quadraticCurveTo(p[i] * width, p[i + 1] * height, mx, my);
  }
  ctx.lineTo(p[p.length - 2] * width, p[p.length - 1] * height);
  ctx.stroke();
}

/**
 * Draw `overlays` onto `ctx` covering `width` x `height` pixels (from the origin). Pure and
 * synchronous so it can run for every video frame; await `loadEditorFonts()` once first.
 */
export function drawOverlays(ctx: Ctx, overlays: readonly Overlay[], width: number, height: number, measure: TextMeasurer = canvasMeasurer()): void {
  for (const o of overlays) {
    ctx.save();
    if (o.type === "stroke") {
      drawStroke(ctx, o, width, height);
    } else {
      const k = (width / REF) * o.scale;
      ctx.translate(o.x * width, o.y * height);
      ctx.rotate(o.rotation);
      ctx.scale(k, k);
      if (o.type === "text") {
        drawText(ctx, o, k, measure);
      } else {
        ctx.font = `${o.size * REF}px ${EMOJI_FONT}`;
        ctx.textAlign = "center";
        ctx.textBaseline = "middle";
        ctx.fillStyle = "#000";
        ctx.fillText(o.emoji, 0, o.size * REF * 0.06);
      }
    }
    ctx.restore();
  }
}
