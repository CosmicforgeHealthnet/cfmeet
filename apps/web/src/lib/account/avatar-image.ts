// Gap audit A22 (web): the Meeting Service keeps one avatar image (PNG, JPEG or WebP, at
// most 512 KiB, recognised by its content). Phone photos are several MB, so the chosen
// image is shrunk in the browser first: decoded (EXIF orientation applied), scaled so its
// longest side is at most 512 px (aspect ratio kept; the avatar is shown with object-cover,
// the server does not require a square), and encoded as JPEG at decreasing quality until
// it is comfortably under the limit. Small PNG / JPEG / WebP files are sent unchanged.
//   node --experimental-strip-types --test tests/avatar-image.test.mjs

/** The server's limit (`MAX_AVATAR_BYTES` in services/meeting-service account_avatar.rs). */
export const AVATAR_MAX_BYTES = 512 * 1024;
/** What a shrunk image must fit in: the limit less a safety margin. */
export const AVATAR_TARGET_BYTES = 480 * 1024;
/** Longest side of a shrunk image, then smaller fallbacks if quality alone is not enough. */
export const AVATAR_SIDES = [512, 384, 256] as const;
/** JPEG qualities tried in order, at each size. */
export const AVATAR_QUALITIES = [0.9, 0.82, 0.72, 0.6, 0.48, 0.36] as const;
/** Types the server accepts. */
export const AVATAR_ACCEPTED_TYPES = ["image/png", "image/jpeg", "image/webp"] as const;

export type AvatarImageError = "not-image" | "unsupported-heic" | "undecodable" | "too-big";

export type AvatarImageResult = { ok: true; file: File } | { ok: false; error: AvatarImageError };

/** User-facing message for each failure, in the settings screen's existing wording. */
export const AVATAR_ERROR_MESSAGES: Record<AvatarImageError, string> = {
  "not-image": "Use a PNG, JPEG or WebP image.",
  "unsupported-heic": "This browser can't open HEIC photos. Choose a JPEG or PNG.",
  undecodable: "That photo couldn't be opened. Choose a JPEG or PNG.",
  "too-big": "That photo is too big. Use one under 512 KB.",
};

/** A decoded image ready to draw. */
export interface DecodedImage {
  width: number;
  height: number;
  source: CanvasImageSource;
  close?: () => void;
}

/** Decoding and encoding, injectable so the sizing rules can be tested without a browser. */
export interface AvatarImageCodec {
  decode(file: Blob): Promise<DecodedImage>;
  /** Draw `image` at `width` x `height` and encode it as `type` at `quality`. */
  encode(image: DecodedImage, width: number, height: number, type: string, quality: number): Promise<Blob | null>;
}

/** Scale `width` x `height` so the longest side is at most `max` (never enlarged). */
export function fitWithin(width: number, height: number, max: number): { width: number; height: number } {
  const longest = Math.max(width, height);
  if (longest <= max) return { width: Math.max(1, Math.round(width)), height: Math.max(1, Math.round(height)) };
  const scale = max / longest;
  return { width: Math.max(1, Math.round(width * scale)), height: Math.max(1, Math.round(height * scale)) };
}

const isHeic = (file: File) => /^image\/hei[cf](-sequence)?$/i.test(file.type) || /\.hei[cf]$/i.test(file.name);

/** Whether `file` looks like an image at all (by type, or by name when the type is empty). */
export function looksLikeImage(file: File): boolean {
  if (file.type) return file.type.toLowerCase().startsWith("image/");
  return /\.(png|jpe?g|webp|gif|bmp|avif|hei[cf])$/i.test(file.name);
}

const jpegName = (name: string) => `${name.replace(/\.[^./\\]*$/, "") || "avatar"}.jpg`;

/** Make `file` uploadable as an avatar: unchanged if it already fits, else shrunk to JPEG. */
export async function prepareAvatarImage(file: File, codec: AvatarImageCodec = browserCodec()): Promise<AvatarImageResult> {
  if (!looksLikeImage(file)) return { ok: false, error: "not-image" };
  if ((AVATAR_ACCEPTED_TYPES as readonly string[]).includes(file.type) && file.size > 0 && file.size <= AVATAR_TARGET_BYTES) {
    return { ok: true, file };
  }
  let image: DecodedImage;
  try {
    image = await codec.decode(file);
    if (!(image.width > 0 && image.height > 0)) throw new Error("empty image");
  } catch {
    return { ok: false, error: isHeic(file) ? "unsupported-heic" : "undecodable" };
  }
  try {
    for (const side of AVATAR_SIDES) {
      const { width, height } = fitWithin(image.width, image.height, side);
      for (const quality of AVATAR_QUALITIES) {
        const blob = await codec.encode(image, width, height, "image/jpeg", quality);
        if (!blob) return { ok: false, error: "undecodable" };
        if (blob.type && blob.type !== "image/jpeg") return { ok: false, error: "undecodable" };
        if (blob.size > 0 && blob.size <= AVATAR_TARGET_BYTES) {
          return { ok: true, file: new File([blob], jpegName(file.name), { type: "image/jpeg" }) };
        }
      }
    }
    return { ok: false, error: "too-big" };
  } catch {
    return { ok: false, error: "undecodable" };
  } finally {
    image.close?.();
  }
}

/** Decode with EXIF orientation applied: `createImageBitmap` where it can, else an `<img>`. */
async function decodeInBrowser(file: Blob): Promise<DecodedImage> {
  if (typeof createImageBitmap === "function") {
    let bitmap: ImageBitmap | null = null;
    try {
      bitmap = await createImageBitmap(file, { imageOrientation: "from-image" });
    } catch {
      // Older engines reject the option; their bitmaps are then decoded without it.
      try { bitmap = await createImageBitmap(file); } catch { bitmap = null; }
    }
    if (bitmap) {
      const b = bitmap;
      return { width: b.width, height: b.height, source: b, close: () => b.close() };
    }
  }
  if (typeof document === "undefined" || typeof URL === "undefined") throw new Error("cannot decode");
  const url = URL.createObjectURL(file);
  try {
    const img = document.createElement("img");
    img.decoding = "async";
    img.src = url;
    await img.decode();
    // <img> applies EXIF orientation by default (image-orientation: from-image).
    return { width: img.naturalWidth, height: img.naturalHeight, source: img };
  } finally {
    URL.revokeObjectURL(url);
  }
}

type Ctx2D = { fillStyle: unknown; fillRect(x: number, y: number, w: number, h: number): void; imageSmoothingQuality?: string; drawImage(source: CanvasImageSource, x: number, y: number, w: number, h: number): void };

async function encodeInBrowser(image: DecodedImage, width: number, height: number, type: string, quality: number): Promise<Blob | null> {
  const draw = (ctx: Ctx2D | null) => {
    if (!ctx) throw new Error("no 2d context");
    ctx.fillStyle = "#ffffff"; // JPEG has no transparency: put transparent PNGs on white.
    ctx.fillRect(0, 0, width, height);
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(image.source, 0, 0, width, height);
  };
  if (typeof OffscreenCanvas === "function") {
    const canvas = new OffscreenCanvas(width, height);
    draw(canvas.getContext("2d") as Ctx2D | null);
    return canvas.convertToBlob({ type, quality });
  }
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  draw(canvas.getContext("2d"));
  return new Promise((resolve) => canvas.toBlob(resolve, type, quality));
}

/** The browser's decoder and canvas encoder. */
export function browserCodec(): AvatarImageCodec {
  return { decode: decodeInBrowser, encode: encodeInBrowser };
}
