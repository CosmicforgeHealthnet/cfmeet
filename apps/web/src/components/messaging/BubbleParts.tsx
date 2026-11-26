"use client";

/**
 * Pieces of a chat bubble (WhatsApp): links you can tap inside the text, and photos shown in
 * the chat with a full-screen viewer. Photos load through the engine (decrypted on this
 * device in private chats) and only plain raster types are rendered.
 */
import { Fragment, useEffect, useState } from "react";
import { ChevronLeft, ChevronRight, Download, ImageOff, X } from "lucide-react";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";

const URL_RE = /\bhttps?:\/\/[^\s<>"']+[^\s<>"'.,;:!?)\]]/gi;

/** Message text with every http(s) link tappable (opens in a new tab, never with this page's access). */
export function LinkifiedText({ text, mine }: { text: string; mine: boolean }) {
  const parts: React.ReactNode[] = [];
  let last = 0;
  for (const match of text.matchAll(URL_RE)) {
    const at = match.index ?? 0;
    if (at > last) parts.push(text.slice(last, at));
    parts.push(
      <a key={at} href={match[0]} target="_blank" rel="noopener noreferrer nofollow"
        className={cn("break-all underline underline-offset-2", mine ? "decoration-white/60" : "text-primary decoration-primary/40")}
        onClick={(e) => e.stopPropagation()}>{match[0]}</a>,
    );
    last = at + match[0].length;
  }
  if (last < text.length) parts.push(text.slice(last));
  return <>{parts.map((p, i) => <Fragment key={i}>{p}</Fragment>)}</>;
}

/** A photo in the chat: a fixed-size placeholder while it loads, tap to open full screen. */
/** A video in a chat bubble (WhatsApp): loaded when shown, played in place with controls. */
export function ChatVideo({ load, name, onDownload, initial = null }: { load: () => Promise<string | null>; name: string; onDownload: () => void; initial?: string | null }) {
  const [url, setUrl] = useState<string | null>(initial);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    if (initial) return;
    let active = true;
    load().then((u) => { if (active) { if (u) setUrl(u); else setFailed(true); } }, () => { if (active) setFailed(true); });
    return () => { active = false; };
  }, [load, initial]);
  if (failed) {
    return (
      <button type="button" onClick={onDownload} className="mt-1 flex h-40 w-60 max-w-full flex-col items-center justify-center gap-2 rounded-xl bg-black/10 text-sm opacity-80">
        <ImageOff className="size-6" />Couldn&apos;t play this video · Download
      </button>
    );
  }
  return (
    <span className="-mx-2 -mt-0.5 mb-1 block overflow-hidden rounded-[14px] bg-black/80" data-testid="chat-video">
      {url
        ? <video src={url} controls preload="metadata" playsInline aria-label={name} className="max-h-80 w-auto max-w-[min(18rem,70vw)]" />
        : <span className="block h-48 w-60 max-w-[70vw] animate-pulse" />}
    </span>
  );
}

/**
 * WhatsApp's photo viewer for a chat: every photo of the chat, with previous / next
 * (buttons and ← →), download and close (Esc).
 */
export function PhotoViewer({ items, index, onIndex, onClose }: {
  items: { key: string; name: string; load: () => Promise<string | null>; download: () => void }[];
  index: number;
  onIndex: (i: number) => void;
  onClose: () => void;
}) {
  const item = items[index];
  // The loaded photo, remembered with its key: a stale one never shows for the next item.
  const [loaded, setLoaded] = useState<{ key: string; url: string | null } | null>(null);
  const url = loaded && item && loaded.key === item.key ? loaded.url : null;
  useEffect(() => {
    if (!item) return;
    let active = true;
    item.load().then((u) => { if (active) setLoaded({ key: item.key, url: u }); }, () => {});
    return () => { active = false; };
  }, [item]);
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key === "ArrowLeft" && index > 0) onIndex(index - 1);
      if (e.key === "ArrowRight" && index < items.length - 1) onIndex(index + 1);
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [index, items.length, onIndex]);
  if (!item) return null;
  const nav = "absolute top-1/2 z-10 flex size-12 -translate-y-1/2 items-center justify-center rounded-full bg-white/10 text-white hover:bg-white/20 disabled:opacity-0";
  return (
    <Dialog open onOpenChange={(v) => { if (!v) onClose(); }}>
      <DialogContent showCloseButton={false} className="flex h-[100dvh] max-w-none flex-col items-center justify-center gap-0 rounded-none border-0 bg-black/95 p-0 sm:max-w-none" data-testid="photo-viewer">
        <DialogTitle className="sr-only">{item.name}</DialogTitle>
        <div className="absolute inset-x-0 top-0 z-10 flex items-center justify-between p-3 text-white">
          <span className="truncate px-2 text-sm opacity-80">{item.name} · {index + 1} of {items.length}</span>
          <span className="flex gap-1">
            <button type="button" aria-label="Download photo" onClick={item.download} className="flex size-10 items-center justify-center rounded-full hover:bg-white/10"><Download className="size-5" /></button>
            <button type="button" aria-label="Close" onClick={onClose} className="flex size-10 items-center justify-center rounded-full hover:bg-white/10"><X className="size-5" /></button>
          </span>
        </div>
        <button type="button" aria-label="Previous photo" disabled={index === 0} onClick={() => onIndex(index - 1)} className={cn(nav, "left-3")}><ChevronLeft className="size-6" /></button>
        <button type="button" aria-label="Next photo" disabled={index === items.length - 1} onClick={() => onIndex(index + 1)} className={cn(nav, "right-3")}><ChevronRight className="size-6" /></button>
        {/* eslint-disable-next-line @next/next/no-img-element */}
        {url ? <img src={url} alt={item.name} className="max-h-[100dvh] max-w-full object-contain" /> : <span className="size-10 animate-pulse rounded-full bg-white/20" />}
      </DialogContent>
    </Dialog>
  );
}

export function ChatImage({ load, name, onDownload, initial = null, onOpen }: { load: () => Promise<string | null>; name: string; onDownload: () => void; initial?: string | null; onOpen?: () => void }) {
  const [url, setUrl] = useState<string | null>(initial);
  const [failed, setFailed] = useState(false);
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (initial) return;
    let active = true;
    load().then((u) => { if (active) { if (u) setUrl(u); else setFailed(true); } }, () => { if (active) setFailed(true); });
    return () => { active = false; };
  }, [load, initial]);

  if (failed) {
    return (
      <button type="button" onClick={onDownload} className="mt-1 flex h-40 w-60 max-w-full flex-col items-center justify-center gap-2 rounded-xl bg-black/10 text-sm opacity-80">
        <ImageOff className="size-6" />Couldn&apos;t show this photo · Download
      </button>
    );
  }
  return (
    <>
      <button type="button" aria-label={`Open photo ${name}`} disabled={!url} onClick={() => (onOpen ? onOpen() : setOpen(true))} data-testid="chat-image"
        className="-mx-2 -mt-0.5 mb-1 block overflow-hidden rounded-[14px] bg-black/10">
        {url
          // eslint-disable-next-line @next/next/no-img-element
          ? <img src={url} alt="" className="max-h-80 w-auto max-w-[min(18rem,70vw)] object-cover" />
          : <span className="block h-48 w-60 max-w-[70vw] animate-pulse" />}
      </button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent showCloseButton={false} className="flex h-[100dvh] max-w-none flex-col items-center justify-center gap-0 rounded-none border-0 bg-black/95 p-0 sm:max-w-none">
          <DialogTitle className="sr-only">{name}</DialogTitle>
          <div className="absolute inset-x-0 top-0 z-10 flex items-center justify-between p-3 text-white">
            <span className="truncate px-2 text-sm opacity-80">{name}</span>
            <span className="flex gap-1">
              <button type="button" aria-label="Download photo" onClick={onDownload} className="flex size-10 items-center justify-center rounded-full hover:bg-white/10"><Download className="size-5" /></button>
              <button type="button" aria-label="Close" onClick={() => setOpen(false)} className="flex size-10 items-center justify-center rounded-full hover:bg-white/10"><X className="size-5" /></button>
            </span>
          </div>
          {/* eslint-disable-next-line @next/next/no-img-element */}
          {url ? <img src={url} alt={name} className="max-h-[100dvh] max-w-full object-contain" /> : null}
        </DialogContent>
      </Dialog>
    </>
  );
}
