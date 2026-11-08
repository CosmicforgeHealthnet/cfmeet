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
import { Button } from "@/components/ui/button";
import { useCalling } from "@/context/CallingContext";
import { useCall } from "@/hooks/calling/useCalls";
import { usePrincipalNames } from "@/hooks/calling/usePrincipalNames";
import { callingErrorMessage } from "@/lib/calling/errors";
import { callerLabel, callerPrincipal, offerHeadline, ringingOffers } from "@/lib/calling/incoming-call";
import { createRingtone } from "@/lib/calling/ringtone";
import type { RecoveredInvitation } from "@/types/calling";

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

  return (
    <div role="alertdialog" aria-live="assertive" aria-labelledby="incoming-call-title" aria-describedby="incoming-call-caller"
      className="fixed inset-x-3 top-3 z-[60] mx-auto max-w-md rounded-2xl border border-primary bg-card p-4 shadow-2xl sm:right-4 sm:left-auto sm:mx-0">
      <div className="flex items-center gap-3">
        <span className="flex size-11 shrink-0 items-center justify-center rounded-full bg-primary/10 text-primary">
          {offer.callKind === "VIDEO" ? <Video className="size-5 animate-pulse" /> : <Phone className="size-5 animate-pulse" />}
        </span>
        <div className="min-w-0 flex-1">
          <p id="incoming-call-title" className="text-sm text-muted-foreground">{headline}</p>
          <p id="incoming-call-caller" className="truncate text-lg font-semibold">{label}</p>
        </div>
        <Button size="icon" variant="ghost" aria-label={silenced ? "Ringtone silenced" : "Silence ringtone"} onClick={() => setSilenced(true)} disabled={silenced}><VolumeX className="size-4" /></Button>
      </div>
      {error ? <p role="alert" className="mt-2 text-sm text-destructive">{error}</p> : null}
      {soundBlocked && !silenced ? <p className="mt-2 text-xs text-muted-foreground">Your browser blocked the ringtone. Click anywhere on the page once to allow call sounds.</p> : null}
      {more > 0 ? <p className="mt-2 text-xs text-muted-foreground">{more} more incoming {more === 1 ? "call" : "calls"}</p> : null}
      <div className="mt-3 flex gap-2">
        <Button className="flex-1" disabled={busy} onClick={() => void respond(true)}><Phone className="size-4" /> Accept</Button>
        <Button className="flex-1" variant="destructive" disabled={busy} onClick={() => void respond(false)}><PhoneOff className="size-4" /> Decline</Button>
      </div>
      {typeof Notification !== "undefined" && Notification.permission === "default" ? (
        <button className="mt-2 flex items-center gap-1 text-xs text-muted-foreground underline" onClick={() => void Notification.requestPermission()}>
          <BellRing className="size-3" /> Show calls as notifications when this tab is in the background
        </button>
      ) : null}
    </div>
  );
}
