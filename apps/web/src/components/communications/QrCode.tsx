"use client";

import { useMemo } from "react";
import qrcode from "qrcode-generator";

/**
 * A QR code drawn as one SVG path (React elements only — no innerHTML, CSP-safe). The value is
 * a Hub connect payload: a secret disclosed once, so it is never logged or persisted here.
 */
export default function QrCode({ value, size = 192, label }: { value: string; size?: number; label: string }) {
  const { path, count } = useMemo(() => {
    const qr = qrcode(0, "M");
    qr.addData(value, "Byte");
    qr.make();
    const n = qr.getModuleCount();
    let d = "";
    for (let r = 0; r < n; r++) {
      for (let c = 0; c < n; c++) {
        if (qr.isDark(r, c)) d += `M${c + 4} ${r + 4}h1v1h-1z`;
      }
    }
    return { path: d, count: n + 8 };
  }, [value]);
  return (
    <svg role="img" aria-label={label} width={size} height={size} viewBox={`0 0 ${count} ${count}`} shapeRendering="crispEdges" className="rounded-lg bg-white" data-testid="qr-code">
      <rect width={count} height={count} fill="#fff" />
      <path d={path} fill="#000" />
    </svg>
  );
}
