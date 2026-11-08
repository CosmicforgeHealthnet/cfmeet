/**
 * Incoming-call ringtone (CAL-004), synthesized with Web Audio: no audio asset, nothing
 * fetched, nothing that the CSP has to allow. Browsers start audio only after a user gesture
 * on the page; `start()` reports whether sound is actually playing so the UI can say so.
 */

export interface Ringtone {
  start: () => Promise<boolean>;
  stop: () => void;
}

/** Two-tone ring (440 Hz + 480 Hz): 2 s on, 4 s off. */
export function createRingtone(): Ringtone {
  let context: AudioContext | null = null;
  let timer: ReturnType<typeof setInterval> | null = null;
  let gain: GainNode | null = null;

  const burst = () => {
    if (!context || !gain) return;
    const now = context.currentTime;
    for (const frequency of [440, 480]) {
      const oscillator = context.createOscillator();
      oscillator.frequency.value = frequency;
      oscillator.connect(gain);
      oscillator.start(now);
      oscillator.stop(now + 2);
    }
  };

  return {
    async start() {
      if (timer) return context?.state === "running";
      try {
        const Ctor = window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
        if (!Ctor) return false;
        context = new Ctor();
        gain = context.createGain();
        gain.gain.value = 0.08;
        gain.connect(context.destination);
        await context.resume().catch(() => undefined);
        burst();
        timer = setInterval(burst, 6000);
        return context.state === "running";
      } catch {
        return false;
      }
    },
    stop() {
      if (timer) clearInterval(timer);
      timer = null;
      void context?.close().catch(() => undefined);
      context = null;
      gain = null;
    },
  };
}
