"use client";

import { useRef, useState } from "react";
import { BarChart3, CalendarClock, FileText, ImagePlus, Link2, Mic, Plus, Send, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { durationOf } from "@/services/v2/social.service";
import { channelsService, type PostBody } from "@/services/v2/channels.service";
import { firstLink, MEDIA_LIMITS, mediaKindOf, presentationsFor, scheduleBounds } from "@/lib/channels";
import { errorText } from "@/components/updates/shared";

/**
 * The composer of a channel's owner and admins who can post: words, a photo / video / voice
 * note / audio / document (with GIF, sticker and voice presentations), a link preview the
 * device builds from the first link (the server never fetches links), a poll, or a
 * scheduled time.
 */
export default function ChannelComposer({ channelId, onPosted }: { channelId: string; onPosted: (scheduled: boolean) => void }) {
  const [text, setText] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [presentation, setPresentation] = useState<"" | "VOICE" | "GIF" | "STICKER">("");
  const [preview, setPreview] = useState<{ on: boolean; title: string; description: string; siteName: string }>({ on: true, title: "", description: "", siteName: "" });
  const [poll, setPoll] = useState<{ question: string; options: string[]; multiple: boolean } | null>(null);
  const [when, setWhen] = useState<string>("");
  const [more, setMore] = useState(false);
  const [picking, setPicking] = useState(false);
  const [busy, setBusy] = useState<number | null>(null);
  const picker = useRef<HTMLInputElement>(null);
  const link = !file && !poll ? firstLink(text) : null;
  const options = file ? presentationsFor(file.type, file.size) : [];
  const ready = poll ? poll.question.trim() && poll.options.filter((o) => o.trim()).length >= 2 : !!text.trim() || !!file;

  const reset = () => {
    setText(""); setFile(null); setPresentation(""); setPoll(null); setWhen(""); setPicking(false);
    setPreview({ on: true, title: "", description: "", siteName: "" });
  };

  const send = async () => {
    if (busy !== null || !ready) return;
    setBusy(0);
    try {
      const body: PostBody = {};
      if (poll) {
        body.poll = { question: poll.question.trim(), options: poll.options.map((o) => o.trim()).filter(Boolean), multiple: poll.multiple };
      } else {
        if (text.trim()) body.text = text.trim();
        if (file) {
          const kind = mediaKindOf(file.type);
          if (!kind) throw new Error("This type of file isn't supported.");
          if (file.size > MEDIA_LIMITS[kind]) throw new Error(`Files of this kind can be at most ${MEDIA_LIMITS[kind] / (1024 * 1024)} MB.`);
          const duration = kind === "VIDEO" || kind === "AUDIO" ? await durationOf(file).catch(() => null) : null;
          if (kind === "VIDEO" && duration === null) throw new Error("This video can't be read.");
          body.mediaId = (await channelsService.upload(file, kind, duration, (p) => setBusy(p))).mediaId;
          if (presentation) body.presentation = presentation;
        }
        if (link && preview.on) {
          body.linkPreview = {
            url: link,
            title: preview.title.trim() || undefined,
            description: preview.description.trim() || undefined,
            siteName: preview.siteName.trim() || (() => { try { return new URL(link).hostname; } catch { return undefined; } })(),
          };
        }
      }
      if (when) body.publishAt = new Date(when).toISOString();
      await channelsService.post(channelId, body);
      toast(when ? "Scheduled" : "Posted");
      reset();
      onPosted(!!when);
    } catch (e) {
      toast(e instanceof Error && !("response" in e) ? e.message : errorText(e, "Your post couldn't be sent."), "error");
    } finally {
      setBusy(null);
    }
  };

  const bounds = scheduleBounds(new Date());
  return (
    <div className="border-t border-border bg-background p-2 pb-[max(env(safe-area-inset-bottom),0.5rem)]" data-testid="channel-composer">
      <div className="mx-auto max-w-2xl space-y-2">
        {file ? (
          <div className="flex flex-wrap items-center gap-2 rounded-2xl bg-muted px-3 py-2 text-sm">
            {file.type.startsWith("audio/") ? <Mic className="size-4" /> : mediaKindOf(file.type) === "DOCUMENT" ? <FileText className="size-4" /> : <ImagePlus className="size-4" />}
            <span className="min-w-0 flex-1 truncate">{file.name}</span>
            {options.length ? (
              <select aria-label="Post as" value={presentation} onChange={(e) => setPresentation(e.target.value as typeof presentation)} className="h-8 rounded-lg border border-border bg-background px-2 text-xs">
                <option value="">As file</option>
                {options.map((o) => <option key={o} value={o}>{o === "VOICE" ? "Voice message" : o === "GIF" ? "GIF" : "Sticker"}</option>)}
              </select>
            ) : null}
            <button type="button" aria-label="Remove file" onClick={() => { setFile(null); setPresentation(""); }} className="flex size-7 items-center justify-center rounded-full hover:bg-background"><X className="size-4" /></button>
          </div>
        ) : null}
        {link ? (
          <div className="rounded-2xl bg-muted px-3 py-2 text-sm">
            <label className="flex items-center gap-2"><Link2 className="size-4" /><span className="min-w-0 flex-1 truncate">{link}</span>
              <input type="checkbox" className="accent-primary" checked={preview.on} onChange={(e) => setPreview({ ...preview, on: e.target.checked })} aria-label="Show a link preview" />
            </label>
            {preview.on ? (
              <div className="mt-2 grid gap-1.5">
                <input value={preview.title} onChange={(e) => setPreview({ ...preview, title: e.target.value })} maxLength={200} placeholder="Preview title (optional)" className="h-9 rounded-lg border border-border bg-background px-2 text-sm" />
                <input value={preview.description} onChange={(e) => setPreview({ ...preview, description: e.target.value })} maxLength={500} placeholder="Preview description (optional)" className="h-9 rounded-lg border border-border bg-background px-2 text-sm" />
              </div>
            ) : null}
          </div>
        ) : null}
        {poll ? (
          <div className="space-y-1.5 rounded-2xl bg-muted p-3" data-testid="poll-builder">
            <div className="flex items-center gap-2">
              <input value={poll.question} onChange={(e) => setPoll({ ...poll, question: e.target.value })} maxLength={300} placeholder="Ask a question" className="h-10 flex-1 rounded-lg border border-border bg-background px-2 text-sm" />
              <button type="button" aria-label="Remove poll" onClick={() => setPoll(null)} className="flex size-8 items-center justify-center rounded-full hover:bg-background"><X className="size-4" /></button>
            </div>
            {poll.options.map((o, i) => (
              <input key={i} value={o} onChange={(e) => setPoll({ ...poll, options: poll.options.map((x, j) => (j === i ? e.target.value : x)) })} maxLength={100} placeholder={`Option ${i + 1}`} className="h-9 w-full rounded-lg border border-border bg-background px-2 text-sm" />
            ))}
            <div className="flex items-center gap-3">
              {poll.options.length < 12 ? <button type="button" onClick={() => setPoll({ ...poll, options: [...poll.options, ""] })} className="flex items-center gap-1 text-sm font-semibold text-primary"><Plus className="size-4" />Add option</button> : null}
              <label className="ml-auto flex items-center gap-2 text-sm"><input type="checkbox" className="accent-primary" checked={poll.multiple} onChange={(e) => setPoll({ ...poll, multiple: e.target.checked })} />Allow multiple answers</label>
            </div>
          </div>
        ) : null}
        {picking ? (
          <label className="flex items-center gap-2 rounded-2xl bg-muted px-3 py-2 text-sm">
            <CalendarClock className="size-4" /><span>Publish at</span>
            <input type="datetime-local" min={bounds.min} max={bounds.max} value={when} onChange={(e) => setWhen(e.target.value)} className="h-9 flex-1 rounded-lg border border-border bg-background px-2" aria-label="Schedule for" data-testid="schedule-input" />
            <button type="button" onClick={() => setPicking(false)} className="font-semibold text-primary">Done</button>
          </label>
        ) : null}
        {when && !picking ? (
          <p className="flex items-center gap-2 px-1 text-xs text-muted-foreground"><CalendarClock className="size-4" />Will be published {new Date(when).toLocaleString()}
            <button type="button" onClick={() => setWhen("")} className="font-semibold text-primary">Post now instead</button></p>
        ) : null}
        <form className="flex items-end gap-1.5 rounded-[26px] border border-border bg-card p-1.5" onSubmit={(e) => { e.preventDefault(); void send(); }}>
          <input ref={picker} type="file" className="hidden"
            accept="image/*,video/*,audio/*,application/pdf,.doc,.docx,.xls,.xlsx,.ppt,.pptx,.odt,.ods,.epub,.zip,.txt,.csv"
            onChange={(e) => { setFile(e.target.files?.[0] ?? null); setPresentation(""); setPoll(null); e.target.value = ""; }} />
          <div className="relative">
            <button type="button" aria-label="Attach" onClick={() => setMore(!more)} className="flex size-10 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><Plus className="size-5" /></button>
            {more ? (
              <div className="absolute bottom-12 left-0 z-20 w-56 rounded-2xl border border-border bg-popover p-1 shadow-xl">
                <button type="button" onClick={() => { setMore(false); picker.current?.click(); }} className="flex w-full items-center gap-2 rounded-xl px-3 py-2.5 text-sm hover:bg-muted"><ImagePlus className="size-4" />Photo, video, audio or file</button>
                <button type="button" onClick={() => { setMore(false); setFile(null); setPoll({ question: "", options: ["", ""], multiple: false }); }} className="flex w-full items-center gap-2 rounded-xl px-3 py-2.5 text-sm hover:bg-muted"><BarChart3 className="size-4" />Poll</button>
                <button type="button" onClick={() => { setMore(false); setPicking(true); }} className="flex w-full items-center gap-2 rounded-xl px-3 py-2.5 text-sm hover:bg-muted"><CalendarClock className="size-4" />Schedule</button>
              </div>
            ) : null}
          </div>
          {!poll ? (
            <textarea value={text} onChange={(e) => setText(e.target.value)} maxLength={4000} rows={1} placeholder={file ? "Add a caption…" : "Write an update"} data-testid="channel-post-text"
              onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && window.matchMedia("(hover: hover)").matches) { e.preventDefault(); void send(); } }}
              className="max-h-32 min-h-10 min-w-0 flex-1 resize-none bg-transparent px-2 py-2 text-[15px] focus:outline-none" />
          ) : <span className="flex-1 px-2 py-2 text-sm text-muted-foreground">Poll</span>}
          <button type="submit" aria-label={when ? "Schedule" : "Post"} disabled={busy !== null || !ready}
            className={cn("flex size-10 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground disabled:opacity-50")} data-testid="channel-post-send">
            {busy !== null ? <span className="text-[10px] font-bold">{Math.round(busy * 100)}%</span> : when ? <CalendarClock className="size-5" /> : <Send className="size-5" />}
          </button>
        </form>
      </div>
    </div>
  );
}
