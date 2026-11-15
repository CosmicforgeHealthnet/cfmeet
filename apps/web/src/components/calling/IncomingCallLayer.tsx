"use client";

/**
 * Global incoming-call experience (CAL-004), rendered on every signed-in page by the calling
 * provider: caller identity, ringtone, tab title, a system notification while the tab is in
 * the background (only after the user allowed notifications), Accept / Decline.
 *
 * Truthful scope: this rings while a CF Meet tab is open (the signaling WebSocket is live).
 * A closed browser is not woken: Web Push / APNs / FCM delivery is EXTERNAL_BLOCKED (CAL-003).
 * Decline ends this device's offer only (CAL-018, owner decision pending).
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { BellRing, Phone, PhoneOff, Video, VolumeX } from "lucide-react";
import { useCalling } from "@/context/CallingContext";
import { useCall } from "@/hooks/calling/useCalls";
import { usePrincipalNames } from "@/hooks/calling/usePrincipalNames";
import { callingErrorMessage } from "@/lib/calling/errors";
import { callerLabel, callerPrincipal, offerHeadline, ringingOffers } from "@/lib/calling/incoming-call";
import { createRingtone } from "@/lib/calling/ringtone";
import type { RecoveredInvitation } from "@/types/calling";

const COLORS = ["#2563eb", "#059669", "#d97706", "#7c3aed", "#db2777", "#0891b2", "#4f46e5", "#ea580c"];
const initialsOf = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";

export default function IncomingCallLayer() {
  const calling = useCalling();
  const [now, setNow] = useState(() => Date.now());
  const [dismissed] = useState(() => new Set<string>());
  const offers = useMemo(
    () => (calling ? ringingOffers(calling.device.invitations, now, dismissed) : []),
    [calling, now, dismissed],
  );
  const offer = offers[0];

  useEffect(() => {
    if (!offers.length) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [offers.length]);

  if (!calling || !offer) return null;
  return <IncomingCallCard key={offer.invitationId} offer={offer} more={offers.length - 1} onDone={() => { dismissed.add(offer.invitationId); setNow(Date.now()); }} />;
}

function IncomingCallCard({ offer, more, onDone }: { offer: RecoveredInvitation; more: number; onDone: () => void }) {
  const calling = useCalling()!;
  const call = useCall(offer.callId);
  const caller = call.data ? callerPrincipal(call.data.participants, calling.principalId) : undefined;
  const names = usePrincipalNames(caller && !caller.displayName ? [caller.principalId] : []);
  const label = callerLabel(call.data?.participants, calling.principalId, names.data);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [silenced, setSilenced] = useState(false);
  const [soundBlocked, setSoundBlocked] = useState(false);
  const ringtone = useRef(createRingtone());
  const headline = offerHeadline(offer);

  // Ringtone (stops on answer, decline, silence, expiry or unmount).
  useEffect(() => {
    const tone = ringtone.current;
    if (silenced) { tone.stop(); return; }
    void tone.start().then((playing) => setSoundBlocked(!playing));
    return () => tone.stop();
  }, [silenced]);

  // Tab title and a background notification (only when the user already allowed them).
  useEffect(() => {
    const title = document.title;
    document.title = `📞 ${headline} — ${label}`;
    let notification: Notification | undefined;
    if (typeof Notification !== "undefined" && Notification.permission === "granted" && document.visibilityState === "hidden") {
      notification = new Notification(headline, { body: `${label} is calling`, tag: `cfmeet-call-${offer.invitationId}`, requireInteraction: true });
      notification.onclick = () => { window.focus(); notification?.close(); };
    }
    return () => { document.title = title; notification?.close(); };
  }, [headline, label, offer.invitationId]);

  const respond = async (accept: boolean) => {
    setBusy(true); setError(null);
    try {
      ringtone.current.stop();
      await calling.answer(offer, accept);
      onDone();
    } catch (e) {
      setError(callingErrorMessage(e));
      setBusy(false);
    }
  };

  const video = offer.callKind === "VIDEO";
  const color = COLORS[([...label].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % COLORS.length];
  // Full-screen incoming call (WhatsApp pattern): who is calling in the middle, Decline / Accept at the bottom.
  return (
    <div role="alertdialog" aria-live="assertive" aria-labelledby="incoming-call-caller" aria-describedby="incoming-call-title"
      className="dark fixed inset-0 z-[70] flex flex-col overflow-hidden bg-[#202124] text-white">
      <div aria-hidden className="pointer-events-none absolute left-1/2 top-[40%] size-[30rem] -translate-x-1/2 -translate-y-1/2 rounded-full opacity-25 blur-[110px]" style={{ backgroundColor: color }} />
      <header className="relative z-10 flex items-center justify-end p-4 sm:p-6">
        <button type="button" aria-label={silenced ? "Ringtone silenced" : "Silence ringtone"} title="Silence" onClick={() => setSilenced(true)} disabled={silenced}
          className="flex size-11 items-center justify-center rounded-full text-white/80 hover:bg-white/10 disabled:opacity-40"><VolumeX className="size-5" /></button>
      </header>
      <div className="relative z-10 flex flex-1 flex-col items-center justify-center px-6 text-center">
        <p id="incoming-call-title" className="flex items-center gap-2 text-sm font-medium text-white/60">{video ? <Video className="size-4" /> : <Phone className="size-4" />}{headline}</p>
        <h2 id="incoming-call-caller" className="mt-2 max-w-md truncate text-3xl font-semibold tracking-tight">{label}</h2>
        <div className="relative mt-12 flex items-center justify-center">
          {[0, 1, 2].map((i) => (
            <span key={i} aria-hidden className="absolute size-40 animate-ping rounded-full" style={{ backgroundColor: color, opacity: 0.18, animationDuration: "2.4s", animationDelay: `${i * 0.8}s` }} />
          ))}
          <span className="relative flex size-40 items-center justify-center rounded-full text-5xl font-semibold shadow-2xl" style={{ backgroundColor: color }}>{initialsOf(label)}</span>
        </div>
        <div className="mt-10 max-w-sm space-y-2 text-xs text-white/60">
          {error ? <p role="alert" className="rounded-xl bg-red-500/20 px-4 py-2 text-sm text-red-100">{error}</p> : null}
          {soundBlocked && !silenced ? <p>Your browser blocked the ringtone. Tap anywhere once to allow call sounds.</p> : null}
          {more > 0 ? <p>{more} more incoming {more === 1 ? "call" : "calls"}</p> : null}
          {typeof Notification !== "undefined" && Notification.permission === "default" ? (
            <button className="inline-flex items-center gap-1 underline" onClick={() => void Notification.requestPermission()}><BellRing className="size-3" />Show calls as notifications when this tab is in the background</button>
          ) : null}
        </div>
      </div>
      <footer className="relative z-10 flex items-start justify-around px-10 pb-12 pt-6 sm:justify-center sm:gap-28">
        <button type="button" disabled={busy} onClick={() => void respond(false)} className="flex flex-col items-center gap-2 disabled:opacity-50">
          <span className="flex size-[4.5rem] items-center justify-center rounded-full bg-[#EA4335] shadow-lg transition-transform hover:scale-105"><PhoneOff className="size-7" /></span>
          <span className="text-sm font-medium text-white/85">Decline</span>
        </button>
        <button type="button" disabled={busy} onClick={() => void respond(true)} className="flex flex-col items-center gap-2 disabled:opacity-50">
          <span className="flex size-[4.5rem] animate-bounce items-center justify-center rounded-full bg-[#1E8E3E] shadow-lg [animation-duration:1.6s]">{video ? <Video className="size-7" /> : <Phone className="size-7" />}</span>
          <span className="text-sm font-medium text-white/85">Accept</span>
        </button>
      </footer>
    </div>
  );
}
