"use client";

import { useEffect, useRef, useState, type ReactNode } from "react";
import { ImagePlus, Loader2, Music2, Palette, Pause, Play, Send, Trash2, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import MusicPicker, { MusicChip, type MusicChoice } from "@/components/media-editor/MusicPicker";
import { formatClock } from "@/lib/media-editor/audio";
import { previewEnvelope, renderAudioClip } from "@/lib/media-editor/audio-render";
import { decodePhoto, exportPhoto, initialPhotoState } from "@/lib/media-editor/photo";
import { GRADIENT_BACKGROUNDS, gradientCss, SOLID_BACKGROUNDS, type Gradient } from "@/lib/media-editor/text-status";
import type { StatusItemInput } from "@/lib/status-publisher/types";

type Background = { solid: string } | { gradient: Gradient };
const BACKGROUNDS: Background[] = [...GRADIENT_BACKGROUNDS.map((gradient) => ({ gradient })), ...SOLID_BACKGROUNDS.map((solid) => ({ solid }))];
const css = (b: Background) => ("gradient" in b ? gradientCss(b.gradient) : b.solid);

/**
 * A music status: the person's own audio (picked in MusicPicker: part, length, volume, fades),
 * shown on a colour / gradient (posted as an AUDIO status with its title) or over a cover photo
 * of theirs (posted as a photo status with the music attached).
 */
export default function MusicStatus({ audienceButton, captionField, caption, onClose, onPost }: {
  audienceButton: ReactNode;
  captionField: ReactNode;
  caption: string;
  onClose: () => void;
  onPost: (item: StatusItemInput) => void;
}) {
  const [music, setMusic] = useState<MusicChoice | null>(null);
  const [picking, setPicking] = useState(true);
  const [bg, setBg] = useState(0);
  const [cover, setCover] = useState<{ file: File; url: string } | null>(null);
  const [playing, setPlaying] = useState(false);
  const [busy, setBusy] = useState(false);
  const coverInput = useRef<HTMLInputElement>(null);
  const audio = useRef<HTMLAudioElement>(null);
  const stopEnv = useRef<(() => void) | null>(null);

  useEffect(() => () => { if (cover) URL.revokeObjectURL(cover.url); }, [cover]);
  useEffect(() => {
    const el = audio.current;
    if (!el || !music) return;
    const url = URL.createObjectURL(music.file);
    el.src = url;
    return () => { stopEnv.current?.(); el.pause(); el.removeAttribute("src"); el.load(); URL.revokeObjectURL(url); };
  }, [music]);

  const toggle = () => {
    const el = audio.current;
    if (!el || !music) return;
    if (playing) { stopEnv.current?.(); el.pause(); setPlaying(false); return; }
    stopEnv.current = previewEnvelope(el, music.settings);
    void el.play().then(() => setPlaying(true), () => setPlaying(false));
  };

  const post = async () => {
    if (!music || busy) return;
    setBusy(true);
    audio.current?.pause();
    try {
      const clip = await renderAudioClip(music.file, music.settings);
      const text = caption.trim() || undefined;
      if (cover) {
        const src = await decodePhoto(cover.file);
        try {
          const photo = await exportPhoto(src, initialPhotoState());
          onPost({ kind: "IMAGE", blob: photo.blob, preview: photo.blob, caption: text, music: { ...clip, title: music.title } });
        } finally {
          src.close();
        }
      } else {
        const b = BACKGROUNDS[bg];
        onPost({
          kind: "AUDIO", blob: clip.blob, durationMs: clip.durationMs, caption: text, musicTitle: music.title,
          ...("gradient" in b ? { backgroundGradient: b.gradient, background: b.gradient.from } : { background: b.solid }),
        });
      }
    } catch {
      toast("The music couldn't be prepared. Try again.", "error");
    } finally {
      setBusy(false);
    }
  };

  if (picking || !music) {
    return (
      <MusicPicker initial={music} onClose={() => { if (music) setPicking(false); else onClose(); }}
        onDone={(c) => { setMusic(c); setPicking(false); }} />
    );
  }

  return (
    <div className="fixed inset-0 z-[80] flex flex-col text-white" role="dialog" aria-modal="true" aria-label="Music status" data-testid="music-status"
      style={cover ? { backgroundColor: "#000" } : { background: css(BACKGROUNDS[bg]) }}>
      <audio ref={audio} preload="auto" onPause={() => setPlaying(false)} />
      <input ref={coverInput} type="file" accept="image/*" className="hidden"
        onChange={(e) => { const f = e.target.files?.[0]; e.target.value = ""; if (f) setCover({ file: f, url: URL.createObjectURL(f) }); }} />
      {cover ? (
        // eslint-disable-next-line @next/next/no-img-element
        <img src={cover.url} alt="" className="absolute inset-0 size-full object-contain" />
      ) : null}
      <div className="relative z-10 flex items-center justify-between gap-2 p-3 pt-[max(env(safe-area-inset-top),0.75rem)]">
        <button type="button" aria-label="Close" onClick={onClose} className="flex size-11 items-center justify-center rounded-full bg-black/30 backdrop-blur"><X className="size-6" /></button>
        <div className="flex items-center gap-2">
          {cover ? (
            <button type="button" aria-label="Remove cover photo" onClick={() => setCover(null)} className="flex size-11 items-center justify-center rounded-full bg-black/30 backdrop-blur"><Trash2 className="size-5" /></button>
          ) : (
            <button type="button" aria-label="Background" onClick={() => setBg((bg + 1) % BACKGROUNDS.length)} className="flex size-11 items-center justify-center rounded-full bg-black/30 backdrop-blur"><Palette className="size-5" /></button>
          )}
          <button type="button" aria-label={cover ? "Change cover photo" : "Add a cover photo"} onClick={() => coverInput.current?.click()} className="flex size-11 items-center justify-center rounded-full bg-black/30 backdrop-blur"><ImagePlus className="size-5" /></button>
        </div>
      </div>

      <div className="relative z-10 flex flex-1 flex-col items-center justify-center gap-5 px-6">
        {!cover ? <span className="flex size-28 items-center justify-center rounded-full bg-black/25"><Music2 className={cn("size-12", playing && "animate-pulse")} /></span> : null}
        <div className="flex items-center gap-3 rounded-full bg-black/40 p-1.5 pr-4 backdrop-blur">
          <button type="button" aria-label={playing ? "Pause preview" : "Play preview"} onClick={toggle} className="flex size-11 items-center justify-center rounded-full bg-white text-black">
            {playing ? <Pause className="size-5" /> : <Play className="ml-0.5 size-5" />}
          </button>
          <MusicChip choice={music} onClick={() => { audio.current?.pause(); setPicking(true); }} />
        </div>
        <p className="text-xs text-white/80">{formatClock(music.settings.endMs - music.settings.startMs)} · {cover ? "posted as a photo with music" : "posted as a music status"}</p>
      </div>

      <div className="relative z-10 space-y-2 bg-gradient-to-t from-black/70 to-transparent p-3 pb-[max(env(safe-area-inset-bottom),0.75rem)]">
        <div className="flex justify-end">{audienceButton}</div>
        <div className="flex items-center gap-2">
          {captionField}
          <button type="button" aria-label="Post music status" onClick={() => void post()} disabled={busy} data-testid="music-status-post"
            className="flex size-14 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-lg disabled:opacity-60">
            {busy ? <Loader2 className="size-6 animate-spin" /> : <Send className="size-6" />}
          </button>
        </div>
      </div>
    </div>
  );
}
