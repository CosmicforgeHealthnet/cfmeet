// Self-hosted web fonts for the media editors (bundled by next/font at build time, served
// from this origin, so the CSP's `font-src 'self'` holds). Importing this module registers
// the families with lib/media-editor/fonts, which the canvas export and the DOM share.
import { Bebas_Neue, Caveat, Courier_Prime, Inter, Pacifico, Playfair_Display } from "next/font/google";
import { registerFontFamily, resetFontLoad } from "@/lib/media-editor/fonts";

const sans = Inter({ subsets: ["latin"], weight: "600", display: "swap", preload: false });
const serif = Playfair_Display({ subsets: ["latin"], weight: "700", display: "swap", preload: false });
const typewriter = Courier_Prime({ subsets: ["latin"], weight: "700", display: "swap", preload: false });
const script = Pacifico({ subsets: ["latin"], weight: "400", display: "swap", preload: false });
const display = Bebas_Neue({ subsets: ["latin"], weight: "400", display: "swap", preload: false });
const hand = Caveat({ subsets: ["latin"], weight: "700", display: "swap", preload: false });

[sans, serif, typewriter, script, display, hand].forEach((f, i) => registerFontFamily(i, f.style.fontFamily));
resetFontLoad();

/** Class names that make sure every editor font's @font-face is part of the page. */
export const EDITOR_FONT_CLASSES = [sans, serif, typewriter, script, display, hand].map((f) => f.className).join(" ");
