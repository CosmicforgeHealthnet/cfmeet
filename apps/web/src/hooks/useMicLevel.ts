"use client";

import { useState, useEffect, useRef } from "react";

export function useMicLevel(isMicOn: boolean, stream: MediaStream | null) {
  const [rawLevel, setRawLevel] = useState(0);

  const audioContextRef = useRef<AudioContext | null>(null);
  const analyserRef = useRef<AnalyserNode | null>(null);
  const animFrameRef = useRef<number>(0);
  const levelRef = useRef(0);

  // Sync with external audio API - legitimate use of setState in effect
  useEffect(() => {
    if (!isMicOn || !stream) {
      levelRef.current = 0;
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setRawLevel(0);
      return;
    }

    let cancelled = false;

    const setupAnalyser = async () => {
      // Stream is passed directly now
      if (cancelled) return;

      const audioContext = new AudioContext();
      const analyser = audioContext.createAnalyser();
      analyser.fftSize = 256;
      analyser.smoothingTimeConstant = 0.5;

      const source = audioContext.createMediaStreamSource(stream);
      source.connect(analyser);

      audioContextRef.current = audioContext;
      analyserRef.current = analyser;

      const dataArray = new Uint8Array(analyser.fftSize);

      const tick = () => {
        if (cancelled) return;
        analyser.getByteTimeDomainData(dataArray);
        let peak = 0;
        for (let i = 0; i < dataArray.length; i++) {
          const deviation = Math.abs(dataArray[i] - 128);
          if (deviation > peak) peak = deviation;
        }
        const raw = peak / 128;
        const normalized = Math.min(1, Math.pow(raw * 4, 0.5));
        if (Math.abs(normalized - levelRef.current) > 0.02) {
          levelRef.current = normalized;
          setRawLevel(normalized);
        }
        animFrameRef.current = requestAnimationFrame(tick);
      };

      tick();
    };

    setupAnalyser();

    return () => {
      cancelled = true;
      cancelAnimationFrame(animFrameRef.current);
      if (audioContextRef.current) {
        audioContextRef.current.close();
        audioContextRef.current = null;
      }
      analyserRef.current = null;
    };
  }, [isMicOn, stream]);

  // Derive final value: if mic is off, always return 0
  return isMicOn ? rawLevel : 0;
}
