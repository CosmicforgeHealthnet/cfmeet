"use client";

import { useEffect, useRef, useState } from "react";
import { MicOff, X } from "lucide-react";

/**
 * "You're muted" nudge (Meet / Zoom / Teams pattern). While the call mic is OFF, the mic level
 * is measured on this device only (a separate local stream that is never published) and a
 * prompt offers to unmute when the person starts talking. Throttled so it never nags.
 */
export default function MutedSpeechPrompt({ muted, onUnmute, deviceId }: { muted: boolean; onUnmute: () => void; deviceId?: string }) {
  const [visible, setVisible] = useState(false);
  const quietUntil = useRef(0);

  useEffect(() => {
    if (!muted) {
      setVisible(false);
      return;
    }
    let cancelled = false;
    let stream: MediaStream | null = null;
    let ctx: AudioContext | null = null;
    let timer: ReturnType<typeof setInterval> | undefined;
    let loudSince = 0;

    navigator.mediaDevices
      .getUserMedia({ audio: deviceId && deviceId !== "default" ? { deviceId: { exact: deviceId } } : true })
      .then((s) => {
        if (cancelled) return s.getTracks().forEach((t) => t.stop());
        stream = s;
        ctx = new AudioContext();
        void ctx.resume().catch(() => undefined);
        const analyser = ctx.createAnalyser();
        analyser.fftSize = 256;
        analyser.smoothingTimeConstant = 0.5;
        ctx.createMediaStreamSource(s).connect(analyser);
        const data = new Uint8Array(analyser.fftSize);
        timer = setInterval(() => {
          analyser.getByteTimeDomainData(data);
          let peak = 0;
          for (let i = 0; i < data.length; i++) peak = Math.max(peak, Math.abs(data[i] - 128));
          const now = Date.now();
          if (peak / 128 > 0.08) {
            if (!loudSince) loudSince = now;
            // Sustained speech (~0.5s), not a cough or a keyboard click.
            if (now - loudSince > 500 && now > quietUntil.current) {
              setVisible(true);
              quietUntil.current = now + 30_000;
            }
          } else {
            loudSince = 0;
          }
        }, 100);
      })
      .catch(() => undefined); // No mic access: simply no prompt.

    return () => {
      cancelled = true;
      if (timer) clearInterval(timer);
      stream?.getTracks().forEach((t) => t.stop());
      void ctx?.close().catch(() => undefined);
    };
  }, [muted, deviceId]);

  // Auto-hide after a few seconds.
  useEffect(() => {
    if (!visible) return;
    const t = setTimeout(() => setVisible(false), 6000);
    return () => clearTimeout(t);
  }, [visible]);

  if (!visible || !muted) return null;
  return (
    <div role="status" aria-live="polite" className="absolute bottom-full left-1/2 z-30 mb-3 flex -translate-x-1/2 items-center gap-3 rounded-full bg-slate-800 py-2 pl-3 pr-2 text-sm text-white shadow-2xl ring-1 ring-white/10">
      <span className="flex size-8 items-center justify-center rounded-full bg-destructive"><MicOff className="size-4" /></span>
      <span className="whitespace-nowrap font-medium">You&apos;re muted</span>
      <button type="button" onClick={() => { setVisible(false); onUnmute(); }} className="h-8 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90">Unmute</button>
      <button type="button" aria-label="Dismiss" onClick={() => { setVisible(false); quietUntil.current = Date.now() + 120_000; }} className="flex size-8 items-center justify-center rounded-full text-white/70 hover:bg-white/10 hover:text-white"><X className="size-4" /></button>
    </div>
  );
}
