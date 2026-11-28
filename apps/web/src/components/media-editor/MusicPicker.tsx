"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowLeft, Check, Loader2, Music2, Pause, Play, Search, Trash2, Upload, X } from "lucide-react";
import { cn } from "@/lib/utils";
import {
  clampFades, clampSelection, defaultMusicSettings, formatClock, MAX_AUDIO_MS, MAX_FADE_MS, resizeSelection, trackTitle, type MusicSettings,
} from "@/lib/media-editor/audio";
import { audioDurationMs, audioPeaks, previewEnvelope } from "@/lib/media-editor/audio-render";
import { deleteTrack, listTracks, saveTrack, searchTracks, touchTrack, type LibraryTrack } from "@/lib/media-editor/music-library";
import Waveform from "./Waveform";

/** The music chosen for a status: the user's own file, the range, volume and fades. */
export type MusicChoice = { file: Blob; title: string; trackMs: number; settings: MusicSettings };

const BARS = 90;
/** Largest audio file accepted (decoded in memory). */
const MAX_FILE_BYTES = 100 * 1024 * 1024;

/**
 * Add music to a status, from the person's OWN audio files only (no music service): pick a file
 * or one of "My music" (tracks used recently, kept on this device in IndexedDB), then choose
 * the part on the audio timeline, its length, volume and fades, and preview it.
 * For videos the original sound's volume is set here too (mixed in the export).
 */
export default function MusicPicker({ initial, onDone, onRemove, onClose, maxMs = MAX_AUDIO_MS, defaultMs, original }: {
  initial?: MusicChoice | null;
  onDone: (choice: MusicChoice) => void;
  /** Shown when music is already attached. */
  onRemove?: () => void;
  onClose: () => void;
  /** Longest selection (60 s for statuses; the video's length for videos). */
  maxMs?: number;
  /** Starting length of a new selection (e.g. the video's length). */
  defaultMs?: number;
  /** Independent original-sound volume (videos). */
  original?: { volume: number; onChange: (v: number) => void; available: boolean };
}) {
  const [track, setTrack] = useState<{ file: Blob; title: string; trackMs: number; peaks: number[] } | null>(null);
  const [settings, setSettings] = useState<MusicSettings | null>(initial?.settings ?? null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [library, setLibrary] = useState<LibraryTrack[] | null>(null);
  const [query, setQuery] = useState("");
  const [playing, setPlaying] = useState(false);
  const [position, setPosition] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const audio = useRef<HTMLAudioElement>(null);
  const stopEnvelope = useRef<(() => void) | null>(null);

  useEffect(() => { void listTracks().then(setLibrary); }, []);
  // Escape closes this sheet only (the editor underneath listens on window too).
  const closeRef = useRef(onClose);
  useEffect(() => { closeRef.current = onClose; });
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      e.preventDefault();
      stopEnvelope.current?.();
      audio.current?.pause();
      closeRef.current();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  const open = async (file: Blob, title: string, known?: number) => {
    setLoading(true);
    setError(null);
    try {
      if (file.size > MAX_FILE_BYTES) throw new Error("big");
      const trackMs = known ?? await audioDurationMs(file);
      if (!(trackMs >= 1000)) throw new Error("short");
      const peaks = await audioPeaks(file, BARS);
      setTrack({ file, title, trackMs, peaks });
      setSettings((s) => (s && initial?.file === file ? s : defaultMusicSettings(trackMs, Math.min(maxMs, defaultMs ?? 30_000))));
      return trackMs;
    } catch (e) {
      setError(e instanceof Error && e.message === "big" ? "This file is too big (at most 100 MB)." : e instanceof Error && e.message === "short" ? "This audio is shorter than a second." : "This audio file can't be played here. Try an MP3, M4A, AAC, OGG or WAV file.");
      return null;
    } finally {
      setLoading(false);
    }
  };

  // Reopen the attached music.
  useEffect(() => {
    if (initial) void Promise.resolve().then(() => open(initial.file, initial.title, initial.trackMs));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const pickFile = async (file: File | undefined) => {
    if (!file) return;
    if (!file.type.startsWith("audio/") && !/\.(mp3|m4a|aac|ogg|oga|opus|wav|flac|webm)$/i.test(file.name)) { setError("Choose an audio file."); return; }
    const title = trackTitle(file.name);
    const ms = await open(file, title);
    if (ms) void saveTrack(file, title, ms).then(() => listTracks().then(setLibrary));
  };

  // Object URL of the open track for the preview player.
  useEffect(() => {
    const el = audio.current;
    if (!el || !track) return;
    const url = URL.createObjectURL(track.file);
    el.src = url;
    return () => { el.pause(); el.removeAttribute("src"); el.load(); URL.revokeObjectURL(url); };
  }, [track]);

  const stop = () => { stopEnvelope.current?.(); stopEnvelope.current = null; audio.current?.pause(); setPlaying(false); };
  const play = () => {
    const el = audio.current;
    if (!el || !settings) return;
    stopEnvelope.current?.();
    stopEnvelope.current = previewEnvelope(el, settings);
    void el.play().then(() => setPlaying(true), () => setPlaying(false));
  };
  useEffect(() => () => stopEnvelope.current?.(), []);
  // Restart the preview's envelope when the settings change mid-play.
  useEffect(() => {
    if (!playing || !settings || !audio.current) return;
    stopEnvelope.current?.();
    stopEnvelope.current = previewEnvelope(audio.current, settings);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings?.startMs, settings?.endMs, settings?.fadeInMs, settings?.fadeOutMs, settings?.volume]);

  const shown = useMemo(() => (library ? searchTracks(library, query) : []), [library, query]);
  const len = settings ? settings.endMs - settings.startMs : 0;
  const maxLen = track ? Math.min(maxMs, track.trackMs) : maxMs;
  const update = (patch: Partial<MusicSettings>) => setSettings((s) => {
    if (!s || !track) return s;
    const next = { ...s, ...patch };
    const r = clampSelection(next, track.trackMs, maxMs, s);
    return { ...next, ...r, ...clampFades(next.fadeInMs, next.fadeOutMs, r.endMs - r.startMs) };
  });

  const done = () => {
    if (!track || !settings) return;
    stop();
    if (library?.some((t) => t.blob === track.file)) void touchTrack(library.find((t) => t.blob === track.file)!.id);
    onDone({ file: track.file, title: track.title, trackMs: track.trackMs, settings });
  };

  return (
    <div className="fixed inset-0 z-[95] flex flex-col bg-neutral-950 text-white" role="dialog" aria-modal="true" aria-label="Add music" data-testid="music-picker">
      <audio ref={audio} preload="auto" onTimeUpdate={(e) => setPosition(e.currentTarget.currentTime * 1000)} onPause={() => setPlaying(false)} />
      <input ref={input} type="file" accept="audio/*,.mp3,.m4a,.aac,.ogg,.opus,.wav,.flac" className="hidden" onChange={(e) => { void pickFile(e.target.files?.[0]); e.target.value = ""; }} />
      <div className="flex items-center gap-2 px-3 pb-2 pt-[max(env(safe-area-inset-top),0.75rem)]">
        <button type="button" aria-label={track ? "Back to my music" : "Close"} onClick={() => { stop(); if (track && !initial) setTrack(null); else onClose(); }}
          className="flex size-11 items-center justify-center rounded-full hover:bg-white/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary">
          {track && !initial ? <ArrowLeft className="size-6" /> : <X className="size-6" />}
        </button>
        <h2 className="min-w-0 flex-1 truncate text-lg font-semibold">{track ? track.title : "Add music"}</h2>
        {track && settings ? (
          <button type="button" onClick={done} data-testid="music-done"
            className="flex h-11 items-center gap-1.5 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white">
            <Check className="size-4" />Done
          </button>
        ) : null}
      </div>

      {error ? <p role="alert" className="mx-4 mb-2 rounded-xl bg-red-600/90 px-4 py-3 text-sm">{error}</p> : null}

      {!track ? (
        <div className="flex min-h-0 flex-1 flex-col gap-3 px-4 pb-[max(env(safe-area-inset-bottom),1rem)]">
          <button type="button" onClick={() => input.current?.click()} disabled={loading} data-testid="music-choose-file"
            className="flex h-14 items-center justify-center gap-2 rounded-2xl bg-primary text-base font-semibold text-primary-foreground disabled:opacity-60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white">
            {loading ? <Loader2 className="size-5 animate-spin" /> : <Upload className="size-5" />}Choose an audio file
          </button>
          <p className="text-xs leading-relaxed text-white/60">
            Use music you own or have the right to share. CF Meet doesn&apos;t provide a music catalogue: &ldquo;My music&rdquo; lists audio files you used recently, kept only on this device.
          </p>
          <label className="relative block">
            <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-white/50" />
            <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search my music" aria-label="Search my music" type="search" enterKeyHint="search"
              className="h-11 w-full rounded-full bg-white/10 pl-9 pr-4 text-base text-white placeholder:text-white/50 focus:outline-none focus:ring-2 focus:ring-primary" />
          </label>
          <h3 className="text-sm font-semibold text-white/80">My music</h3>
          <ul className="min-h-0 flex-1 space-y-1 overflow-y-auto" data-testid="music-library">
            {library === null ? <li className="h-14 animate-pulse rounded-xl bg-white/5" /> : null}
            {library && !shown.length ? <li className="py-6 text-center text-sm text-white/60">{library.length ? "No track matches your search." : "Tracks you add appear here."}</li> : null}
            {shown.map((t) => (
              <li key={t.id} className="flex items-center gap-2">
                <button type="button" onClick={() => void open(t.blob, t.title, t.durationMs)} disabled={loading}
                  className="flex min-w-0 flex-1 items-center gap-3 rounded-xl px-2 py-2 text-left hover:bg-white/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary">
                  <span className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-primary/25 text-primary"><Music2 className="size-5" /></span>
                  <span className="min-w-0 flex-1"><span className="block truncate font-medium">{t.title}</span><span className="block text-xs text-white/60">{formatClock(t.durationMs)}</span></span>
                </button>
                <button type="button" aria-label={`Remove ${t.title} from my music`} onClick={() => void deleteTrack(t.id).then(() => listTracks().then(setLibrary))}
                  className="flex size-10 items-center justify-center rounded-full text-white/60 hover:bg-white/10 hover:text-white focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary">
                  <Trash2 className="size-4" />
                </button>
              </li>
            ))}
          </ul>
        </div>
      ) : settings ? (
        <div className="flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto px-4 pb-[max(env(safe-area-inset-bottom),1rem)]" data-testid="music-trim">
          <div className="flex items-center gap-3">
            <button type="button" aria-label={playing ? "Pause preview" : "Play preview"} onClick={() => (playing ? stop() : play())} data-testid="music-preview"
              className="flex size-14 shrink-0 items-center justify-center rounded-full bg-white text-black focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary">
              {playing ? <Pause className="size-6" /> : <Play className="ml-0.5 size-6" />}
            </button>
            <p className="text-sm text-white/80">
              <span className="block font-semibold tabular-nums text-white">{formatClock(settings.startMs)} – {formatClock(settings.endMs)}</span>
              {formatClock(len)} of {formatClock(track.trackMs)} · drag the window to choose the part
            </p>
          </div>
          <Waveform peaks={track.peaks} totalMs={track.trackMs} selection={settings} maxSelectionMs={maxLen}
            progress={playing ? position / track.trackMs : undefined}
            onSelection={(r) => update(r)} label="Music timeline" className="h-20" />
          <Slider label="Length" value={Math.round(len / 1000)} min={1} max={Math.max(1, Math.floor(maxLen / 1000))} unit="s"
            onChange={(v) => setSettings((s) => (s ? { ...s, ...resizeSelection(s, v * 1000, track.trackMs, maxMs), ...clampFades(s.fadeInMs, s.fadeOutMs, v * 1000) } : s))} />
          <Slider label="Music volume" value={Math.round(settings.volume * 100)} min={0} max={200} step={5} unit="%" onChange={(v) => update({ volume: v / 100 })} />
          {original ? (
            <Slider label="Original sound" value={Math.round(original.volume * 100)} min={0} max={200} step={5} unit="%" disabled={!original.available}
              onChange={(v) => original.onChange(v / 100)} />
          ) : null}
          <Slider label="Fade in" value={settings.fadeInMs / 1000} min={0} max={MAX_FADE_MS / 1000} step={0.5} unit="s" onChange={(v) => update({ fadeInMs: v * 1000 })} />
          <Slider label="Fade out" value={settings.fadeOutMs / 1000} min={0} max={MAX_FADE_MS / 1000} step={0.5} unit="s" onChange={(v) => update({ fadeOutMs: v * 1000 })} />
          {onRemove ? (
            <button type="button" onClick={() => { stop(); onRemove(); }} className="mt-auto flex h-11 items-center justify-center gap-2 rounded-full bg-white/10 text-sm font-semibold hover:bg-white/15 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary">
              <Trash2 className="size-4" />Remove music
            </button>
          ) : null}
        </div>
      ) : (
        <Loader2 className="m-auto size-8 animate-spin text-white/70" aria-label="Opening audio" />
      )}
    </div>
  );
}

function Slider({ label, value, min, max, step = 1, unit, onChange, disabled }: { label: string; value: number; min: number; max: number; step?: number; unit: string; onChange: (v: number) => void; disabled?: boolean }) {
  return (
    <label className={cn("flex items-center gap-3 text-sm", disabled && "opacity-40")}>
      <span className="w-28 shrink-0 text-white/80">{label}</span>
      <input type="range" min={min} max={max} step={step} value={value} disabled={disabled} aria-label={label} onChange={(e) => onChange(Number(e.target.value))}
        className="h-2 min-w-0 flex-1 cursor-pointer accent-primary" />
      <span className="w-12 shrink-0 text-right tabular-nums text-white/80">{value}{unit}</span>
    </label>
  );
}

/** A small chip showing attached music (tap to edit). */
export function MusicChip({ choice, onClick }: { choice: MusicChoice; onClick: () => void }) {
  return (
    <button type="button" onClick={onClick} aria-label={`Music: ${choice.title}. Edit music`} data-testid="music-chip"
      className="flex h-9 max-w-[16rem] items-center gap-1.5 rounded-full bg-black/55 px-3 text-xs font-semibold text-white backdrop-blur focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary">
      <Music2 className="size-4 shrink-0" /><span className="truncate">{choice.title}</span>
      <span className="shrink-0 tabular-nums text-white/70">{formatClock(choice.settings.endMs - choice.settings.startMs)}</span>
    </button>
  );
}
