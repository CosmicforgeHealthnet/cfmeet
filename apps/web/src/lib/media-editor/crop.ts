// Crop / rotate / flip maths for the photo editor. Framework-free.
//
// A crop is a rectangle normalised (0-1) to the ORIENTED image: the source after its quarter
// turn and flips. Rotating or flipping transforms the crop along with the image.

export type Rotation = 0 | 90 | 180 | 270;

export interface Crop {
  x: number;
  y: number;
  w: number;
  h: number;
}

export type CropHandle = "n" | "s" | "e" | "w" | "ne" | "nw" | "se" | "sw";

export const FULL_CROP: Crop = { x: 0, y: 0, w: 1, h: 1 };
/** Smallest crop side, as a fraction of the oriented image side. */
export const MIN_CROP = 0.05;

export interface AspectPreset {
  id: string;
  label: string;
  /** Width / height in pixels; null = free, "original" = the oriented image's. */
  ratio: number | null | "original";
}

export const ASPECT_PRESETS: readonly AspectPreset[] = [
  { id: "free", label: "Free", ratio: null },
  { id: "original", label: "Original", ratio: "original" },
  { id: "1:1", label: "Square", ratio: 1 },
  { id: "4:5", label: "4:5", ratio: 4 / 5 },
  { id: "3:4", label: "3:4", ratio: 3 / 4 },
  { id: "9:16", label: "9:16", ratio: 9 / 16 },
  { id: "4:3", label: "4:3", ratio: 4 / 3 },
  { id: "16:9", label: "16:9", ratio: 16 / 9 },
];

const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/** Size of the image after a quarter turn. */
export function orientedSize(width: number, height: number, rotation: Rotation): { width: number; height: number } {
  return rotation === 90 || rotation === 270 ? { width: height, height: width } : { width, height };
}

/** Pixel ratio of a preset for an oriented image (null = free). */
export function presetRatio(preset: AspectPreset, ow: number, oh: number): number | null {
  return preset.ratio === "original" ? ow / oh : preset.ratio;
}

/** Pixel aspect (w / h) of a crop on an `ow` x `oh` oriented image. */
export function cropAspect(c: Crop, ow: number, oh: number): number {
  return (c.w * ow) / (c.h * oh);
}

/** Keep the crop inside the image with at least the minimum size. */
export function clampCrop(c: Crop): Crop {
  const w = clamp(c.w, MIN_CROP, 1);
  const h = clamp(c.h, MIN_CROP, 1);
  return { x: clamp(c.x, 0, 1 - w), y: clamp(c.y, 0, 1 - h), w, h };
}

/** The crop after rotating the image a quarter turn clockwise (dir 1) or anticlockwise (-1). */
export function rotateCrop(c: Crop, dir: 1 | -1): Crop {
  return dir === 1 ? { x: 1 - (c.y + c.h), y: c.x, w: c.h, h: c.w } : { x: c.y, y: 1 - (c.x + c.w), w: c.h, h: c.w };
}

export function rotateBy(r: Rotation, dir: 1 | -1): Rotation {
  return (((r + dir * 90) % 360) + 360) % 360 as Rotation;
}

export const flipCropH = (c: Crop): Crop => ({ ...c, x: 1 - c.x - c.w });
export const flipCropV = (c: Crop): Crop => ({ ...c, y: 1 - c.y - c.h });

/**
 * The largest crop of pixel `ratio` that fits the image, centred on `c`'s centre (shifted
 * inside the image where needed).
 */
export function fitAspect(c: Crop, ratio: number, ow: number, oh: number): Crop {
  // In normalised units a crop of pixel ratio r has w / h = r * oh / ow.
  const nr = (ratio * oh) / ow;
  let w = 1;
  let h = w / nr;
  if (h > 1) { h = 1; w = nr; }
  const cx = c.x + c.w / 2;
  const cy = c.y + c.h / 2;
  return { x: clamp(cx - w / 2, 0, 1 - w), y: clamp(cy - h / 2, 0, 1 - h), w, h };
}

/** Move a crop by a normalised delta, kept inside the image. */
export function moveCrop(c: Crop, dx: number, dy: number): Crop {
  return { ...c, x: clamp(c.x + dx, 0, 1 - c.w), y: clamp(c.y + dy, 0, 1 - c.h) };
}

/**
 * Drag a handle by a normalised delta. Free crops move the grabbed edges; with a pixel
 * `ratio` the opposite corner (or edge centre) stays put and the aspect is kept.
 */
export function resizeCrop(c: Crop, handle: CropHandle, dx: number, dy: number, ratio: number | null, ow: number, oh: number): Crop {
  let left = c.x;
  let top = c.y;
  let right = c.x + c.w;
  let bottom = c.y + c.h;
  if (handle.includes("w")) left = clamp(left + dx, 0, right - MIN_CROP);
  if (handle.includes("e")) right = clamp(right + dx, left + MIN_CROP, 1);
  if (handle.includes("n")) top = clamp(top + dy, 0, bottom - MIN_CROP);
  if (handle.includes("s")) bottom = clamp(bottom + dy, top + MIN_CROP, 1);
  if (ratio === null) return { x: left, y: top, w: right - left, h: bottom - top };

  const nr = (ratio * oh) / ow; // normalised w / h
  const horizontal = handle === "e" || handle === "w";
  const vertical = handle === "n" || handle === "s";
  let w = right - left;
  let h = bottom - top;
  if (horizontal) h = w / nr;
  else if (vertical) w = h * nr;
  else if (Math.abs(dx) * ow >= Math.abs(dy) * oh) h = w / nr;
  else w = h * nr;

  // Anchor: the opposite corner, or the centre of the opposite edge.
  const ax = handle.includes("w") ? c.x + c.w : handle.includes("e") ? c.x : c.x + c.w / 2;
  const ay = handle.includes("n") ? c.y + c.h : handle.includes("s") ? c.y : c.y + c.h / 2;
  // Room available from the anchor in the growth direction.
  const roomW = handle.includes("w") ? ax : handle.includes("e") ? 1 - ax : 2 * Math.min(ax, 1 - ax);
  const roomH = handle.includes("n") ? ay : handle.includes("s") ? 1 - ay : 2 * Math.min(ay, 1 - ay);
  const k = Math.min(1, roomW / w, roomH / h);
  w *= k;
  h *= k;
  const minK = Math.max(MIN_CROP / w, MIN_CROP / h, 1);
  if (minK > 1 && w * minK <= roomW + 1e-9 && h * minK <= roomH + 1e-9) { w *= minK; h *= minK; }
  const x = handle.includes("w") ? ax - w : handle.includes("e") ? ax : ax - w / 2;
  const y = handle.includes("n") ? ay - h : handle.includes("s") ? ay : ay - h / 2;
  return { x, y, w, h };
}

/**
 * Zoom into (factor > 1) or out of a crop around its centre, keeping its aspect, never past
 * the image or below the minimum size.
 */
export function zoomCrop(c: Crop, factor: number): Crop {
  const maxK = Math.min(1 / c.w, 1 / c.h);
  const minK = Math.max(MIN_CROP / c.w, MIN_CROP / c.h);
  const k = clamp(1 / factor, minK, maxK);
  const w = c.w * k;
  const h = c.h * k;
  const cx = c.x + c.w / 2;
  const cy = c.y + c.h / 2;
  return { x: clamp(cx - w / 2, 0, 1 - w), y: clamp(cy - h / 2, 0, 1 - h), w, h };
}

/** Integer pixel rectangle of a crop on an `ow` x `oh` oriented image. */
export function cropPixels(c: Crop, ow: number, oh: number): { x: number; y: number; width: number; height: number } {
  const x = Math.round(c.x * ow);
  const y = Math.round(c.y * oh);
  return { x, y, width: Math.max(1, Math.min(ow - x, Math.round(c.w * ow))), height: Math.max(1, Math.min(oh - y, Math.round(c.h * oh))) };
}

/** Scale `width` x `height` so the longest side is at most `max` (never enlarged). */
export function fitLongSide(width: number, height: number, max: number): { width: number; height: number } {
  const k = Math.min(1, max / Math.max(width, height));
  return { width: Math.max(1, Math.round(width * k)), height: Math.max(1, Math.round(height * k)) };
}

/** Largest `width` x `height` box of the given aspect inside `bw` x `bh`. */
export function containSize(aspect: number, bw: number, bh: number): { width: number; height: number } {
  if (bw <= 0 || bh <= 0) return { width: 0, height: 0 };
  return bw / bh > aspect ? { width: bh * aspect, height: bh } : { width: bw, height: bw / aspect };
}

type Ctx = CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;

/**
 * Set `ctx`'s transform so drawing the SOURCE image at (0, 0) lands its rotated, flipped and
 * cropped part exactly on `outW` x `outH`.
 */
export function applyOrientation(ctx: Ctx, srcW: number, srcH: number, rotation: Rotation, flipH: boolean, flipV: boolean, crop: Crop, outW: number, outH: number): void {
  const { width: ow, height: oh } = orientedSize(srcW, srcH, rotation);
  const cw = crop.w * ow;
  const ch = crop.h * oh;
  ctx.scale(outW / cw, outH / ch);
  ctx.translate(-crop.x * ow, -crop.y * oh);
  if (flipH) { ctx.translate(ow, 0); ctx.scale(-1, 1); }
  if (flipV) { ctx.translate(0, oh); ctx.scale(1, -1); }
  if (rotation === 90) { ctx.translate(ow, 0); ctx.rotate(Math.PI / 2); }
  else if (rotation === 180) { ctx.translate(ow, oh); ctx.rotate(Math.PI); }
  else if (rotation === 270) { ctx.translate(0, oh); ctx.rotate(-Math.PI / 2); }
}

/** Map a source pixel to oriented pixels (the same transform as applyOrientation, for tests). */
export function orientPoint(x: number, y: number, srcW: number, srcH: number, rotation: Rotation, flipH: boolean, flipV: boolean): { x: number; y: number } {
  const { width: ow, height: oh } = orientedSize(srcW, srcH, rotation);
  let px = x;
  let py = y;
  if (rotation === 90) [px, py] = [ow - y, x];
  else if (rotation === 180) [px, py] = [ow - x, oh - y];
  else if (rotation === 270) [px, py] = [y, oh - x];
  if (flipV) py = oh - py;
  if (flipH) px = ow - px;
  return { x: px, y: py };
}
