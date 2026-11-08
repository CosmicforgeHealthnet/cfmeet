"use client";

import { useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { Flag, Phone, PhoneIncoming, PhoneOutgoing, RefreshCw, Users, Video } from "lucide-react";
import { useQuery } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Spinner } from "@/components/ui/spinner";
import { useCalling, type CallTarget } from "@/context/CallingContext";
import { useCall, useCallActions, useCallHistory, useCallingIdentity, useCallingMutation } from "@/hooks/calling";
import { usePrincipalNames } from "@/hooks/calling/usePrincipalNames";
import { identityApi } from "@/lib/messaging/api";
import { callingErrorMessage } from "@/lib/calling/errors";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import { activeParticipant, isWindingDown, ownParticipant, ownSession, visibleControls } from "@/lib/calling/call-permissions";
import { hubCallsService } from "@/services/calling/hub-calls.service";
import type { CallKind, CallState, IvrInputRequest } from "@/types/calling";
import CallRecordingControls from "./CallRecordingControls";
import AbuseReportDialog from "./AbuseReportDialog";

/** Calling's group-call target bound (`target_principal_ids` ≤ 64; deployments may set a lower maximum). */
const MAX_GROUP_TARGETS = 64;

const callStatePresentation: Record<CallState, { title: string; description: string }> = {
  REQUESTED: { title: "Placing call", description: "The call request was accepted and is waiting for authorization." },
  AUTHORIZED: { title: "Verifying route", description: "The call is authorized. The destination and available route are being resolved." },
  RESOLVING: { title: "Preparing call", description: "Looking up the destination and preparing available endpoints." },
  ROUTING: { title: "Selecting a route", description: "Selecting an available route." },
  DIALING: { title: "Dialing", description: "Contacting the selected endpoint or endpoints." },
  RINGING: { title: "Ringing", description: "Waiting for someone to answer." },
  ANSWERED: { title: "Connecting", description: "An endpoint answered. Media is still being established." },
  CONNECTED: { title: "Connected", description: "The call is active." },
  HELD: { title: "On hold", description: "Media is paused for everyone until a participant resumes." },
  TRANSFERRING: { title: "Transferring", description: "The call is being transferred." },
  ENDING: { title: "Ending call", description: "Termination was requested. Waiting for cleanup to finish." },
  ENDED: { title: "Call ended", description: "The call completed." },
  DECLINED: { title: "Call declined", description: "The call offer was declined." },
  BUSY: { title: "Line busy", description: "The destination is unavailable because it is busy." },
  NO_ANSWER: { title: "Missed call", description: "The call timed out without an answer." },
  FAILED: { title: "Call failed", description: "The call could not be completed. No automatic retry was started." },
  CANCELLED: { title: "Call cancelled", description: "The call was cancelled before it connected." },
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

const deviceStatusText: Record<string, string> = {
  online: "Ready to receive calls in this browser",
  connecting: "Connecting this browser…",
  reconnecting: "Reconnecting to calling…",
  offline: "This browser is offline for calls",
  disabled: "Receiving calls in this browser is off",
};

export default function CallsContent({ initialCallId = "" }: { initialCallId?: string }) {
  const identity = useCallingIdentity();
  const calling = useCalling();
  if (identity.isPending) return <div className="flex items-center gap-2 p-6"><Spinner /> Loading calling…</div>;
  if (identity.error || !calling) {
    return (
      <div className="space-y-3 p-6">
        <h1 className="text-2xl font-semibold">Calls</h1>
        <p role="alert">{identity.error ? callingErrorMessage(identity.error) : "Calling is starting…"}</p>
        <Button variant="outline" onClick={() => void identity.refetch()}>Try again</Button>
      </div>
    );
  }
  return <CallsWorkspace key={initialCallId} initialCallId={initialCallId} />;
}

function CallsWorkspace({ initialCallId }: { initialCallId: string }) {
  const calling = useCalling()!;
  const { principalId, device } = calling;
  const [selected, setSelected] = useState(initialCallId);
  const [missed, setMissed] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const history = useCallHistory(missed ? { missed: true } : {});
  const entries = history.data?.pages.flatMap((page) => page.data) ?? [];
  const run = async (action: () => Promise<unknown>) => {
    setError(null); setBusy(true);
    try { await action(); } catch (e) { setError(callingErrorMessage(e)); } finally { setBusy(false); }
  };

  return <div className="mx-auto max-w-6xl space-y-5 p-4 pb-40 md:p-6 md:pb-40">
    <header className="flex flex-wrap items-center justify-between gap-3">
      <div><h1 className="text-2xl font-bold tracking-tight">Calls</h1><p className="text-sm text-muted-foreground">Voice, video and group calls, and your call history.</p></div>
      <div className="flex items-center gap-3">
        <span role="status" className="text-sm text-muted-foreground">{deviceStatusText[device.status] ?? device.status}</span>
        {device.status === "disabled" ? <Button variant="outline" onClick={() => void device.setReceiving(true)}><Phone className="size-4" /> Receive calls here</Button> : null}
        {device.status === "offline" ? <Button variant="outline" onClick={() => void device.register()}><RefreshCw className="size-4" /> Reconnect</Button> : null}
      </div>
    </header>
    {error || device.error ? <p role="alert" className="rounded-xl border border-destructive/30 bg-destructive/5 p-3 text-sm text-destructive">{error || device.error}</p> : null}
    <StartCall busy={busy} run={run} onStarted={setSelected} />
    <HubContacts busy={busy} run={run} onStarted={setSelected} />
    <div className="grid gap-5 lg:grid-cols-[1fr_1fr]">
      <section className="rounded-2xl border border-border bg-card p-5">
        <div className="mb-4 flex items-center justify-between gap-2"><h2 className="font-semibold">Recent calls</h2><div className="flex gap-2"><Button size="sm" variant={missed ? "default" : "outline"} onClick={() => setMissed(!missed)}>Missed</Button><Button size="icon" variant="ghost" aria-label="Refresh calls" onClick={() => void history.refetch()}><RefreshCw className="size-4" /></Button></div></div>
        {history.isPending ? <Spinner /> : history.error ? <p role="alert" className="text-sm text-destructive">{callingErrorMessage(history.error)}</p> : entries.length === 0 ? <p className="py-8 text-center text-sm text-muted-foreground">{missed ? "No missed calls." : "Your calls will appear here."}</p> : <ul className="space-y-1">{entries.map((entry) => <li key={entry.callId}><button onClick={() => setSelected(entry.callId)} aria-pressed={selected === entry.callId} className={`flex w-full items-center gap-3 rounded-xl p-3 text-left hover:bg-muted ${selected === entry.callId ? "bg-muted" : ""}`}>
          {entry.participation.direction === "INBOUND" ? <PhoneIncoming className="size-4 shrink-0" /> : <PhoneOutgoing className="size-4 shrink-0" />}<div className="min-w-0 flex-1"><p className="truncate text-sm font-medium">{entry.counterparts.map((p) => p.displayName || "CF Meet user").join(", ") || entry.enterprise?.entryLabel || "Call"}</p><p className="text-xs text-muted-foreground">{entry.participation.outcome.replaceAll("_", " ")} · {new Date(entry.createdAt).toLocaleString()}</p></div><span className="text-xs text-muted-foreground">{entry.counterparts.length > 1 ? "Group" : entry.kind === "VIDEO" ? "Video" : "Voice"}</span>
        </button></li>)}</ul>}
        {history.hasNextPage ? <Button variant="ghost" className="mt-3 w-full" disabled={history.isFetchingNextPage} onClick={() => void history.fetchNextPage()}>Load more</Button> : null}
      </section>
      <CallDetails callId={selected} principalId={principalId} busy={busy} run={run} />
    </div>
  </div>;
}

type Runner = (action: () => Promise<unknown>) => Promise<void>;

/** Direct, group (several people) or extension call through the adapter (CAL-014 group calling). */
function StartCall({ busy, run, onStarted }: { busy: boolean; run: Runner; onStarted: (callId: string) => void }) {
  const calling = useCalling()!;
  const [picked, setPicked] = useState<string[]>([]);
  const [filter, setFilter] = useState("");
  const [extension, setExtension] = useState("");
  const [kind, setKind] = useState<CallKind>("VOICE");
  const directory = useQuery({ queryKey: ["calling", "directory", calling.principalId], queryFn: () => identityApi.directory(), retry: false });
  const people = useMemo(() => (directory.data?.people ?? []).filter((p) => p.principalId !== calling.principalId), [directory.data, calling.principalId]);
  const shown = people.filter((p) => p.displayName.toLowerCase().includes(filter.trim().toLowerCase())).slice(0, 50);
  const toggle = (userId: string) => setPicked((current) => current.includes(userId) ? current.filter((id) => id !== userId) : current.length >= MAX_GROUP_TARGETS ? current : [...current, userId]);
  const inCall = !!calling.media;

  const submit = () => run(async () => {
    let target: CallTarget;
    if (extension.trim()) {
      target = { targetExtension: extension.trim() };
    } else {
      const chosen = people.filter((p) => picked.includes(p.userId));
      const missing = chosen.filter((p) => !p.principalId).map((p) => p.userId);
      const resolved = missing.length ? (await identityApi.resolve(missing)).people : [];
      const principalIds = chosen.map((p) => p.principalId ?? resolved.find((r) => r.userId === p.userId)?.principalId).filter((id): id is string => !!id);
      if (!principalIds.length || principalIds.length !== chosen.length) throw new Error("Some of the people you chose cannot receive calls yet.");
      target = { targetPrincipalIds: principalIds };
    }
    onStarted(await calling.startCall(kind, target));
    setPicked([]); setExtension("");
  });

  return (
    <section className="space-y-4 rounded-2xl border border-border bg-card p-5">
      <h2 className="font-semibold">Start a call</h2>
      <form className="space-y-3" onSubmit={(event) => { event.preventDefault(); void submit(); }}>
        <div className="grid gap-3 md:grid-cols-[1fr_auto_auto] md:items-end">
          <div className="space-y-2">
            <Label htmlFor="call-person-filter">People from your organizations</Label>
            <Input id="call-person-filter" value={filter} placeholder="Search colleagues" disabled={!!extension} onChange={(e) => setFilter(e.target.value)} />
          </div>
          <div className="space-y-2"><Label htmlFor="call-extension">Or extension</Label><Input id="call-extension" value={extension} maxLength={32} placeholder="Extension" onChange={(e) => { setExtension(e.target.value); setPicked([]); }} /></div>
          <div className="space-y-2"><Label htmlFor="call-kind">Call type</Label><select id="call-kind" value={kind} onChange={(e) => setKind(e.target.value as CallKind)} className="h-10 rounded-md border border-input bg-background px-3 text-sm"><option value="VOICE">Voice</option><option value="VIDEO">Video</option></select></div>
        </div>
        {!extension ? (
          <ul aria-label="Choose people to call" className="flex max-h-40 flex-wrap gap-2 overflow-y-auto">
            {shown.map((person) => (
              <li key={person.userId}>
                <label className={`flex cursor-pointer items-center gap-2 rounded-full border px-3 py-1 text-sm ${picked.includes(person.userId) ? "border-primary bg-primary/10" : "border-border"}`}>
                  <input type="checkbox" className="size-3.5" checked={picked.includes(person.userId)} onChange={() => toggle(person.userId)} />{person.displayName}
                </label>
              </li>
            ))}
            {directory.isPending ? <li><Spinner /></li> : null}
            {!directory.isPending && !shown.length ? <li className="text-sm text-muted-foreground">{people.length ? "No match." : "No colleagues to call yet. Contacts are below."}</li> : null}
          </ul>
        ) : null}
        <div className="flex flex-wrap items-center gap-3">
          <Button type="submit" disabled={busy || !calling.device.endpoint || (!picked.length && !extension.trim()) || inCall}>
            {picked.length > 1 ? <Users className="size-4" /> : kind === "VIDEO" ? <Video className="size-4" /> : <Phone className="size-4" />}
            {picked.length > 1 ? `Group call (${picked.length})` : "Call"}
          </Button>
          {inCall ? <span className="text-sm text-muted-foreground">You are in a call.</span> : null}
          {!calling.device.endpoint ? <span className="text-sm text-muted-foreground">This browser must be connected to place calls.</span> : null}
        </div>
      </form>
      {directory.error ? <p className="text-sm text-muted-foreground">Colleagues could not be loaded. <button className="underline" onClick={() => void directory.refetch()}>Try again</button></p> : null}
    </section>
  );
}

/** Contacts from the Communications Hub, called through the Hub launch operation (CAL-014). */
function HubContacts({ busy, run, onStarted }: { busy: boolean; run: Runner; onStarted: (callId: string) => void }) {
  const calling = useCalling()!;
  const contacts = useQuery({ queryKey: ["calling", "hub-contacts"], queryFn: () => hubCallsService.contacts(), retry: false, staleTime: 60_000 });
  const list = contacts.data?.data ?? [];
  if (contacts.isPending) return null;
  return (
    <section className="space-y-3 rounded-2xl border border-border bg-card p-5">
      <h2 className="font-semibold">Contacts</h2>
      {contacts.error ? <p className="text-sm text-muted-foreground">{callingErrorMessage(contacts.error)}</p> : list.length === 0 ? (
        <p className="text-sm text-muted-foreground">Your accepted contacts appear here. Add contacts in Messages.</p>
      ) : (
        <ul className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
          {list.map((contact) => (
            <li key={contact.contactId} className="flex items-center gap-2 rounded-xl border border-border p-2">
              <span className="min-w-0 flex-1 truncate text-sm">{contact.alias || contact.displayName}</span>
              <Button size="icon" variant="ghost" aria-label={`Voice call ${contact.displayName}`} disabled={busy || !!calling.media || !calling.device.endpoint} onClick={() => void run(async () => onStarted(await calling.callContact(contact.contactId, "VOICE")))}><Phone className="size-4" /></Button>
              <Button size="icon" variant="ghost" aria-label={`Video call ${contact.displayName}`} disabled={busy || !!calling.media || !calling.device.endpoint} onClick={() => void run(async () => onStarted(await calling.callContact(contact.contactId, "VIDEO")))}><Video className="size-4" /></Button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function CallDetails({ callId, principalId, busy, run }: { callId: string; principalId: string; busy: boolean; run: Runner }) {
  const calling = useCalling()!;
  const call = useCall(callId);
  const actions = useCallActions(callId);
  const mutation = useCallingMutation();
  const [now, setNow] = useState(() => Date.now());
  const [reporting, setReporting] = useState(false);
  const unnamed = call.data?.participants.filter((p) => !p.displayName && p.principalId !== principalId) ?? [];
  const names = usePrincipalNames(unnamed.map((p) => p.principalId));
  const state = call.data?.state;

  useEffect(() => {
    if (state !== "CONNECTED") return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [state]);

  if (!callId) return <section className="rounded-2xl border border-border bg-card p-5"><h2 className="font-semibold">Call details</h2><p className="py-8 text-center text-sm text-muted-foreground">Select a call to view its details.</p></section>;

  const data = call.data;
  const endpointId = calling.device.endpoint?.endpointId ?? null;
  const live = !!data && !isWindingDown(data.state);
  const available = (name: string) => !!actions.data?.actions.some((a) => a.action === name && a.available);
  const controls = data ? visibleControls(data, principalId, available) : null;
  const mine = data ? activeParticipant(data, principalId) : undefined;
  const everMine = data ? ownParticipant(data, principalId) : undefined;
  const session = data ? ownSession(data, principalId, endpointId) : undefined;
  const joinable = live && session && data?.sessions.find((s) => s.sessionId === session.sessionId)?.mediaAccess === "AUTHORIZED" && calling.media?.sessionId !== session.sessionId;
  const durationAsOf = data ? Date.parse(data.durationsAsOf) : Number.NaN;
  const liveDuration = data?.state === "CONNECTED" && Number.isFinite(durationAsOf) ? (data.connectedDurationMs ?? 0) + Math.max(0, now - durationAsOf) : (data?.connectedDurationMs ?? 0);
  const command = (operation: "cancel" | "end" | "hold" | "resume") => run(() => mutation.mutateAsync({ operation, callId, body: { expectedVersion: data!.version }, key: newIdempotencyKey() }));

  return (
    <section className="space-y-4 rounded-2xl border border-border bg-card p-5">
      <div className="flex items-center justify-between gap-2"><h2 className="font-semibold">Call details</h2><Button asChild size="sm" variant="ghost"><Link href={`/calling/${encodeURIComponent(callId)}`}>Open details</Link></Button></div>
      {call.isPending ? <Spinner /> : call.error ? <p role="alert" className="text-sm text-destructive">{callingErrorMessage(call.error)}</p> : data ? <>
        <div className="rounded-xl border border-border bg-background p-4">
          <p className="text-lg font-medium">{data.participants.length > 2 ? "Group call" : data.kind === "VIDEO" ? "Video call" : "Voice call"}</p>
          <p className="mt-1 flex items-center gap-2 font-semibold" role="status" aria-live="polite">
            <span className={`size-2 rounded-full bg-primary ${data.state === "RINGING" ? "animate-pulse" : ""}`} />
            {callStatePresentation[data.state].title}
            {data.state === "CONNECTED" ? <span className="font-mono text-sm tabular-nums">{formatCallDuration(liveDuration)}</span> : null}
          </p>
          <p className="mt-1 text-sm text-muted-foreground">{callStatePresentation[data.state].description}</p>
          {data.failureCategory ? <p className="mt-2 text-sm text-destructive">Reason: {data.failureCategory.replaceAll("_", " ")}</p> : null}
          {data.state === "ENDED" && data.connectedDurationMs !== null ? <p className="mt-2 text-sm text-muted-foreground">Call summary · Duration {formatCallDuration(data.connectedDurationMs)}</p> : null}
        </div>
        <ul className="space-y-2 text-sm">{data.participants.map((p) => <li key={p.participantId} className="flex justify-between gap-2"><span>{p.principalId === principalId ? "You" : p.displayName || names.data?.[p.principalId] || "CF Meet user"}</span><span className="text-muted-foreground">{p.endedAt ? "Left" : p.role === "INITIATOR" ? "Caller" : "Recipient"}</span></li>)}</ul>
        <div className="flex flex-wrap gap-2">
          {joinable ? <Button disabled={busy || !!calling.media} onClick={() => void run(() => calling.joinMedia(callId, session!.sessionId, data.kind))}>{data.kind === "VIDEO" ? <Video className="size-4" /> : <Phone className="size-4" />} Join on this device</Button> : null}
          {controls?.hold ? <Button variant="outline" disabled={busy || mutation.isPending} onClick={() => void command("hold")}>Hold</Button> : null}
          {controls?.resume ? <Button variant="outline" disabled={busy || mutation.isPending} onClick={() => void command("resume")}>Resume</Button> : null}
          {controls?.cancel ? <Button variant="outline" disabled={busy || mutation.isPending} onClick={() => void command("cancel")}>Cancel call</Button> : null}
          {live && session ? <Button variant="outline" disabled={busy} onClick={() => void run(async () => {
            await mutation.mutateAsync({ operation: "removeSession", callId, sessionId: session.sessionId, body: { expectedVersion: data.version }, key: newIdempotencyKey() });
            if (calling.media?.callId === callId) calling.leaveMedia();
          })}>Leave call</Button> : null}
          {controls?.end ? <Button variant="destructive" disabled={busy || mutation.isPending} onClick={() => void command("end")}>End for all</Button> : null}
          {everMine ? <Button variant="ghost" onClick={() => setReporting(true)}><Flag className="size-4" /> Report</Button> : null}
        </div>
        {endpointId && live && mine && data.initiatorPrincipalId === principalId ? <form className="flex gap-2" onSubmit={(event) => {
          event.preventDefault();
          const digit = String(new FormData(event.currentTarget).get("digit")) as IvrInputRequest["digit"];
          void run(() => mutation.mutateAsync({ operation: "ivrInput", callId, body: { endpointId, digit } }));
        }}><Input name="digit" aria-label="Keypad digit" placeholder="Keypad: 0–9, * or #" maxLength={1} pattern="[0-9*#]" required /><Button type="submit" disabled={busy} variant="outline">Send key</Button></form> : null}
        {controls?.transfer && mine ? <form className="flex gap-2" onSubmit={(e) => { e.preventDefault(); const form = new FormData(e.currentTarget); void run(() => mutation.mutateAsync({ operation: "transfer", callId, key: newIdempotencyKey(), body: { transferringParticipantId: mine.participantId, expectedVersion: data.version, targetExtension: String(form.get("extension")) } })); }}><Input name="extension" aria-label="Transfer to extension" placeholder="Transfer to extension" maxLength={32} required /><Button type="submit" variant="outline" disabled={busy}>Transfer</Button></form> : null}
        {mine && live ? <CallRecordingControls callId={callId} participantId={mine.participantId} endpointId={session ? endpointId : null} live={live} /> : null}
        {actions.error ? <p className="text-sm text-destructive">{callingErrorMessage(actions.error)}</p> : null}
        <AbuseReportDialog callId={callId} open={reporting} onOpenChange={setReporting} />
      </> : null}
    </section>
  );
}
