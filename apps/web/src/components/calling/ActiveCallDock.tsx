"use client";

/**
 * The active call, on every signed-in page (CAL-002, CAL-015): media connection, its surfaced
 * status (connecting / network interrupted / retrying in n s / could not restore) with an
 * automatic rejoin, and the controls the signed-in party may use — Leave for anyone, End for
 * all / Hold / Resume for any active participant (ADR 0009 party rules), Cancel for the caller
 * before the answer.
 */

import { useState } from "react";
import Link from "next/link";
import { ChevronDown, ChevronUp, PhoneOff, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useCalling } from "@/context/CallingContext";
import { useCall, useCallActions, useCallingMutation } from "@/hooks/calling/useCalls";
import { usePrincipalNames } from "@/hooks/calling/usePrincipalNames";
import { callingErrorMessage } from "@/lib/calling/errors";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import { callerLabel } from "@/lib/calling/incoming-call";
import { ownSession, visibleControls } from "@/lib/calling/call-permissions";
import { mediaStatusLabel } from "@/lib/calling/media-reconnect";
import CallMedia from "./CallMedia";

export default function ActiveCallDock() {
  const calling = useCalling();
  const media = calling?.media ?? null;
  const call = useCall(media?.callId ?? "");
  const actions = useCallActions(media?.callId ?? "");
  const mutation = useCallingMutation();
  const others = call.data?.participants.filter((p) => p.principalId !== calling?.principalId && !p.displayName) ?? [];
  const names = usePrincipalNames(others.map((p) => p.principalId));
  const [collapsed, setCollapsed] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!calling || !media) return null;

  const { mediaState, mediaEvents } = calling;
  const counterpart = call.data
    ? call.data.participants.filter((p) => p.principalId !== calling.principalId).map((p) => p.displayName || names.data?.[p.principalId] || "CF Meet user").join(", ") || callerLabel(call.data.participants, calling.principalId)
    : "Call";
  const controls = call.data
    ? visibleControls(call.data, calling.principalId, (name) => !!actions.data?.actions.some((a) => a.action === name && a.available))
    : { cancel: false, end: false, hold: false, resume: false, transfer: false };
  const session = call.data ? ownSession(call.data, calling.principalId, calling.device.endpoint?.endpointId) : undefined;
  const run = async (action: () => Promise<unknown>) => {
    setError(null);
    try { await action(); } catch (e) { setError(callingErrorMessage(e)); }
  };
  const command = (operation: "cancel" | "end" | "hold" | "resume") => run(() =>
    mutation.mutateAsync({ operation, callId: media.callId, body: { expectedVersion: call.data!.version }, key: newIdempotencyKey() }));
  const leave = () => run(async () => {
    if (session && call.data) {
      await mutation.mutateAsync({ operation: "removeSession", callId: media.callId, sessionId: session.sessionId, body: { expectedVersion: call.data.version }, key: newIdempotencyKey() });
    }
    calling.leaveMedia();
  });
  const statusTone = mediaState.status === "connected" ? "bg-emerald-500" : mediaState.status === "failed" ? "bg-destructive" : "animate-pulse bg-amber-500";

  return (
    <section aria-label="Active call" className="fixed right-3 bottom-3 z-50 w-[min(24rem,calc(100vw-1.5rem))] rounded-2xl border border-border bg-card p-3 shadow-2xl">
      <div className="flex items-center gap-2">
        <span className={`size-2.5 shrink-0 rounded-full ${statusTone}`} aria-hidden />
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-semibold">{counterpart}</p>
          <p role="status" aria-live="polite" className="truncate text-xs text-muted-foreground">
            {call.data?.state === "HELD" ? "On hold · " : ""}{mediaStatusLabel(mediaState)}
          </p>
        </div>
        <Button asChild size="sm" variant="ghost"><Link href={`/calling/${encodeURIComponent(media.callId)}`}>Details</Link></Button>
        <Button size="icon" variant="ghost" aria-label={collapsed ? "Expand call" : "Collapse call"} onClick={() => setCollapsed(!collapsed)}>
          {collapsed ? <ChevronUp className="size-4" /> : <ChevronDown className="size-4" />}
        </Button>
      </div>
      {/* The media connection stays mounted while collapsed so audio continues. */}
      <div className={collapsed ? "hidden" : "mt-3 space-y-3"}>
        {media.credential ? (
          <CallMedia key={media.credential.token} credential={media.credential} video={media.kind === "VIDEO"}
            onConnected={mediaEvents.connected} onDisconnected={mediaEvents.disconnected}
            onProviderReconnecting={mediaEvents.providerReconnecting} onProviderReconnected={mediaEvents.providerReconnected}
            onError={(e) => setError(callingErrorMessage(e))} />
        ) : null}
        {mediaState.status === "failed" ? (
          <Button size="sm" variant="outline" onClick={calling.retryMedia}><RefreshCw className="size-4" /> Try to reconnect</Button>
        ) : null}
        {error ? <p role="alert" className="text-xs text-destructive">{error}</p> : null}
        <div className="flex flex-wrap gap-2">
          {controls.hold ? <Button size="sm" variant="outline" disabled={mutation.isPending} onClick={() => void command("hold")}>Hold</Button> : null}
          {controls.resume ? <Button size="sm" variant="outline" disabled={mutation.isPending} onClick={() => void command("resume")}>Resume</Button> : null}
          {controls.cancel ? <Button size="sm" variant="outline" disabled={mutation.isPending} onClick={() => void command("cancel")}>Cancel call</Button> : null}
          <Button size="sm" variant="outline" disabled={mutation.isPending} onClick={() => void leave()}>Leave</Button>
          {controls.end ? <Button size="sm" variant="destructive" disabled={mutation.isPending} onClick={() => void command("end")}><PhoneOff className="size-4" /> End for all</Button> : null}
        </div>
      </div>
    </section>
  );
}
