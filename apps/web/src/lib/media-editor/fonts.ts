// Fonts shared by the media editors (overlay text, text statuses). Framework-free: the
// families default to system stacks and the app registers its self-hosted web fonts at
// start-up (components/media-editor/fonts.ts), so the DOM preview, the canvas export and
// the status viewer all draw with the same faces.

export interface EditorFont {
  /** Stable index: text statuses store it (the server accepts 0-4). */
  id: number;
  label: string;
  /** CSS font-family list used until the app registers its web font. */
  fallback: string;
  weight: number;
  style: "normal" | "italic";
  /** Whether text is drawn upper-cased (display face). */
  uppercase: boolean;
  /** Letter spacing in em. */
  tracking: number;
}

export const EDITOR_FONTS: readonly EditorFont[] = [
  { id: 0, label: "Sans", fallback: "Inter, ui-sans-serif, system-ui, sans-serif", weight: 600, style: "normal", uppercase: false, tracking: 0 },
  { id: 1, label: "Serif", fallback: "'Playfair Display', Georgia, 'Times New Roman', serif", weight: 700, style: "normal", uppercase: false, tracking: 0 },
  { id: 2, label: "Typewriter", fallback: "'Courier Prime', 'Courier New', ui-monospace, monospace", weight: 700, style: "normal", uppercase: false, tracking: 0 },
  { id: 3, label: "Script", fallback: "Pacifico, 'Brush Script MT', cursive", weight: 400, style: "normal", uppercase: false, tracking: 0 },
  { id: 4, label: "Bold", fallback: "'Bebas Neue', Impact, 'Arial Narrow', sans-serif", weight: 400, style: "normal", uppercase: true, tracking: 0.03 },
  { id: 5, label: "Hand", fallback: "Caveat, 'Comic Sans MS', cursive", weight: 700, style: "normal", uppercase: false, tracking: 0 },
];

/** How many fonts a text status may use (server range 0-4). */
export const TEXT_STATUS_FONT_COUNT = 5;

const families: string[] = EDITOR_FONTS.map((f) => f.fallback);

/** Register the web-font family for font `id` (keeps the fallback stack behind it). */
export function registerFontFamily(id: number, family: string): void {
  const font = EDITOR_FONTS[id];
  if (!font || !family) return;
  families[id] = `${family}, ${font.fallback}`;
}

export function fontOf(id: number): EditorFont {
  return EDITOR_FONTS[id] ?? EDITOR_FONTS[0];
}

/** CSS font-family for font `id`. */
export function fontFamily(id: number): string {
  return families[id] ?? families[0];
}

/** Canvas / CSS `font` shorthand for font `id` at `px`. */
export function fontCss(id: number, px: number): string {
  const f = fontOf(id);
  return `${f.style === "italic" ? "italic " : ""}${f.weight} ${Math.max(1, px)}px ${fontFamily(id)}`;
}

/** Inline style for DOM text in font `id` (size set by the caller). */
export function fontStyle(id: number): { fontFamily: string; fontWeight: number; fontStyle: "normal" | "italic"; textTransform: "uppercase" | "none"; letterSpacing: string } {
  const f = fontOf(id);
  return { fontFamily: fontFamily(id), fontWeight: f.weight, fontStyle: f.style, textTransform: f.uppercase ? "uppercase" : "none", letterSpacing: `${f.tracking}em` };
}

/** Text as it is drawn in font `id`. */
export function displayText(id: number, text: string): string {
  return fontOf(id).uppercase ? text.toUpperCase() : text;
}

/** Emoji stack used for stickers. */
export const EMOJI_FONT = "'Apple Color Emoji', 'Segoe UI Emoji', 'Noto Color Emoji', 'Segoe UI Symbol', sans-serif";

let fontsReady: Promise<void> | null = null;

/**
 * Resolve once every editor font is loaded (call before drawing to a canvas; the canvas does
 * not wait for web fonts). Safe without a DOM.
 */
export function loadEditorFonts(): Promise<void> {
  if (typeof document === "undefined" || !document.fonts?.load) return Promise.resolve();
  if (!fontsReady) {
    fontsReady = Promise.all(EDITOR_FONTS.map((f) => document.fonts.load(fontCss(f.id, 48), "AaBb").catch(() => []))).then(() => undefined);
  }
  return fontsReady;
}

/** Forget the cached load (e.g. after registering families). */
export function resetFontLoad(): void {
  fontsReady = null;
}
