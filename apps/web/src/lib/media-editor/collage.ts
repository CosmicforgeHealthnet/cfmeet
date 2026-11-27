// Collage layouts and maths: templates of normalised cells, borders, and the cover-fit
// pan / zoom of each photo inside its cell. Framework-free.

import { drawOverlays, type Overlay } from "./overlays";

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface CollageLayout {
  id: string;
  label: string;
  /** Cells in reading order, normalised to the canvas. */
  cells: Rect[];
}

const r = (x: number, y: number, w: number, h: number): Rect => ({ x, y, w, h });
const T = 1 / 3;

export const COLLAGE_LAYOUTS: readonly CollageLayout[] = [
  { id: "2-cols", label: "Side by side", cells: [r(0, 0, 0.5, 1), r(0.5, 0, 0.5, 1)] },
  { id: "2-rows", label: "Stacked", cells: [r(0, 0, 1, 0.5), r(0, 0.5, 1, 0.5)] },
  { id: "3-rows", label: "Three rows", cells: [r(0, 0, 1, T), r(0, T, 1, T), r(0, 2 * T, 1, T)] },
  { id: "3-top", label: "One over two", cells: [r(0, 0, 1, 0.5), r(0, 0.5, 0.5, 0.5), r(0.5, 0.5, 0.5, 0.5)] },
  { id: "3-left", label: "One beside two", cells: [r(0, 0, 0.5, 1), r(0.5, 0, 0.5, 0.5), r(0.5, 0.5, 0.5, 0.5)] },
  { id: "3-cols", label: "Three columns", cells: [r(0, 0, T, 1), r(T, 0, T, 1), r(2 * T, 0, T, 1)] },
  { id: "4-grid", label: "Grid", cells: [r(0, 0, 0.5, 0.5), r(0.5, 0, 0.5, 0.5), r(0, 0.5, 0.5, 0.5), r(0.5, 0.5, 0.5, 0.5)] },
  { id: "4-top", label: "One over three", cells: [r(0, 0, 1, 0.6), r(0, 0.6, T, 0.4), r(T, 0.6, T, 0.4), r(2 * T, 0.6, T, 0.4)] },
  { id: "4-rows", label: "Four rows", cells: [r(0, 0, 1, 0.25), r(0, 0.25, 1, 0.25), r(0, 0.5, 1, 0.25), r(0, 0.75, 1, 0.25)] },
  { id: "5-top", label: "Two over three", cells: [r(0, 0, 0.5, 0.5), r(0.5, 0, 0.5, 0.5), r(0, 0.5, T, 0.5), r(T, 0.5, T, 0.5), r(2 * T, 0.5, T, 0.5)] },
  { id: "5-feature", label: "One over four", cells: [r(0, 0, 1, 0.5), r(0, 0.5, 0.5, 0.25), r(0.5, 0.5, 0.5, 0.25), r(0, 0.75, 0.5, 0.25), r(0.5, 0.75, 0.5, 0.25)] },
  { id: "6-grid", label: "Grid of six", cells: [0, 1, 2].flatMap((row) => [r(0, row * T, 0.5, T), r(0.5, row * T, 0.5, T)]) },
  { id: "6-cols", label: "Three by two", cells: [0, 1].flatMap((row) => [r(0, row * 0.5, T, 0.5), r(T, row * 0.5, T, 0.5), r(2 * T, row * 0.5, T, 0.5)]) },
  { id: "6-feature", label: "Feature", cells: [r(0, 0, 2 * T, 2 * T), r(2 * T, 0, T, T), r(2 * T, T, T, T), r(0, 2 * T, T, T), r(T, 2 * T, T, T), r(2 * T, 2 * T, T, T)] },
];

export const MIN_PHOTOS = 2;
export const MAX_PHOTOS = 6;
export const COLLAGE_WIDTH = 1080;
export const COLLAGE_HEIGHT = 1920;

export function layoutsFor(count: number): CollageLayout[] {
  return COLLAGE_LAYOUTS.filter((l) => l.cells.length === count);
}

export function layoutById(id: string): CollageLayout | undefined {
  return COLLAGE_LAYOUTS.find((l) => l.id === id);
}

/** The given layout if it has `count` cells, else the first layout that does. */
export function layoutForCount(count: number, preferred?: string): CollageLayout {
  const p = preferred ? layoutById(preferred) : undefined;
  if (p && p.cells.length === count) return p;
  return layoutsFor(count)[0] ?? layoutsFor(Math.max(MIN_PHOTOS, Math.min(MAX_PHOTOS, count)))[0];
}

const near = (a: number, b: number) => Math.abs(a - b) < 1e-6;

/**
 * Pixel rectangles of a layout's cells on a `width` x `height` canvas with `border` px
 * outside and between cells (every gap is exactly `border`).
 */
export function cellRects(layout: CollageLayout, width: number, height: number, border: number): Rect[] {
  return layout.cells.map((c) => {
    const left = c.x * width + (near(c.x, 0) ? border : border / 2);
    const right = (c.x + c.w) * width - (near(c.x + c.w, 1) ? border : border / 2);
    const top = c.y * height + (near(c.y, 0) ? border : border / 2);
    const bottom = (c.y + c.h) * height - (near(c.y + c.h, 1) ? border : border / 2);
    return { x: left, y: top, w: Math.max(0, right - left), h: Math.max(0, bottom - top) };
  });
}

/** Index of the cell containing (px, py), or -1. */
export function cellAt(rects: readonly Rect[], px: number, py: number): number {
  return rects.findIndex((c) => px >= c.x && px <= c.x + c.w && py >= c.y && py <= c.y + c.h);
}

/** How a photo sits in its cell: zoom >= 1 over cover-fit, view centre in image units 0-1. */
export interface CellView {
  zoom: number;
  cx: number;
  cy: number;
}

export const DEFAULT_VIEW: CellView = { zoom: 1, cx: 0.5, cy: 0.5 };
export const MAX_CELL_ZOOM = 5;

/** The source rectangle (image px) shown in a `cellW` x `cellH` cell, clamped inside the image. */
export function coverSource(imgW: number, imgH: number, cellW: number, cellH: number, view: CellView): { sx: number; sy: number; sw: number; sh: number } {
  const zoom = Math.max(1, Math.min(MAX_CELL_ZOOM, view.zoom));
  const cover = Math.max(cellW / imgW, cellH / imgH) * zoom;
  const sw = Math.min(imgW, cellW / cover);
  const sh = Math.min(imgH, cellH / cover);
  const sx = Math.max(0, Math.min(imgW - sw, view.cx * imgW - sw / 2));
  const sy = Math.max(0, Math.min(imgH - sh, view.cy * imgH - sh / 2));
  return { sx, sy, sw, sh };
}

/** The view with its centre pulled back so the image still covers the cell. */
export function clampView(imgW: number, imgH: number, cellW: number, cellH: number, view: CellView): CellView {
  const zoom = Math.max(1, Math.min(MAX_CELL_ZOOM, view.zoom));
  const s = coverSource(imgW, imgH, cellW, cellH, { ...view, zoom });
  return { zoom, cx: (s.sx + s.sw / 2) / imgW, cy: (s.sy + s.sh / 2) / imgH };
}

/** Drag the photo by (dx, dy) cell pixels: the view centre moves the other way. */
export function panView(imgW: number, imgH: number, cellW: number, cellH: number, view: CellView, dx: number, dy: number): CellView {
  const s = coverSource(imgW, imgH, cellW, cellH, view);
  const k = s.sw / cellW; // image px per cell px
  return clampView(imgW, imgH, cellW, cellH, { ...view, cx: view.cx - (dx * k) / imgW, cy: view.cy - (dy * k) / imgH });
}

export function zoomView(imgW: number, imgH: number, cellW: number, cellH: number, view: CellView, factor: number): CellView {
  return clampView(imgW, imgH, cellW, cellH, { ...view, zoom: view.zoom * factor });
}

export interface CollageSlot {
  photoId: string;
  view: CellView;
}

export interface CollageState {
  layoutId: string;
  slots: CollageSlot[];
  /** Space between and around the photos, px at COLLAGE_WIDTH (scaled for other widths). */
  gap: number;
  /** Background colour, seen in the gaps. */
  background: string;
  /** Outline drawn around each photo, px at COLLAGE_WIDTH (0 = none). */
  border: number;
  borderColor: string;
  /** Corner radius in px at COLLAGE_WIDTH. */
  radius: number;
  overlays: Overlay[];
}

export const COLLAGE_COLORS = ["#ffffff", "#000000", "#f5f5f4", "#1e293b", "#fde68a", "#fbcfe8", "#bfdbfe", "#bbf7d0", "#fecaca"];

export function swapSlots(slots: readonly CollageSlot[], a: number, b: number): CollageSlot[] {
  if (a === b || !slots[a] || !slots[b]) return slots as CollageSlot[];
  const out = [...slots];
  [out[a], out[b]] = [{ ...out[b], view: DEFAULT_VIEW }, { ...out[a], view: DEFAULT_VIEW }];
  return out;
}

export interface CollagePhoto {
  image: CanvasImageSource;
  width: number;
  height: number;
}

type Ctx = CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;

function clipRound(ctx: Ctx, c: Rect, radius: number) {
  const rr = Math.min(radius, c.w / 2, c.h / 2);
  ctx.beginPath();
  if (rr <= 0) { ctx.rect(c.x, c.y, c.w, c.h); return; }
  ctx.moveTo(c.x + rr, c.y);
  ctx.arcTo(c.x + c.w, c.y, c.x + c.w, c.y + c.h, rr);
  ctx.arcTo(c.x + c.w, c.y + c.h, c.x, c.y + c.h, rr);
  ctx.arcTo(c.x, c.y + c.h, c.x, c.y, rr);
  ctx.arcTo(c.x, c.y, c.x + c.w, c.y, rr);
  ctx.closePath();
}

/** Draw the collage (overlays included) filling `width` x `height`. */
export function drawCollage(ctx: Ctx, state: CollageState, photos: ReadonlyMap<string, CollagePhoto>, width: number, height: number, withOverlays = true): void {
  const layout = layoutForCount(state.slots.length, state.layoutId);
  const k = width / COLLAGE_WIDTH;
  ctx.save();
  ctx.fillStyle = state.background;
  ctx.fillRect(0, 0, width, height);
  const rects = cellRects(layout, width, height, state.gap * k);
  rects.forEach((c, i) => {
    const slot = state.slots[i];
    const photo = slot ? photos.get(slot.photoId) : undefined;
    ctx.save();
    clipRound(ctx, c, state.radius * k);
    ctx.clip();
    if (photo) {
      const s = coverSource(photo.width, photo.height, c.w, c.h, slot.view);
      ctx.imageSmoothingQuality = "high";
      ctx.drawImage(photo.image, s.sx, s.sy, s.sw, s.sh, c.x, c.y, c.w, c.h);
    } else {
      ctx.fillStyle = "#262626";
      ctx.fillRect(c.x, c.y, c.w, c.h);
    }
    if (state.border > 0) {
      // Stroked inside the clip: the outline sits within the cell.
      ctx.lineWidth = state.border * k * 2;
      ctx.strokeStyle = state.borderColor;
      clipRound(ctx, c, state.radius * k);
      ctx.stroke();
    }
    ctx.restore();
  });
  ctx.restore();
  if (withOverlays) drawOverlays(ctx, state.overlays, width, height);
}
