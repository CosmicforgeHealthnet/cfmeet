"use client";

import { useEffect, useRef, useState } from "react";
import { BarChart3, DoorOpen, Megaphone, X } from "lucide-react";
import { useParticipantsV2 } from "@/hooks/v2";
import type { SidebarPanel } from "./sidebar";

/** In-call banners (Meet / Teams pattern): announcements, new polls, breakout timing. */
export interface RoomNotice {
  id: string;
  kind: "announcement" | "poll" | "breakout";
  title: string;
  body?: string;
  panel?: SidebarPanel;
  action?: string;
}

export const NOTICE_EVENT = "cfmeet:notice";
export const CAPTION_EVENT = "cfmeet:caption";
/** A breakout move/enter/leave changed this person's media room; reconnect to it. */
export const MEDIA_MOVE_EVENT = "cfmeet:media-move";

export const emitNotice = (notice: RoomNotice) => window.dispatchEvent(new CustomEvent(NOTICE_EVENT, { detail: notice }));

const ICONS = { announcement: Megaphone, poll: BarChart3, breakout: DoorOpen };

export function RoomNotices({ onOpenPanel }: { onOpenPanel: (panel: SidebarPanel) => void }) {
  const [notices, setNotices] = useState<RoomNotice[]>([]);
  useEffect(() => {
    const timers = new Set<ReturnType<typeof setTimeout>>();
    const receive = (event: Event) => {
      const notice = (event as CustomEvent<RoomNotice>).detail;
      setNotices((current) => [...current.filter((n) => n.id !== notice.id).slice(-2), notice]);
      const timer = setTimeout(() => setNotices((current) => current.filter((n) => n.id !== notice.id)), notice.kind === "announcement" ? 15000 : 9000);
      timers.add(timer);
    };
    window.addEventListener(NOTICE_EVENT, receive);
    return () => { window.removeEventListener(NOTICE_EVENT, receive); timers.forEach(clearTimeout); };
  }, []);
  const dismiss = (id: string) => setNotices((current) => current.filter((n) => n.id !== id));

  if (!notices.length) return null;
  return (
    <div className="pointer-events-none absolute inset-x-3 top-3 z-40 flex flex-col items-center gap-2 sm:inset-x-auto sm:left-1/2 sm:w-[26rem] sm:-translate-x-1/2">
      {notices.map((notice) => {
        const Icon = ICONS[notice.kind];
        return (
          <div key={notice.id} role="status" className="pointer-events-auto flex w-full items-start gap-3 rounded-2xl bg-[#2D2E31]/95 p-3 pr-2 text-white shadow-2xl ring-1 ring-white/10 backdrop-blur animate-fade-in">
            <span className={`flex size-9 shrink-0 items-center justify-center rounded-xl ${notice.kind === "announcement" ? "bg-amber-400/15 text-amber-300" : "bg-primary/20 text-[#8ab4ff]"}`}><Icon className="size-4" /></span>
            <div className="min-w-0 flex-1 py-0.5">
              <p className="text-sm font-semibold">{notice.title}</p>
              {notice.body && <p className="mt-0.5 whitespace-pre-wrap break-words text-sm text-white/80">{notice.body}</p>}
              {notice.panel && <button type="button" onClick={() => { onOpenPanel(notice.panel!); dismiss(notice.id); }} className="mt-2 h-8 rounded-full bg-primary px-4 text-sm font-semibold hover:bg-primary/90">{notice.action ?? "Open"}</button>}
            </div>
            <button type="button" aria-label="Dismiss" onClick={() => dismiss(notice.id)} className="flex size-8 shrink-0 items-center justify-center rounded-full text-white/60 hover:bg-white/10 hover:text-white"><X className="size-4" /></button>
          </div>
        );
      })}
    </div>
  );
}

interface CaptionLine { key: string; speaker: string; text: string; final: boolean; at: number }

/** Live captions (from transcription caption.segment events), Meet-style at the bottom of the stage. */
export function CaptionsOverlay({ conferenceId, on }: { conferenceId: string; on: boolean }) {
  const participants = useParticipantsV2(conferenceId);
  const names = useRef(new Map<string, string>());
  names.current = new Map((participants.data ?? []).map((p) => [p.id, p.displayName]));
  const [lines, setLines] = useState<CaptionLine[]>([]);

  useEffect(() => {
    if (!on) { setLines([]); return; }
    const receive = (event: Event) => {
      const seg = (event as CustomEvent<{ jobId?: string; sequence?: number; speakerParticipantId?: string; text?: string; final?: boolean; startMs?: number }>).detail;
      if (!seg?.text) return;
      const speaker = (seg.speakerParticipantId && names.current.get(seg.speakerParticipantId)) || "Speaker";
      // Partial and final segments share a start time: update the line in place.
      const key = `${seg.jobId}-${seg.speakerParticipantId}-${seg.startMs ?? seg.sequence}`;
      setLines((current) => {
        const next = current.filter((line) => line.key !== key && Date.now() - line.at < 12000);
        return [...next, { key, speaker, text: seg.text!, final: !!seg.final, at: Date.now() }].slice(-2);
      });
    };
    window.addEventListener(CAPTION_EVENT, receive);
    const sweep = setInterval(() => setLines((current) => current.filter((line) => Date.now() - line.at < 12000)), 2000);
    return () => { window.removeEventListener(CAPTION_EVENT, receive); clearInterval(sweep); };
  }, [on]);

  if (!on) return null;
  return (
    <div aria-live="polite" className="pointer-events-none absolute inset-x-3 bottom-3 z-30 flex justify-center">
      <div className="w-full max-w-3xl space-y-1.5 rounded-2xl bg-black/75 px-4 py-3 text-white backdrop-blur" style={{ display: lines.length ? undefined : "none" }}>
        {lines.map((line) => (
          <p key={line.key} className="text-base leading-snug sm:text-lg"><span className="mr-2 font-semibold text-[#8ab4ff]">{line.speaker}</span>{line.text}</p>
        ))}
      </div>
      {!lines.length && <span className="rounded-full bg-black/60 px-3 py-1.5 text-xs text-white/80">Captions on · waiting for speech</span>}
    </div>
  );
}
