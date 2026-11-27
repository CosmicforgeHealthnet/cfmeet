// Photo filters and adjustments as 4x5 colour matrices applied to pixels. Pixel maths (rather
// than the canvas `filter` property, which some browsers lack) makes the preview and the export
// identical everywhere. Framework-free.

/** Row-major 4x5 matrix: [r g b a offset] for each output channel R, G, B, A; offsets in 0-255. */
export type ColorMatrix = readonly number[];

export const IDENTITY: ColorMatrix = [1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 1, 0];

/** `a` after `b` (apply b first, then a). */
export function multiply(a: ColorMatrix, b: ColorMatrix): ColorMatrix {
  const out: number[] = new Array(20).fill(0);
  for (let r = 0; r < 4; r++) {
    for (let c = 0; c < 5; c++) {
      let v = c === 4 ? a[r * 5 + 4] : 0;
      for (let k = 0; k < 4; k++) v += a[r * 5 + k] * b[k * 5 + c];
      out[r * 5 + c] = v;
    }
  }
  return out;
}

/** Compose matrices applied left to right. */
export function compose(...ms: ColorMatrix[]): ColorMatrix {
  return ms.reduce((acc, m) => multiply(m, acc), IDENTITY);
}

const LR = 0.2126;
const LG = 0.7152;
const LB = 0.0722;

export function saturation(s: number): ColorMatrix {
  const i = 1 - s;
  return [
    LR * i + s, LG * i, LB * i, 0, 0,
    LR * i, LG * i + s, LB * i, 0, 0,
    LR * i, LG * i, LB * i + s, 0, 0,
    0, 0, 0, 1, 0,
  ];
}

export function contrast(c: number): ColorMatrix {
  const o = 128 * (1 - c);
  return [c, 0, 0, 0, o, 0, c, 0, 0, o, 0, 0, c, 0, o, 0, 0, 0, 1, 0];
}

export function brightness(offset: number): ColorMatrix {
  return [1, 0, 0, 0, offset, 0, 1, 0, 0, offset, 0, 0, 1, 0, offset, 0, 0, 0, 1, 0];
}

export function channels(r: number, g: number, b: number): ColorMatrix {
  return [r, 0, 0, 0, 0, 0, g, 0, 0, 0, 0, 0, b, 0, 0, 0, 0, 0, 1, 0];
}

export function sepia(amount: number): ColorMatrix {
  const a = Math.max(0, Math.min(1, amount));
  const s = [0.393, 0.769, 0.189, 0.349, 0.686, 0.168, 0.272, 0.534, 0.131];
  const m: number[] = [];
  for (let r = 0; r < 3; r++) {
    for (let c = 0; c < 3; c++) m.push((r === c ? 1 - a : 0) + s[r * 3 + c] * a);
    m.push(0, 0);
  }
  return [...m, 0, 0, 0, 1, 0];
}

export interface FilterPreset {
  id: string;
  label: string;
  matrix: ColorMatrix;
}

export const FILTER_PRESETS: readonly FilterPreset[] = [
  { id: "none", label: "Original", matrix: IDENTITY },
  { id: "vivid", label: "Vivid", matrix: compose(saturation(1.35), contrast(1.1)) },
  { id: "warm", label: "Warm", matrix: compose(channels(1.08, 1.0, 0.88), saturation(1.08)) },
  { id: "cool", label: "Cool", matrix: compose(channels(0.9, 0.99, 1.08), saturation(1.02)) },
  { id: "golden", label: "Golden", matrix: compose(channels(1.1, 1.03, 0.82), contrast(1.05), brightness(6)) },
  { id: "fade", label: "Fade", matrix: compose(saturation(0.82), contrast(0.82), brightness(16)) },
  { id: "vintage", label: "Vintage", matrix: compose(sepia(0.42), contrast(0.9), brightness(10)) },
  { id: "dramatic", label: "Dramatic", matrix: compose(saturation(0.85), contrast(1.32), brightness(-8)) },
  { id: "mono", label: "Mono", matrix: saturation(0) },
  { id: "noir", label: "Noir", matrix: compose(saturation(0), contrast(1.45), brightness(-6)) },
];

export interface Adjustments {
  /** -100 to 100 each; 0 = unchanged. */
  brightness: number;
  contrast: number;
  saturation: number;
}

export const NO_ADJUSTMENTS: Adjustments = { brightness: 0, contrast: 0, saturation: 0 };

const unit = (v: number) => Math.max(-100, Math.min(100, v)) / 100;

/** The single matrix for a preset followed by the adjustments. */
export function editMatrix(filter: string, adj: Adjustments): ColorMatrix {
  const preset = FILTER_PRESETS.find((p) => p.id === filter)?.matrix ?? IDENTITY;
  return compose(preset, brightness(unit(adj.brightness) * 70), contrast(1 + unit(adj.contrast) * 0.8), saturation(1 + unit(adj.saturation)));
}

export function isIdentity(m: ColorMatrix): boolean {
  return m.every((v, i) => Math.abs(v - IDENTITY[i]) < 1e-9);
}

/** Apply `m` to RGBA pixels in place. */
export function applyMatrix(data: Uint8ClampedArray, m: ColorMatrix): void {
  if (isIdentity(m)) return;
  const [a0, a1, a2, , a4, b0, b1, b2, , b4, c0, c1, c2, , c4] = m;
  for (let i = 0; i < data.length; i += 4) {
    const r = data[i];
    const g = data[i + 1];
    const b = data[i + 2];
    data[i] = a0 * r + a1 * g + a2 * b + a4;
    data[i + 1] = b0 * r + b1 * g + b2 * b + b4;
    data[i + 2] = c0 * r + c1 * g + c2 * b + c4;
  }
}
