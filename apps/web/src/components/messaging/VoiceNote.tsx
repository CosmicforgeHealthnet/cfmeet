"use client";

/**
 * Voice notes (WhatsApp): record with the microphone, see the timer, cancel, listen back, send.
 * The recording goes through the normal file flow, so private chats encrypt it on this device.
 * The bubble player plays voice notes and other audio inline.
 */
import { useEffect, useRef, useState } from "react";
import { Loader2, Mic, Pause, Play, SendHorizontal, Square, Trash2 } from "lucide-react";
import { cn } from "@/lib/utils";

/** The server's limit for one voice note. */
const MAX_MS = 15 * 60 * 1000;
const TYPES = ["audio/webm;codecs=opus", "audio/mp4", "audio/ogg;codecs=opus", "audio/webm"];

export function voiceRecordingSupported(): boolean {
  return typeof window !== "undefined" && typeof MediaRecorder !== "undefined" && !!navigator.mediaDevices?.getUserMedia;
}

const clock = (ms: number) => {
  const total = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
};

function micError(e: unknown): string {
  const name = e instanceof DOMException ? e.name : "";
  if (name === "NotAllowedError" || name === "SecurityError") return "Allow the microphone for this site to record a voice note.";
  if (name === "NotFoundError" || name === "OverconstrainedError") return "No microphone was found.";
  if (name === "NotReadableError") return "Another app is using the microphone.";
  return "Couldn't start recording.";
}

type Take = { blob: Blob; url: string; durationMs: number };

/**
 * The composer while a voice note is being made. Starts recording when it appears; `onDone`
 * closes it (after sending or cancelling).
 */
export function VoiceRecorder({ onSend, onDone, onError }: {
  onSend: (file: File, durationMs: number) => void;
  onDone: () => void;
  onError: (message: string) => void;
}) {
  const [elapsed, setElapsed] = useState(0);
  const [take, setTake] = useState<Take | null>(null);
  const [starting, setStarting] = useState(true);
  const rec = useRef<{ recorder: MediaRecorder; stream: MediaStream; started: number; chunks: Blob[]; sendNow: boolean; cancelled: boolean } | null>(null);
  const finish = useRef<(t: Take) => void>(() => {});

  useEffect(() => {
    finish.current = (t: Take) => {
      if (rec.current?.sendNow) {
        URL.revokeObjectURL(t.url);
        send(t);
      } else setTake(t);
    };
  });

  useEffect(() => {
    let alive = true;
    let timer: ReturnType<typeof setInterval> | undefined;
    (async () => {
      try {
        const stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true } });
        if (!alive) { stream.getTracks().forEach((t) => t.stop()); return; }
        const mimeType = TYPES.find((t) => MediaRecorder.isTypeSupported(t));
        const recorder = new MediaRecorder(stream, mimeType ? { mimeType, audioBitsPerSecond: 32_000 } : undefined);
        const state = { recorder, stream, started: Date.now(), chunks: [] as Blob[], sendNow: false, cancelled: false };
        rec.current = state;
        recorder.ondataavailable = (e) => { if (e.data.size) state.chunks.push(e.data); };
        recorder.onstop = () => {
          stream.getTracks().forEach((t) => t.stop());
          clearInterval(timer);
          if (state.cancelled || !alive) return;
          const durationMs = Math.min(MAX_MS, Math.max(1, Date.now() - state.started));
          // The server checks the type without codec details ("audio/webm", not "audio/webm;codecs=opus").
          const type = (recorder.mimeType || "audio/webm").split(";")[0];
          const blob = new Blob(state.chunks, { type });
          finish.current({ blob, url: URL.createObjectURL(blob), durationMs });
        };
        recorder.start(250);
        setStarting(false);
        timer = setInterval(() => {
          const ms = Date.now() - state.started;
          setElapsed(ms);
          if (ms >= MAX_MS && recorder.state === "recording") recorder.stop();
        }, 200);
      } catch (e) {
        if (alive) { onError(micError(e)); onDone(); }
      }
    })();
    return () => {
      alive = false;
      clearInterval(timer);
      const r = rec.current;
      if (r && r.recorder.state !== "inactive") { r.cancelled = true; r.recorder.stop(); }
      r?.stream.getTracks().forEach((t) => t.stop());
    };
    // Runs once: a new recording each time the recorder opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => () => { if (take) URL.revokeObjectURL(take.url); }, [take]);

  const stop = (sendNow: boolean) => {
    const r = rec.current;
    if (!r || r.recorder.state === "inactive") return;
    r.sendNow = sendNow;
    r.recorder.stop();
  };
  const cancel = () => {
    const r = rec.current;
    if (r && r.recorder.state !== "inactive") { r.cancelled = true; r.recorder.stop(); }
    onDone();
  };
  function send(t: Take) {
    const ext = t.blob.type === "audio/mp4" ? "m4a" : t.blob.type === "audio/ogg" ? "ogg" : "webm";
    const stamp = new Date().toISOString().slice(0, 19).replace(/[T:]/g, "-");
    onSend(new File([t.blob], `Voice note ${stamp}.${ext}`, { type: t.blob.type }), t.durationMs);
    onDone();
  }

  const icon = "flex size-10 shrink-0 items-center justify-center rounded-full";
  return (
    <div className="flex items-center gap-1.5 rounded-[26px] border border-border bg-card p-1.5 shadow-sm" data-testid="voice-recorder">
      <button type="button" aria-label="Delete voice note" onClick={cancel} className={cn(icon, "text-muted-foreground hover:bg-muted hover:text-destructive")}><Trash2 className="size-5" /></button>
      {take ? (
        <AudioBar src={take.url} knownMs={take.durationMs} mine={false} className="min-w-0 flex-1" />
      ) : (
        <span className="flex min-w-0 flex-1 items-center gap-2 px-1 text-sm" aria-live="polite">
          {starting ? <><Loader2 className="size-4 animate-spin text-muted-foreground" />Starting microphone…</> : (
            <>
              <span className="size-2.5 animate-pulse rounded-full bg-red-500" />
              <span className="tabular-nums font-medium" data-testid="voice-timer">{clock(elapsed)}</span>
              <span className="truncate text-muted-foreground">Recording</span>
            </>
          )}
        </span>
      )}
      {!take ? <button type="button" aria-label="Stop and listen" disabled={starting} onClick={() => stop(false)} className={cn(icon, "text-red-500 hover:bg-muted disabled:opacity-30")}><Square className="size-4 fill-current" /></button> : null}
      <button type="button" aria-label="Send voice note" data-testid="voice-send" disabled={starting} onClick={() => (take ? send(take) : stop(true))}
        className={cn(icon, "bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-30")}><SendHorizontal className="size-5" /></button>
    </div>
  );
}

/** The composer's microphone button (shown while there is no text to send). */
export function MicButton({ disabled, onClick }: { disabled: boolean; onClick: () => void }) {
  return (
    <button type="button" data-testid="voice-start" aria-label="Record a voice note" title="Record a voice note" disabled={disabled} onClick={onClick}
      className="flex size-10 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground transition-opacity hover:bg-primary/90 disabled:opacity-30">
      <Mic className="size-5" />
    </button>
  );
}

/** Play / pause, a seek bar and the time. */
function AudioBar({ src, knownMs, mine, className }: { src: string; knownMs?: number; mine: boolean; className?: string }) {
  const audio = useRef<HTMLAudioElement>(null);
  const [playing, setPlaying] = useState(false);
  const [at, setAt] = useState(0);
  const [length, setLength] = useState(knownMs ? knownMs / 1000 : 0);

  const onMeta = () => {
    const a = audio.current;
    if (!a) return;
    if (Number.isFinite(a.duration) && a.duration > 0) { setLength(a.duration); return; }
    // Recorded webm files often don't state their length: seeking to the end makes the browser find it.
    const fix = () => { a.removeEventListener("timeupdate", fix); if (Number.isFinite(a.duration)) setLength(a.duration); a.currentTime = 0; };
    a.addEventListener("timeupdate", fix);
    a.currentTime = 1e7;
  };
  const toggle = () => {
    const a = audio.current;
    if (!a) return;
    if (a.paused) void a.play().catch(() => setPlaying(false));
    else a.pause();
  };

  return (
    <span className={cn("flex items-center gap-2", className)}>
      <audio ref={audio} src={src} preload="metadata" onLoadedMetadata={onMeta} onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)}
        onEnded={() => { setPlaying(false); setAt(0); }} onTimeUpdate={(e) => { const t = e.currentTarget.currentTime; if (t < 1e6) setAt(t); }} />
      <button type="button" aria-label={playing ? "Pause" : "Play"} onClick={toggle} data-testid="voice-play"
        className={cn("flex size-9 shrink-0 items-center justify-center rounded-full", mine ? "bg-white/20 hover:bg-white/30" : "bg-primary/10 text-primary hover:bg-primary/20")}>
        {playing ? <Pause className="size-4 fill-current" /> : <Play className="ml-0.5 size-4 fill-current" />}
      </button>
      <input type="range" min={0} max={length || 1} step={0.1} value={Math.min(at, length || 1)} aria-label="Position"
        onChange={(e) => { const a = audio.current; if (a) { a.currentTime = Number(e.target.value); setAt(a.currentTime); } }}
        className={cn("h-1 min-w-0 flex-1 cursor-pointer", mine ? "accent-white" : "accent-primary")} />
      <span className="w-10 shrink-0 text-right text-xs tabular-nums opacity-80">{clock((playing || at ? at : length) * 1000)}</span>
    </span>
  );
}

/** A voice note (or other audio) in a chat bubble. */
export function VoicePlayer({ load, mine, onDownload, initial = null }: { load: () => Promise<string | null>; mine: boolean; onDownload: () => void; initial?: string | null }) {
  const [url, setUrl] = useState<string | null>(initial);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (initial) return;
    let active = true;
    load().then((u) => { if (active) { if (u) setUrl(u); else setFailed(true); } }, () => { if (active) setFailed(true); });
    return () => { active = false; };
  }, [load, initial]);

  if (failed) return <button type="button" onClick={onDownload} className="mt-1 text-sm underline opacity-80">Couldn&apos;t play this voice note · Download</button>;
  return (
    <span className="mt-0.5 flex w-64 max-w-full items-center gap-2" data-testid="voice-note">
      <Mic className="size-4 shrink-0 opacity-70" aria-hidden />
      {url ? <AudioBar src={url} mine={mine} className="min-w-0 flex-1" /> : <span className="flex h-9 flex-1 items-center gap-2 text-xs opacity-70"><Loader2 className="size-4 animate-spin" />Loading…</span>}
    </span>
  );
}
