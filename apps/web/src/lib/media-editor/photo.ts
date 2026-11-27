// Photo pipeline: decode (EXIF orientation applied) -> quarter turn / flip / crop -> colour
// matrix -> overlays -> JPEG or WebP. The editor's preview and the export call the same
// functions at different sizes, so what is seen is what is posted. Framework-free.

import { applyOrientation, cropPixels, FULL_CROP, orientedSize, fitLongSide, type Crop, type Rotation } from "./crop";
import { applyMatrix, editMatrix, isIdentity, NO_ADJUSTMENTS, type Adjustments } from "./filters";
import { loadEditorFonts } from "./fonts";
import { drawOverlays, type Overlay } from "./overlays";

export interface PhotoEdits {
  crop: Crop;
  rotation: Rotation;
  flipH: boolean;
  flipV: boolean;
  /** FILTER_PRESETS id. */
  filter: string;
  adjust: Adjustments;
  /** Longest side of the output, at most PHOTO_MAX_SIDE. */
  maxSide: number;
}

export interface PhotoEditorState {
  edits: PhotoEdits;
  overlays: Overlay[];
}

/** Longest side of an exported photo. */
export const PHOTO_MAX_SIDE = 2560;
/** Output sizes offered by "Resize". */
export const PHOTO_SIDES = [2560, 1920, 1280, 1080, 720] as const;
/** Exports aim below this (the status upload limit is 8 MB). */
export const PHOTO_TARGET_BYTES = 5 * 1024 * 1024;
/** Decoded working copies are at most this big (memory on phones). */
export const WORKING_MAX_SIDE = 4096;

export const DEFAULT_EDITS: PhotoEdits = { crop: FULL_CROP, rotation: 0, flipH: false, flipV: false, filter: "none", adjust: NO_ADJUSTMENTS, maxSide: PHOTO_MAX_SIDE };
export const initialPhotoState = (): PhotoEditorState => ({ edits: DEFAULT_EDITS, overlays: [] });

/** Output pixel size for the edits on a `srcW` x `srcH` source. */
export function outputSize(srcW: number, srcH: number, edits: PhotoEdits): { width: number; height: number } {
  const o = orientedSize(srcW, srcH, edits.rotation);
  const c = cropPixels(edits.crop, o.width, o.height);
  return fitLongSide(c.width, c.height, Math.min(PHOTO_MAX_SIDE, edits.maxSide));
}

export interface Source {
  image: CanvasImageSource;
  width: number;
  height: number;
}

type Ctx = CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;

/** Draw the edited photo (no overlays) filling `width` x `height` of `ctx`. */
export function drawPhoto(ctx: Ctx, src: Source, edits: Pick<PhotoEdits, "crop" | "rotation" | "flipH" | "flipV" | "filter" | "adjust">, width: number, height: number): void {
  ctx.save();
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = "high";
  applyOrientation(ctx, src.width, src.height, edits.rotation, edits.flipH, edits.flipV, edits.crop, width, height);
  ctx.drawImage(src.image, 0, 0, src.width, src.height);
  ctx.restore();
  const m = editMatrix(edits.filter, edits.adjust);
  const w = Math.round(width);
  const h = Math.round(height);
  if (w > 0 && h > 0 && !isIdentity(m)) {
    const img = ctx.getImageData(0, 0, w, h);
    applyMatrix(img.data, m);
    ctx.putImageData(img, 0, 0);
  }
}

/** Decode a photo with its EXIF orientation applied, shrunk to the working size. */
export async function decodePhoto(file: Blob): Promise<Source & { close(): void }> {
  const probe = await createImageBitmap(file, { imageOrientation: "from-image" });
  const fit = fitLongSide(probe.width, probe.height, WORKING_MAX_SIDE);
  if (fit.width === probe.width && fit.height === probe.height) return { image: probe, width: probe.width, height: probe.height, close: () => probe.close() };
  probe.close();
  const bmp = await createImageBitmap(file, { imageOrientation: "from-image", resizeWidth: fit.width, resizeHeight: fit.height, resizeQuality: "high" });
  return { image: bmp, width: bmp.width, height: bmp.height, close: () => bmp.close() };
}

/** Encode with `type` at `quality`; null when the browser cannot. */
export type Encoder = (type: string, quality: number) => Promise<Blob | null>;

const QUALITIES = [0.9, 0.84, 0.76, 0.68, 0.58, 0.48] as const;

/**
 * The smaller of JPEG and WebP at the highest quality that fits `target` bytes. A browser
 * that answers a WebP request with PNG (older Safari) is treated as having no WebP.
 */
export async function encodeSmallest(encode: Encoder, target = PHOTO_TARGET_BYTES): Promise<Blob> {
  let best: Blob | null = null;
  let webp = true;
  for (const q of QUALITIES) {
    const jpeg = await encode("image/jpeg", q);
    const w = webp ? await encode("image/webp", q) : null;
    if (w && w.type !== "image/webp") webp = false;
    const candidates = [jpeg, w && w.type === "image/webp" ? w : null].filter((b): b is Blob => !!b);
    const smallest = candidates.sort((a, b) => a.size - b.size)[0] ?? null;
    if (smallest && (!best || smallest.size < best.size)) best = smallest;
    if (smallest && smallest.size <= target) return smallest;
  }
  if (!best) throw new Error("This browser could not encode the photo.");
  return best;
}

function canvasOf(width: number, height: number): { canvas: HTMLCanvasElement | OffscreenCanvas; ctx: Ctx } {
  if (typeof OffscreenCanvas !== "undefined") {
    const canvas = new OffscreenCanvas(width, height);
    const ctx = canvas.getContext("2d");
    if (ctx) return { canvas, ctx };
  }
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Canvas is not available.");
  return { canvas, ctx };
}

/** Encoder for a canvas (OffscreenCanvas or element). */
export function canvasEncoder(canvas: HTMLCanvasElement | OffscreenCanvas): Encoder {
  return (type, quality) =>
    "convertToBlob" in canvas
      ? canvas.convertToBlob({ type, quality }).catch(() => null)
      : new Promise((resolve) => (canvas as HTMLCanvasElement).toBlob((b) => resolve(b), type, quality));
}

export interface ExportedImage {
  blob: Blob;
  width: number;
  height: number;
}

/** A blank canvas of the given size with a 2D context. */
export function makeCanvas(width: number, height: number) {
  return canvasOf(width, height);
}

/** Render and encode the final photo. */
export async function exportPhoto(src: Source, state: PhotoEditorState): Promise<ExportedImage> {
  await loadEditorFonts();
  const { width, height } = outputSize(src.width, src.height, state.edits);
  const { canvas, ctx } = canvasOf(width, height);
  drawPhoto(ctx, src, state.edits, width, height);
  drawOverlays(ctx, state.overlays, width, height);
  const blob = await encodeSmallest(canvasEncoder(canvas));
  return { blob, width, height };
}
