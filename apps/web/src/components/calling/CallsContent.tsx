"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { Phone, PhoneIncoming, PhoneOutgoing, RefreshCw, Video } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Spinner } from "@/components/ui/spinner";
import { useCall, useCallActions, useCallHistory, useCallingDevice, useCallingIdentity, useCallingMutation } from "@/hooks/calling";
import { identityApi } from "@/lib/messaging/api";
import { callingErrorMessage } from "@/lib/calling/errors";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import { isTerminalCallState, type CallKind, type CallState, type MediaCredential, type RecoveredInvitation } from "@/types/calling";
import CallMedia from "./CallMedia";

const callStatePresentation: Record<CallState, { title: string; description: string }> = {
  REQUESTED: {
    title: "Placing call",
    description: "The call request was accepted and is waiting for authorization.",
  },
  AUTHORIZED: {
    title: "Verifying route",
    description: "The call is authorized. The destination and available route are being resolved.",
  },
  RESOLVING: {
    title: "Preparing call",
    description: "Looking up the destination and preparing available endpoints.",
  },
  ROUTING: {
    title: "Selecting a route",
    description: "Selecting an available route. External telephone connectivity depends on configured routing.",
  },
  DIALING: {
    title: "Dialing",
    description: "Contacting the selected endpoint or endpoints.",
  },
  RINGING: {
    title: "Ringing",
    description: "Waiting for someone to answer.",
  },
  ANSWERED: {
    title: "Connecting",
    description: "An endpoint answered. Media is still being established.",
  },
  CONNECTED: {
    title: "Connected",
    description: "Your call is connected.",
  },
  HELD: {
    title: "On hold",
    description: "The call is intentionally on hold. Resume it when you are ready.",
  },
  TRANSFERRING: {
    title: "Transferring",
    description: "The handoff is in progress. If it cannot complete, the call may return to its previous state.",
  },
  ENDING: {
    title: "Ending call",
    description: "Termination was requested. Waiting for external cleanup to finish.",
  },
  ENDED: {
    title: "Call ended",
    description: "The call completed.",
  },
  DECLINED: {
    title: "Call declined",
    description: "The call offer was declined.",
  },
  BUSY: {
    title: "Line busy",
    description: "The destination is unavailable because it is busy.",
  },
  NO_ANSWER: {
    title: "Missed call",
    description: "The call timed out without an answer.",
  },
  FAILED: {
    title: "Call failed",
    description: "The call could not be completed. No automatic retry was started.",
  },
  CANCELLED: {
    title: "Call cancelled",
    description: "The call was cancelled before it connected.",
  },
};

function formatCallDuration(milliseconds: number): string {
  const totalSeconds = Math.max(0, Math.floor(milliseconds / 1000));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`
    : `${minutes}:${String(seconds).padStart(2, "0")}`;
}

export default function CallsContent({ initialCallId = "" }: { initialCallId?: string }) {
  const identity = useCallingIdentity();
  const principalId = identity.data?.principalId;
  // Keyed session unmounts and drops endpoint credentials when identity changes.
  if (identity.isPending) return <div className="flex items-center gap-2 p-6"><Spinner /> Loading calling…</div>;
  if (identity.error || !principalId) return <div className="space-y-3 p-6"><h1 className="text-2xl font-semibold">Calls</h1><p role="alert">{callingErrorMessage(identity.error)}</p><Button variant="outline" onClick={() => void identity.refetch()}>Try again</Button></div>;
  return <CallingSession key={`${principalId}:${initialCallId}`} principalId={principalId} initialCallId={initialCallId} />;
}

function CallingSession({ principalId, initialCallId }: { principalId: string; initialCallId: string }) {
  const device = useCallingDevice(principalId);
  const [selected, setSelected] = useState(initialCallId);
  const [missed, setMissed] = useState(false);
  const [target, setTarget] = useState("");
  const [extension, setExtension] = useState("");
  const [kind, setKind] = useState<CallKind>("VOICE");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [media, setMedia] = useState<{ credential: MediaCredential; callId: string; sessionId: string } | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const history = useCallHistory(missed ? { missed: true } : {});
  const call = useCall(selected);
  const mediaCall = useCall(media?.callId ?? "");
  const actions = useCallActions(selected);
  const mutation = useCallingMutation();
  const directory = useQuery({ queryKey: ["calling", "directory", principalId], queryFn: () => identityApi.directory(), retry: false });
  const run = async (action: () => Promise<unknown>) => {
    setError(null); setBusy(true);
    try { await action(); } catch (e) { setError(callingErrorMessage(e)); }
    finally { setBusy(false); }
  };
  const connectMedia = async (callId: string, sessionId: string) => {
    const credential = await device.mediaCredential(sessionId);
    setMedia({ credential, callId, sessionId });
  };
  const answer = (invitation: RecoveredInvitation, accept: boolean) => run(async () => {
    const result = await device.answer(invitation, accept);
    setSelected(result.call.callId);
    if ("sessionId" in result) await connectMedia(result.call.callId, result.sessionId);
  });
  const activeMedia = media && !mediaCall.error && !(mediaCall.data && isTerminalCallState(mediaCall.data.state)) ? media : null;
  const entries = history.data?.pages.flatMap((page) => page.data) ?? [];
  const ownParticipant = call.data?.participants.find((p) => p.principalId === principalId && !p.endedAt);
  const ownSession = call.data?.sessions.find((s) => s.participantId === ownParticipant?.participantId && s.endpointId === device.endpoint?.endpointId && s.mediaAccess === "AUTHORIZED");
  const available = (name: string) => actions.data?.actions.some((a) => a.action === name && a.available);
  const selectedCallState = call.data?.state;

  useEffect(() => {
    if (selectedCallState !== "CONNECTED") return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [selectedCallState]);

  const connectedDuration = call.data?.connectedDurationMs ?? 0;
  const durationAsOf = call.data ? Date.parse(call.data.durationsAsOf) : Number.NaN;
  const liveDuration = selectedCallState === "CONNECTED" && Number.isFinite(durationAsOf)
    ? connectedDuration + Math.max(0, now - durationAsOf)
    : connectedDuration;

  return <div className="mx-auto max-w-6xl space-y-5 p-4 md:p-6">
    <header className="flex flex-wrap items-center justify-between gap-3">
      <div><h1 className="text-2xl font-bold tracking-tight">Calls</h1><p className="text-sm text-muted-foreground">Voice, video, and your call history.</p></div>
      <div className="flex items-center gap-3"><span role="status" className="text-sm text-muted-foreground">{device.status === "online" ? "Ready to receive calls" : device.status}</span>
        <Button variant="outline" disabled={device.status === "connecting" || device.status === "online"} onClick={() => void device.register()}><Phone className="size-4" /> Enable this browser</Button></div>
    </header>
    {error || device.error || mutation.error ? <p role="alert" className="rounded-xl border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">{error || device.error || callingErrorMessage(mutation.error)}</p> : null}
    {device.invitations.filter((i) => i.actionable).map((invitation) => <section key={invitation.invitationId} className="flex flex-wrap items-center justify-between gap-3 rounded-2xl border border-primary bg-primary/5 p-4">
      <div className="flex items-center gap-3"><PhoneIncoming className="size-5 text-primary" /><div><h2 className="font-semibold">Incoming {invitation.callKind.toLowerCase()} call</h2><p className="text-sm text-muted-foreground">{invitation.purpose === "TRANSFER" ? "Transferred to you" : "Someone is calling you"}</p></div></div>
      <div className="flex gap-2"><Button disabled={busy} onClick={() => void answer(invitation, true)}>Accept</Button><Button disabled={busy} variant="outline" onClick={() => void answer(invitation, false)}>Decline</Button></div>
    </section>)}
    {activeMedia ? <CallMedia key={activeMedia.sessionId} credential={activeMedia.credential} video={mediaCall.data?.kind === "VIDEO"} onError={(e) => setError(callingErrorMessage(e))} onLeave={() => setMedia(null)} /> : null}
    <section className="space-y-4 rounded-2xl border border-border bg-card p-5">
      <h2 className="font-semibold">Start a call</h2>
      <form className="flex flex-wrap items-end gap-3" onSubmit={(event) => { event.preventDefault(); void run(async () => {
        if (!device.endpoint) throw new Error("Enable this browser before calling.");
        const person = directory.data?.people.find((p) => p.userId === target);
        let targetPrincipalId = person?.principalId;
        if (!extension.trim() && person && !targetPrincipalId) {
          targetPrincipalId = (await identityApi.resolve([person.userId])).people.find((p) => p.userId === person.userId)?.principalId;
        }
        if (!extension.trim() && !targetPrincipalId) throw new Error("This contact is not available for calling.");
        const result = await mutation.mutateAsync({ operation: "create", key: newIdempotencyKey(), body: {
          kind, initiatorPrincipalId: principalId, initiatorEndpointId: device.endpoint.endpointId,
          ...(extension.trim() ? { targetExtension: extension.trim() } : { targetPrincipalIds: [targetPrincipalId!] }),
        } });
        if ("call" in result) {
          setSelected(result.call.callId);
          if ("initiatorSessionId" in result && result.initiatorSessionId) await connectMedia(result.call.callId, result.initiatorSessionId);
        }
      }); }}>
        <div className="min-w-48 flex-1 space-y-2"><Label htmlFor="call-person">Person</Label><select id="call-person" value={target} onChange={(e) => { setTarget(e.target.value); setExtension(""); }} className="h-10 w-full rounded-md border border-input bg-background px-3 text-sm" disabled={!!extension}>
          <option value="">Choose a contact</option>{directory.data?.people.filter((p) => p.principalId !== principalId).map((person) => <option key={person.userId} value={person.userId}>{person.displayName}</option>)}
        </select></div>
        <div className="space-y-2"><Label htmlFor="call-extension">Or extension</Label><Input id="call-extension" value={extension} maxLength={32} placeholder="Extension" onChange={(e) => { setExtension(e.target.value); setTarget(""); }} /></div>
        <div className="space-y-2"><Label htmlFor="call-kind">Call type</Label><select id="call-kind" value={kind} onChange={(e) => setKind(e.target.value as CallKind)} className="h-10 rounded-md border border-input bg-background px-3 text-sm"><option value="VOICE">Voice</option><option value="VIDEO">Video</option></select></div>
        <Button type="submit" disabled={busy || mutation.isPending || !device.endpoint || (!target && !extension.trim()) || !!activeMedia}>{kind === "VIDEO" ? <Video className="size-4" /> : <Phone className="size-4" />} Call</Button>
      </form>
      {directory.error ? <p className="text-sm text-muted-foreground">Contacts could not be loaded. <button className="underline" onClick={() => void directory.refetch()}>Try again</button></p> : null}
    </section>
    <div className="grid gap-5 lg:grid-cols-[1fr_1fr]">
      <section className="rounded-2xl border border-border bg-card p-5">
        <div className="mb-4 flex items-center justify-between gap-2"><h2 className="font-semibold">Recent calls</h2><div className="flex gap-2"><Button size="sm" variant={missed ? "default" : "outline"} onClick={() => setMissed(!missed)}>Missed</Button><Button size="icon" variant="ghost" aria-label="Refresh calls" onClick={() => void history.refetch()}><RefreshCw className="size-4" /></Button></div></div>
        {history.isPending ? <Spinner /> : history.error ? <p role="alert" className="text-sm text-destructive">{callingErrorMessage(history.error)}</p> : entries.length === 0 ? <p className="py-8 text-center text-sm text-muted-foreground">{missed ? "No missed calls." : "Your calls will appear here."}</p> : <ul className="space-y-1">{entries.map((entry) => <li key={entry.callId}><button onClick={() => setSelected(entry.callId)} aria-pressed={selected === entry.callId} className={`flex w-full items-center gap-3 rounded-xl p-3 text-left hover:bg-muted ${selected === entry.callId ? "bg-muted" : ""}`}>
          {entry.participation.direction === "INBOUND" ? <PhoneIncoming className="size-4 shrink-0" /> : <PhoneOutgoing className="size-4 shrink-0" />}<div className="min-w-0 flex-1"><p className="truncate text-sm font-medium">{entry.counterparts.map((p) => p.displayName || "CF Meet user").join(", ") || entry.enterprise?.entryLabel || "Call"}</p><p className="text-xs text-muted-foreground">{entry.participation.outcome.replaceAll("_", " ")} · {new Date(entry.createdAt).toLocaleString()}</p></div><span className="text-xs text-muted-foreground">{entry.kind === "VIDEO" ? "Video" : "Voice"}</span>
        </button></li>)}</ul>}
        {history.hasNextPage ? <Button variant="ghost" className="mt-3 w-full" disabled={history.isFetchingNextPage} onClick={() => void history.fetchNextPage()}>Load more</Button> : null}
      </section>
      <section className="space-y-4 rounded-2xl border border-border bg-card p-5">
        <div className="flex items-center justify-between gap-2"><h2 className="font-semibold">Call details</h2>{selected ? <Button asChild size="sm" variant="ghost"><Link href={`/calling/${encodeURIComponent(selected)}`}>Open details</Link></Button> : null}</div>
        {!selected ? <p className="py-8 text-center text-sm text-muted-foreground">Select a call to view its details.</p> : call.isPending ? <Spinner /> : call.error ? <p role="alert" className="text-sm text-destructive">{callingErrorMessage(call.error)}</p> : call.data ? <>
          <div className="rounded-xl border border-border bg-background p-4">
            <p className="text-lg font-medium">{call.data.kind === "VIDEO" ? "Video call" : "Voice call"}</p>
            <p className="mt-1 flex items-center gap-2 font-semibold" role="status" aria-live="polite">
              <span className={`size-2 rounded-full bg-primary ${call.data.state === "RINGING" ? "animate-pulse" : ""}`} />
              {callStatePresentation[call.data.state].title}
              {call.data.state === "CONNECTED" ? <span className="font-mono text-sm tabular-nums">{formatCallDuration(liveDuration)}</span> : null}
            </p>
            <p className="mt-1 text-sm text-muted-foreground">{callStatePresentation[call.data.state].description}</p>
            {call.data.state === "FAILED" && call.data.failureCategory ? (
              <p className="mt-2 text-sm text-destructive">Reason: {call.data.failureCategory.replaceAll("_", " ")}</p>
            ) : null}
            {call.data.state === "ENDED" && call.data.connectedDurationMs !== null ? (
              <p className="mt-2 text-sm text-muted-foreground">Call summary · Duration {formatCallDuration(call.data.connectedDurationMs)}</p>
            ) : null}
          </div>
          <ul className="space-y-2 text-sm">{call.data.participants.map((p) => <li key={p.participantId} className="flex justify-between gap-2"><span>{p.principalId === principalId ? "You" : p.displayName || "CF Meet user"}</span><span className="text-muted-foreground">{p.endedAt ? "Left" : p.role === "INITIATOR" ? "Caller" : "Recipient"}</span></li>)}</ul>
          <div className="flex flex-wrap gap-2">{(["cancel", "end", "hold", "resume"] as const).map((operation) => {
            const requestedOnlyAllowsCancel = call.data?.state !== "REQUESTED" || operation === "cancel";
            const commandsEnabled = call.data?.state !== "ENDING" && !isTerminalCallState(call.data!.state);
            return available(operation.toUpperCase()) && call.data?.initiatorPrincipalId === principalId && requestedOnlyAllowsCancel && commandsEnabled ? (
              <Button key={operation} variant={operation === "end" ? "destructive" : "outline"} disabled={busy || mutation.isPending} onClick={() => void run(() => mutation.mutateAsync({ operation, callId: selected, body: { expectedVersion: call.data!.version }, key: newIdempotencyKey() }))}>{operation[0].toUpperCase() + operation.slice(1)}</Button>
            ) : null;
          })}
            {ownSession && !activeMedia && !isTerminalCallState(call.data.state) && call.data.state !== "ENDING" ? <Button disabled={busy} onClick={() => void run(() => connectMedia(selected, ownSession.sessionId))}>Join audio</Button> : null}
            {ownSession && !isTerminalCallState(call.data.state) && call.data.state !== "ENDING" ? <Button variant="outline" disabled={busy} onClick={() => void run(async () => {
              await mutation.mutateAsync({ operation: "removeSession", callId: selected, sessionId: ownSession.sessionId, body: { expectedVersion: call.data!.version }, key: newIdempotencyKey() });
              if (media?.callId === selected) setMedia(null);
            })}>Leave call</Button> : null}
          </div>
          {device.endpoint && !isTerminalCallState(call.data.state) && call.data.state !== "ENDING" && call.data.initiatorPrincipalId === principalId ? <form className="flex gap-2" onSubmit={(event) => {
            event.preventDefault();
            const digit = String(new FormData(event.currentTarget).get("digit")) as import("@/types/calling").IvrInputRequest["digit"];
            void run(() => mutation.mutateAsync({ operation: "ivrInput", callId: selected, body: { endpointId: device.endpoint!.endpointId, digit } }));
          }}><Input name="digit" aria-label="Keypad digit" placeholder="Keypad: 0–9, * or #" maxLength={1} pattern="[0-9*#]" required /><Button type="submit" disabled={busy} variant="outline">Send key</Button></form> : null}
          {available("TRANSFER") && ownParticipant && call.data.state !== "ENDING" ? <form className="flex gap-2" onSubmit={(e) => { e.preventDefault(); const form = new FormData(e.currentTarget); void run(() => mutation.mutateAsync({ operation: "transfer", callId: selected, key: newIdempotencyKey(), body: { transferringParticipantId: ownParticipant.participantId, expectedVersion: call.data!.version, targetExtension: String(form.get("extension")) } })); }}><Input name="extension" aria-label="Transfer to extension" placeholder="Transfer to extension" maxLength={32} required /><Button type="submit" variant="outline" disabled={busy}>Transfer</Button></form> : null}
          {call.data.failureCategory ? <p className="text-sm text-destructive">Call failed: {call.data.failureCategory.replaceAll("_", " ")}</p> : null}
          {actions.error ? <p className="text-sm text-destructive">{callingErrorMessage(actions.error)}</p> : null}
        </> : null}
      </section>
    </div>
  </div>;
}
