// Burning text, stickers, emojis and freehand drawing into a status video. The overlays are
// the same normalised state the on-screen OverlayLayer edits (positions 0-1 of the OUTPUT
// frame, after crop / rotate), drawn with the same `drawOverlays` the photo export uses, so
// every frame matches the editor. They don't change over time, so the layer is painted once
// per output size and each frame only blits it (cheap on slow phones).
//
// Framework-free; the canvas factory is injectable so node tests can check the pipeline.

import { drawOverlays, isPlaced, type Overlay, type TextMeasurer } from "./overlays";

type Ctx2D = CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;

/** A layer to paint the overlays on once: anything `drawImage` accepts, with its own context. */
export type OverlayLayerCanvas = { canvas: CanvasImageSource; ctx: Ctx2D };
export type OverlayLayerFactory = (width: number, height: number) => OverlayLayerCanvas;

/** The frame drawer `renderVideoEdit` / `exportVideo` call for every output frame. */
export type FrameOverlayDrawer = (ctx: Ctx2D, width: number, height: number) => void;

/** Whether any overlay would leave a mark (empty text / one-point strokes are ignored). */
export function hasVisibleOverlays(overlays: readonly Overlay[] | null | undefined): boolean {
  return Boolean(overlays?.some((o) => (isPlaced(o) ? (o.type === "text" ? o.text.trim().length > 0 : o.emoji.length > 0) : o.points.length >= 2)));
}

function defaultFactory(width: number, height: number): OverlayLayerCanvas {
  if (typeof OffscreenCanvas === "function") {
    const canvas = new OffscreenCanvas(width, height);
    const ctx = canvas.getContext("2d");
    if (ctx) return { canvas, ctx };
  }
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Canvas 2D is not available.");
  return { canvas, ctx };
}

/**
 * A per-frame drawer for `overlays`, or null when nothing would be drawn (so an untouched
 * video can still be copied without re-encoding). The overlay layer is rendered once per
 * output size (parts of one edit share it) and composited onto each frame.
 * Await `loadEditorFonts()` before the first frame so text uses the editor's fonts.
 */
export function overlayFrameDrawer(
  overlays: readonly Overlay[] | null | undefined,
  options: { makeLayer?: OverlayLayerFactory; draw?: typeof drawOverlays; measure?: TextMeasurer } = {},
): FrameOverlayDrawer | null {
  if (!overlays || !hasVisibleOverlays(overlays)) return null;
  const list = overlays.slice();
  const makeLayer = options.makeLayer ?? defaultFactory;
  const draw = options.draw ?? drawOverlays;
  let layer: { width: number; height: number; canvas: CanvasImageSource } | null = null;
  return (ctx, width, height) => {
    const w = Math.max(1, Math.round(width));
    const h = Math.max(1, Math.round(height));
    if (!layer || layer.width !== w || layer.height !== h) {
      const made = makeLayer(w, h);
      if (options.measure) draw(made.ctx, list, w, h, options.measure);
      else draw(made.ctx, list, w, h);
      layer = { width: w, height: h, canvas: made.canvas };
    }
    ctx.drawImage(layer.canvas, 0, 0, w, h);
  };
}
