"use client";

/**
 * Calls (Teams / Zoom Phone / Google Voice patterns): All · Missed · Voicemail on the left with
 * one-tap call back; on the right a permanent dialer (people search or keypad, voice / video)
 * with starred contacts, or the selected call. In-call controls live in the floating call bar
 * (ActiveCallDock) only, so they are never shown twice.
 */
import { communicationsService } from "@/services/v2/communications.service";
import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import Link from "next/link";
import {
  ArrowLeft, Check, Delete, Trash2, MicOff, ExternalLink, Settings2, Star, UserPlus, Voicemail, Flag, Grid3x3, MoreHorizontal, Pause, Phone, PhoneCall, PhoneIncoming, PhoneMissed, PhoneOff, PhoneOutgoing,
  Play, RefreshCw, Search, Users, Video, X,
} from "lucide-react";
import { Popover } from "radix-ui";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { callsService } from "@/services/calling/calls.service";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { useCalling } from "@/context/CallingContext";
import { callingKeys, useCall, useCallActions, useCallHistory, useCallingIdentity, useCallingMutation } from "@/hooks/calling";
import { usePrincipalNames } from "@/hooks/calling/usePrincipalNames";
import { identityApi } from "@/lib/messaging/api";
import { callingErrorMessage } from "@/lib/calling/errors";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import { activeParticipant, isWindingDown, ownParticipant, ownSession, visibleControls } from "@/lib/calling/call-permissions";
import { hubCallsService, type HubContact } from "@/services/calling/hub-calls.service";
import type { CallKind, CallState, IvrInputRequest } from "@/types/calling";
import type { CallHistoryEntry } from "@/types/calling/history";
import CallRecordingControls from "./CallRecordingControls";
import AbuseReportDialog from "./AbuseReportDialog";
import VoicemailInbox from "./VoicemailInbox";
import { CALL_SETTINGS_LINKS } from "./CallingNavigation";
import { rememberCallName } from "@/lib/calling/call-names";
import { launchStep } from "@/lib/calling/launch";

/** Calling's group-call target bound (`target_principal_ids` ≤ 64; deployments may set a lower maximum). */
const MAX_GROUP_TARGETS = 64;

const callStatePresentation: Record<CallState, { title: string; description: string }> = {
  REQUESTED: { title: "Calling…", description: "Placing your call." },
  AUTHORIZED: { title: "Calling…", description: "Finding the best way to reach them." },
  RESOLVING: { title: "Calling…", description: "Finding the best way to reach them." },
  ROUTING: { title: "Calling…", description: "Finding the best way to reach them." },
  DIALING: { title: "Calling…", description: "Waiting for their device." },
  RINGING: { title: "Ringing…", description: "Their device is ringing." },
  ANSWERED: { title: "Connecting…", description: "They answered. Connecting audio." },
  CONNECTED: { title: "Connected", description: "" },
  HELD: { title: "On hold", description: "Nobody can hear each other until someone resumes." },
  TRANSFERRING: { title: "Transferring…", description: "Passing the call on." },
  ENDING: { title: "Ending…", description: "" },
  ENDED: { title: "Call ended", description: "" },
  DECLINED: { title: "Declined", description: "They declined the call." },
  BUSY: { title: "Busy", description: "They're on another call." },
  NO_ANSWER: { title: "No answer", description: "Nobody picked up." },
  FAILED: { title: "Call failed", description: "The call couldn't be completed." },
  CANCELLED: { title: "Cancelled", description: "The call was cancelled before anyone answered." },
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

const deviceStatus: Record<string, { text: string; tone: string }> = {
  online: { text: "Ready for calls", tone: "bg-emerald-500" },
  connecting: { text: "Connecting…", tone: "bg-amber-500 animate-pulse" },
  reconnecting: { text: "Reconnecting…", tone: "bg-amber-500 animate-pulse" },
  offline: { text: "Offline", tone: "bg-muted-foreground" },
  disabled: { text: "Not receiving calls here", tone: "bg-muted-foreground" },
};

const AVATAR_COLORS = ["#2563eb", "#059669", "#d97706", "#7c3aed", "#db2777", "#0891b2", "#4f46e5", "#ea580c"];
const colorFor = (key: string) => AVATAR_COLORS[([...key].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % AVATAR_COLORS.length];
const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";
function Avatar({ name, seed, size = "size-11", className }: { name: string; seed?: string; size?: string; className?: string }) {
  return <span aria-hidden className={cn("flex shrink-0 items-center justify-center rounded-full text-sm font-semibold text-white", size, className)} style={{ backgroundColor: colorFor(seed ?? name) }}>{initials(name)}</span>;
}

const entryName = (entry: CallHistoryEntry) => entry.counterparts.map((p) => p.displayName || "CF Meet user").join(", ") || entry.enterprise?.entryLabel || "Call";
function dayLabel(iso: string) {
  const d = new Date(iso), today = new Date(), yesterday = new Date();
  yesterday.setDate(today.getDate() - 1);
  if (d.toDateString() === today.toDateString()) return "Today";
  if (d.toDateString() === yesterday.toDateString()) return "Yesterday";
  return d.toLocaleDateString(undefined, today.getTime() - d.getTime() < 6 * 86400000 ? { weekday: "long" } : { weekday: "short", day: "numeric", month: "short" });
}
const clock = (iso: string) => new Date(iso).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
const outcomeText: Record<string, string> = {
  ANSWERED: "", NOT_ANSWERED: "No answer", MISSED: "Missed", DECLINED: "Declined", ANSWERED_ELSEWHERE: "Answered on another device", RINGING: "Ringing", NOT_REACHED: "Couldn't connect", IN_PROGRESS: "In progress",
};

type Launch = ({ contactId: string } | { conversationId: string }) & { kind: CallKind; name: string | null };

export default function CallsContent({ initialCallId = "", launch = null }: { initialCallId?: string; launch?: Launch | null }) {
  const identity = useCallingIdentity();
  const calling = useCalling();
  // Calling starts a moment after the page: show loading, not "not available", until it has.
  if (identity.isPending || (!identity.error && !calling)) return <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground"><Spinner /> Loading calls…</div>;
  if (identity.error || !calling) {
    return (
      <div className="flex h-full flex-col items-center justify-center px-6 text-center">
        <span className="flex size-14 items-center justify-center rounded-2xl bg-muted text-muted-foreground"><PhoneOff className="size-6" /></span>
        <h1 className="mt-4 text-lg font-semibold">Calls aren&apos;t available right now</h1>
        <p role="alert" className="mt-1 max-w-sm text-sm text-muted-foreground">{identity.error ? callingErrorMessage(identity.error) : "Calling is starting…"}</p>
        <button type="button" onClick={() => void identity.refetch()} className="mt-4 h-10 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90">Try again</button>
      </div>
    );
  }
  return <CallsWorkspace key={initialCallId} initialCallId={initialCallId} launch={launch} />;
}

type Runner = (action: () => Promise<unknown>) => Promise<void>;

type ListTab = "all" | "missed" | "voicemail";

function CallsWorkspace({ initialCallId, launch }: { initialCallId: string; launch: Launch | null }) {
  const calling = useCalling()!;
  const { principalId, device } = calling;
  const [selected, setSelected] = useState(initialCallId);
  const [tab, setTab] = useState<ListTab>("all");
  const [search, setSearch] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // Phones show one screen at a time: the list, or the dialer / a call.
  const [dialerOpen, setDialerOpen] = useState(false);
  const history = useCallHistory(tab === "missed" ? { missed: true } : {}, tab !== "voicemail");
  const entries = (history.data?.pages.flatMap((page) => page.data) ?? []).filter((e) => !search.trim() || entryName(e).toLowerCase().includes(search.trim().toLowerCase()));
  const run: Runner = async (action) => {
    setError(null); setBusy(true);
    try { await action(); } catch (e) { setError(callingErrorMessage(e)); } finally { setBusy(false); }
  };
  const canCall = !busy && !calling.media && !!device.endpoint;
  const status = deviceStatus[device.status] ?? { text: device.status, tone: "bg-muted-foreground" };
  const open = (id: string) => { setSelected(id); setDialerOpen(false); };
  const callBack = (entry: CallHistoryEntry, kind: CallKind) => run(async () => {
    const ids = entry.counterparts.map((p) => p.principalId).filter(Boolean);
    if (!ids.length) throw new Error("This call can't be returned from here.");
    open(await calling.startCall(kind, { targetPrincipalIds: ids }, entryName(entry)));
  });
  const rightVisible = !!selected || dialerOpen;

  // Arriving from a Call button (Contacts, Messages): the call screen shows at once with the
  // name; the call is placed as soon as this browser is connected (unless cancelled meanwhile).
  // The launch is remembered here: the address bar drops `?contactId=` at once (lib/calling/launch.ts).
  const router = useRouter();
  const pendingLaunch = useRef<Launch | null>(null);
  const sawPlacing = useRef(false);
  const announcedLaunch = useRef(false);
  useEffect(() => {
    if (!launch || announcedLaunch.current || calling.media) return;
    announcedLaunch.current = true;
    pendingLaunch.current = launch;
    calling.announce(launch.kind, launch.name);
    router.replace("/calling");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [launch]);
  useEffect(() => {
    const decision = launchStep({ pending: !!pendingLaunch.current, placing: !!calling.placing, sawPlacing: sawPlacing.current,
      endpointReady: !!device.endpoint, inCall: !!calling.media });
    sawPlacing.current = decision.sawPlacing;
    if (decision.step === "drop") { pendingLaunch.current = null; return; } // cancelled before it started
    if (decision.step !== "place" || !pendingLaunch.current) return;
    const placed = pendingLaunch.current;
    pendingLaunch.current = null;
    void run(async () => {
      const label = placed.name ?? undefined;
      const id = "contactId" in placed ? await calling.callContact(placed.contactId, placed.kind, label) : await calling.callConversation(placed.conversationId, placed.kind, label);
      rememberCallName(id, placed.name);
      open(id);
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [launch, device.endpoint, calling.media, calling.placing]);

  return (
    <div className="flex h-full min-h-[32rem] flex-col bg-background">
      {error || device.error ? <p role="alert" className="flex items-center gap-2 bg-destructive/10 px-4 py-2 text-sm text-destructive">{error || device.error}<button type="button" aria-label="Dismiss" onClick={() => setError(null)} className="ml-auto rounded-full p-1 hover:bg-destructive/10"><X className="size-4" /></button></p> : null}
      <div className="flex min-h-0 flex-1">
        <aside className={cn("flex w-full min-h-0 flex-col border-r border-border md:w-[22rem] md:shrink-0", rightVisible && "hidden md:flex")} aria-label="Calls">
          <div className="space-y-3 px-4 pb-2 pt-4">
            <div className="flex items-start justify-between gap-2">
              <div className="min-w-0">
                <h1 className="text-2xl font-bold tracking-tight">Calls</h1>
                {device.status !== "online" ? <p role="status" className="mt-0.5 flex items-center gap-1.5 text-xs text-muted-foreground"><span className={cn("size-2 rounded-full", status.tone)} />{status.text}
                  {device.status === "disabled" ? <button type="button" onClick={() => void device.setReceiving(true)} className="font-semibold text-primary hover:underline">· Turn on</button> : null}
                  {device.status === "offline" ? <button type="button" onClick={() => void device.register()} className="inline-flex items-center gap-1 font-semibold text-primary hover:underline">· <RefreshCw className="size-3" />Reconnect</button> : null}
                </p> : null}
              </div>
              <div className="flex shrink-0 items-center gap-1">
                <Popover.Root>
                  <Popover.Trigger asChild><button type="button" aria-label="Call settings" title="Call settings" className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><Settings2 className="size-5" /></button></Popover.Trigger>
                  <Popover.Portal>
                    <Popover.Content align="end" sideOffset={4} className="z-50 w-60 rounded-xl border border-border bg-popover p-1.5 shadow-xl">
                      <p className="px-3 pb-1 pt-1.5 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground">Call settings</p>
                      {CALL_SETTINGS_LINKS.map(([label, href]) => <Link key={href} href={href} className="flex w-full items-center rounded-lg px-3 py-2.5 text-sm hover:bg-muted">{label}</Link>)}
                    </Popover.Content>
                  </Popover.Portal>
                </Popover.Root>
                <button type="button" onClick={() => { setSelected(""); setDialerOpen(true); }} aria-label="New call" title="New call" className={cn("hidden size-10 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-sm hover:bg-primary/90", selected && "md:flex")}><PhoneCall className="size-5" /></button>
              </div>
            </div>
            <div role="tablist" aria-label="Filter calls" className="flex gap-2">
              {([["all", "All"], ["missed", "Missed"], ["voicemail", "Voicemail"]] as const).map(([value, label]) => (
                <button key={value} role="tab" type="button" aria-selected={tab === value} onClick={() => setTab(value)} className={cn("h-8 rounded-full px-3.5 text-sm font-medium transition-colors", tab === value ? "bg-primary/15 text-primary" : "bg-muted/70 text-muted-foreground hover:text-foreground")}>{label}</button>
              ))}
            </div>
            {tab !== "voicemail" && (
              <label className="flex h-10 items-center gap-2 rounded-full bg-muted/60 px-3.5 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
                <Search className="size-4 shrink-0" />
                <input type="search" aria-label="Search calls" placeholder="Search calls" value={search} onChange={(e) => setSearch(e.target.value)} className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground" />
              </label>
            )}
          </div>

          {tab === "voicemail" ? (
            <div className="no-scrollbar min-h-0 flex-1 overflow-y-auto px-4 pb-4"><VoicemailInbox /></div>
          ) : (
            <ul className="no-scrollbar min-h-0 flex-1 overflow-y-auto px-2 pb-2">
              {history.isPending && [0, 1, 2, 3].map((i) => <li key={i} className="flex animate-pulse items-center gap-3 px-3 py-3"><span className="size-11 rounded-full bg-muted" /><span className="flex-1 space-y-2"><span className="block h-3.5 w-1/2 rounded-full bg-muted" /><span className="block h-3 w-1/3 rounded-full bg-muted/70" /></span></li>)}
              {history.error ? <li role="alert" className="px-3 py-4 text-sm text-destructive">{callingErrorMessage(history.error)}</li> : null}
              {!history.isPending && !history.error && entries.length === 0 ? (
                <li className="flex flex-col items-center px-6 py-14 text-center">
                  <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary">{tab === "missed" ? <PhoneMissed className="size-6" /> : <Phone className="size-6" />}</span>
                  <p className="mt-4 font-semibold">{search ? "No calls match" : tab === "missed" ? "No missed calls" : "No calls yet"}</p>
                  <p className="mt-1 text-sm text-muted-foreground">{search ? "Try another name." : tab === "missed" ? "You haven't missed anyone." : "Calls you make and get show up here."}</p>
                </li>
              ) : null}
              {entries.map((entry, i) => {
                const prev = entries[i - 1];
                const newDay = !prev || new Date(prev.createdAt).toDateString() !== new Date(entry.createdAt).toDateString();
                const name = entryName(entry);
                const isMissed = entry.participation.missed;
                const inbound = entry.participation.direction === "INBOUND";
                const DirIcon = isMissed ? PhoneMissed : inbound ? PhoneIncoming : PhoneOutgoing;
                const group = entry.counterparts.length > 1;
                // A finished call can still report RINGING (the server leaves invitations open after a cancel): show what really happened.
                const finished = ["ENDED", "CANCELLED", "FAILED", "NO_ANSWER", "DECLINED", "REJECTED", "BUSY"].includes(entry.state);
                const outcome = finished && (entry.participation.outcome === "RINGING" || entry.participation.outcome === "IN_PROGRESS") ? (inbound ? "MISSED" : "NOT_ANSWERED") : entry.participation.outcome;
                // CF1: an outgoing call that ended BUSY reached someone who was on another call.
                const outcomeLabel = !inbound && entry.state === "BUSY" ? "On another call" : outcomeText[outcome] ?? "";
                const detail = [outcomeLabel, entry.durationMs ? formatCallDuration(entry.durationMs) : ""].filter(Boolean).join(" · ");
                return (
                  <Fragment key={entry.callId}>
                    {newDay && <li className="px-3 pb-1 pt-3 text-xs font-semibold uppercase tracking-wider text-muted-foreground">{dayLabel(entry.createdAt)}</li>}
                    <li className="group relative">
                      <button type="button" onClick={() => open(entry.callId)} aria-pressed={selected === entry.callId} className={cn("flex w-full items-center gap-3 rounded-2xl px-2.5 py-2.5 text-left transition-colors", selected === entry.callId ? "bg-primary/10" : "hover:bg-muted/70")}>
                        {group ? <span className="flex size-12 shrink-0 items-center justify-center rounded-full bg-muted text-muted-foreground"><Users className="size-5" /></span> : <Avatar name={name} seed={entry.counterparts[0]?.principalId ?? name} size="size-12" />}
                        <span className="min-w-0 flex-1 pr-12">
                          <span className={cn("block truncate text-[15px] font-semibold", isMissed && "text-destructive")}>{name}</span>
                          <span className="flex items-center gap-1.5 text-sm text-muted-foreground">
                            <DirIcon className={cn("size-3.5 shrink-0", isMissed && "text-destructive")} />
                            {entry.kind === "VIDEO" ? <Video className="size-3.5 shrink-0" aria-label="Video" /> : null}
                            {entry.voicemail ? <Voicemail className="size-3.5 shrink-0 text-primary" aria-label="Left a voicemail" /> : null}
                            <span className="truncate">{clock(entry.createdAt)}{detail ? ` · ${detail}` : ""}</span>
                          </span>
                        </span>
                      </button>
                      <button type="button" aria-label={`Call ${name} back`} title="Call back" disabled={!canCall} onClick={() => void callBack(entry, entry.kind)}
                        className="absolute right-2 top-1/2 flex size-11 -translate-y-1/2 items-center justify-center rounded-full text-primary hover:bg-primary/10 disabled:opacity-30">
                        {entry.kind === "VIDEO" ? <Video className="size-5" /> : <Phone className="size-5" />}
                      </button>
                    </li>
                  </Fragment>
                );
              })}
              {history.hasNextPage ? <li className="py-2 text-center"><button type="button" disabled={history.isFetchingNextPage} onClick={() => void history.fetchNextPage()} className="h-9 rounded-full px-4 text-sm font-semibold text-primary hover:bg-primary/10 disabled:opacity-50">{history.isFetchingNextPage ? "Loading…" : "Show older calls"}</button></li> : null}
            </ul>
          )}
          {/* WhatsApp-style floating "new call" button on phones. */}
          <button type="button" onClick={() => { setSelected(""); setDialerOpen(true); }} aria-label="New call" title="New call"
            className="fixed bottom-24 right-4 z-30 flex size-14 items-center justify-center rounded-2xl bg-primary text-primary-foreground shadow-xl shadow-primary/30 hover:bg-primary/90 md:hidden">
            <PhoneCall className="size-6" />
          </button>
        </aside>

        <main className={cn("min-w-0 min-h-0 flex-1 overflow-y-auto", !rightVisible && "hidden md:block")}>
          {selected ? (
            <CallDetails callId={selected} principalId={principalId} busy={busy} run={run} onBack={() => { setSelected(""); router.replace("/calling"); }} />
          ) : (
            <Dialer busy={busy} canCall={canCall} run={run} onStarted={open} onBack={() => setDialerOpen(false)} />
          )}
        </main>
      </div>
    </div>
  );
}

const KEYS = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "*", "0", "#"];

/**
 * New call (WhatsApp "Select contact"): one list of people, each with a voice and a video button.
 * Extras on top: add a contact, a group call (pick several), or dial an extension.
 */
function Dialer({ busy, canCall, run, onStarted, onBack }: { busy: boolean; canCall: boolean; run: Runner; onStarted: (callId: string) => void; onBack: () => void }) {
  const calling = useCalling()!;
  const [filter, setFilter] = useState("");
  const [groupMode, setGroupMode] = useState(false);
  const [picked, setPicked] = useState<string[]>([]);
  const [keypad, setKeypad] = useState(false);
  const [extension, setExtension] = useState("");
  const directory = useQuery({ queryKey: ["calling", "directory", calling.principalId], queryFn: () => identityApi.directory(), retry: false });
  const contacts = useQuery({ queryKey: ["calling", "hub-contacts"], queryFn: () => hubCallsService.contacts(), retry: false, staleTime: 60_000 });
  const q = filter.trim().toLowerCase();
  const myContacts = (contacts.data?.data ?? [])
    .filter((c) => (c.alias || c.displayName).toLowerCase().includes(q))
    .sort((x, y) => Number(y.favorite) - Number(x.favorite) || (x.alias || x.displayName).localeCompare(y.alias || y.displayName));
  const people = useMemo(() => (directory.data?.people ?? []).filter((p) => p.principalId !== calling.principalId), [directory.data, calling.principalId]);
  const colleagues = people.filter((p) => p.displayName.toLowerCase().includes(q)).slice(0, 50);
  const toggle = (userId: string) => setPicked((current) => current.includes(userId) ? current.filter((id) => id !== userId) : current.length >= MAX_GROUP_TARGETS ? current : [...current, userId]);

  const principalsFor = async (userIds: string[]) => {
    const chosen = people.filter((p) => userIds.includes(p.userId));
    const missing = chosen.filter((p) => !p.principalId).map((p) => p.userId);
    const resolved = missing.length ? (await identityApi.resolve(missing)).people : [];
    const ids = chosen.map((p) => p.principalId ?? resolved.find((r) => r.userId === p.userId)?.principalId).filter((id): id is string => !!id);
    if (!ids.length || ids.length !== chosen.length) throw new Error("Some of the people you chose can't receive calls yet.");
    return ids;
  };
  const callContact = (contact: HubContact, kind: CallKind) => run(async () => {
    const id = await calling.callContact(contact.contactId, kind, contact.alias || contact.displayName);
    rememberCallName(id, contact.alias || contact.displayName);
    onStarted(id);
  });
  const callPeople = (userIds: string[], kind: CallKind, name?: string) => run(async () => {
    const id = await calling.startCall(kind, { targetPrincipalIds: await principalsFor(userIds) }, name);
    if (name) rememberCallName(id, name);
    onStarted(id); setPicked([]); setGroupMode(false);
  });
  const callExtension = () => run(async () => { onStarted(await calling.startCall("VOICE", { targetExtension: extension.trim() })); setExtension(""); });

  const iconBtn = "flex size-10 shrink-0 items-center justify-center rounded-full text-primary hover:bg-primary/10 disabled:opacity-30";
  const action = "flex w-full items-center gap-3 rounded-xl px-2 py-2.5 text-left hover:bg-muted/60";
  const actionIcon = "flex size-11 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground";
  const callButtons = (onVoice: () => void, onVideo: () => void, name: string) => (
    <>
      <button type="button" aria-label={`Voice call ${name}`} title="Voice call" disabled={!canCall} onClick={onVoice} className={iconBtn}><Phone className="size-5" /></button>
      <button type="button" aria-label={`Video call ${name}`} title="Video call" disabled={!canCall} onClick={onVideo} className={iconBtn}><Video className="size-5" /></button>
    </>
  );

  return (
    <div className="mx-auto flex min-h-full max-w-2xl flex-col">
      <div className="sticky top-0 z-10 space-y-3 bg-background px-4 pb-3 pt-4 sm:px-6">
        <div className="flex items-center gap-1">
          <button type="button" onClick={onBack} aria-label="Back to calls" className="-ml-2 flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted md:hidden"><ArrowLeft className="size-5" /></button>
          <div className="min-w-0">
            <h2 className="text-xl font-bold tracking-tight">{groupMode ? "New group call" : "New call"}</h2>
            {groupMode ? <p className="text-xs text-muted-foreground">{picked.length ? `${picked.length} selected` : "Choose who to call"}</p> : null}
          </div>
          {groupMode ? <button type="button" onClick={() => { setGroupMode(false); setPicked([]); }} className="ml-auto h-9 rounded-xl px-3 text-sm font-semibold text-primary hover:bg-primary/10">Cancel</button> : null}
        </div>
        <label className="flex h-11 items-center gap-2 rounded-xl bg-muted/70 px-3.5 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
          <Search className="size-4 shrink-0" />
          <input value={filter} placeholder="Search name" onChange={(e) => setFilter(e.target.value)} aria-label="Search people" className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground" />
        </label>
        {calling.media ? <p className="text-xs text-muted-foreground">You&apos;re on a call. Hang up to start another.</p> : !calling.device.endpoint ? <p className="text-xs text-muted-foreground">This browser isn&apos;t connected for calls yet.</p> : null}
      </div>

      <div className="flex-1 px-2 pb-28 sm:px-4">
        {!groupMode && !q ? (
          <div className="pb-2">
            <Link href="/dashboard/contacts?add=1" className={action}><span className={actionIcon}><UserPlus className="size-5" /></span><span className="font-medium">New contact</span></Link>
            {people.length > 1 ? <button type="button" onClick={() => setGroupMode(true)} className={action}><span className={actionIcon}><Users className="size-5" /></span><span className="font-medium">New group call</span></button> : null}
            <button type="button" onClick={() => setKeypad(!keypad)} aria-expanded={keypad} className={action}><span className={actionIcon}><Grid3x3 className="size-5" /></span><span className="font-medium">Call an extension</span></button>
            {keypad ? (
              <div className="mx-2 mb-2 flex flex-col items-center rounded-2xl bg-muted/40 p-4">
                <div className="flex h-12 w-full max-w-xs items-center gap-2 border-b border-border">
                  <input autoFocus value={extension} maxLength={32} placeholder="Extension" aria-label="Extension" onChange={(e) => setExtension(e.target.value.replace(/[^0-9*#]/g, ""))}
                    className="min-w-0 flex-1 bg-transparent text-center text-2xl font-light tracking-[0.15em] tabular-nums outline-none placeholder:text-base placeholder:tracking-normal placeholder:text-muted-foreground" />
                  {extension ? <button type="button" aria-label="Delete digit" onClick={() => setExtension(extension.slice(0, -1))} className="flex size-9 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><Delete className="size-5" /></button> : null}
                </div>
                <div className="mt-3 grid w-full max-w-[15rem] grid-cols-3 place-items-center gap-y-2">
                  {KEYS.map((k) => <button key={k} type="button" onClick={() => setExtension((e) => (e + k).slice(0, 32))} className="flex size-14 items-center justify-center rounded-full bg-background text-xl tabular-nums hover:bg-muted active:bg-primary/15">{k}</button>)}
                </div>
                <button type="button" disabled={!canCall || !extension.trim()} onClick={() => void callExtension()} aria-label="Call extension" className="mt-3 flex size-14 items-center justify-center rounded-full bg-[#1E8E3E] text-white hover:bg-[#188038] disabled:opacity-40">{busy ? <Spinner /> : <Phone className="size-6" />}</button>
              </div>
            ) : null}
          </div>
        ) : null}

        {!groupMode && myContacts.length > 0 ? (
          <section>
            <p className="px-2 pb-1 pt-3 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Contacts</p>
            <ul>
              {myContacts.map((c) => {
                const name = c.alias || c.displayName;
                return (
                  <li key={c.contactId} className="flex items-center gap-3 rounded-xl px-2 py-2 hover:bg-muted/40">
                    <Link href={`/dashboard/contacts?contact=${encodeURIComponent(c.contactId)}`} aria-label={`View ${name}'s contact info`} className="flex min-w-0 flex-1 items-center gap-3">
                      <Avatar name={name} seed={c.contactId} size="size-11" />
                      <span className="flex min-w-0 flex-1 items-center gap-1 truncate font-medium">{name}{c.favorite ? <Star className="size-3.5 shrink-0 fill-amber-400 text-amber-400" aria-label="Favorite" /> : null}</span>
                    </Link>
                    {callButtons(() => void callContact(c, "VOICE"), () => void callContact(c, "VIDEO"), name)}
                  </li>
                );
              })}
            </ul>
          </section>
        ) : null}

        {colleagues.length > 0 ? (
          <section>
            <p className="px-2 pb-1 pt-3 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Colleagues</p>
            <ul aria-label="Colleagues">
              {colleagues.map((p) => {
                const on = picked.includes(p.userId);
                return (
                  <li key={p.userId}>
                    {groupMode ? (
                      <label className="flex cursor-pointer items-center gap-3 rounded-xl px-2 py-2 hover:bg-muted/40">
                        <input type="checkbox" className="sr-only" checked={on} onChange={() => toggle(p.userId)} />
                        <Avatar name={p.displayName} seed={p.userId} size="size-11" />
                        <span className="min-w-0 flex-1 truncate font-medium">{p.displayName}</span>
                        <span className={cn("flex size-6 shrink-0 items-center justify-center rounded-full border-2", on ? "border-primary bg-primary text-primary-foreground" : "border-muted-foreground/40")}>{on && <Check className="size-3.5" strokeWidth={3} />}</span>
                      </label>
                    ) : (
                      <div className="flex items-center gap-3 rounded-xl px-2 py-2 hover:bg-muted/40">
                        <Avatar name={p.displayName} seed={p.userId} size="size-11" />
                        <span className="min-w-0 flex-1 truncate font-medium">{p.displayName}</span>
                        {callButtons(() => void callPeople([p.userId], "VOICE", p.displayName), () => void callPeople([p.userId], "VIDEO", p.displayName), p.displayName)}
                      </div>
                    )}
                  </li>
                );
              })}
            </ul>
          </section>
        ) : null}

        {contacts.isPending || directory.isPending ? [0, 1, 2].map((i) => <div key={i} className="flex animate-pulse items-center gap-3 px-2 py-2"><span className="size-11 rounded-full bg-muted" /><span className="h-3.5 w-32 rounded-full bg-muted" /></div>) : null}
        {!contacts.isPending && !directory.isPending && !myContacts.length && !colleagues.length ? (
          <div className="flex flex-col items-center px-6 py-12 text-center">
            <span className="flex size-12 items-center justify-center rounded-2xl bg-muted text-muted-foreground"><Users className="size-5" /></span>
            <p className="mt-3 font-semibold">{q ? `No one matches "${filter}"` : "No one to call yet"}</p>
            <p className="mt-1 max-w-xs text-sm text-muted-foreground">{q ? "Try another name." : "Add a contact with their CF Meet ID, then call them from here."}</p>
          </div>
        ) : null}
      </div>

      {groupMode && picked.length > 0 ? (
        <div className="sticky bottom-0 flex items-center justify-center gap-4 border-t border-border bg-background/95 px-4 py-4 backdrop-blur">
          <button type="button" disabled={!canCall} onClick={() => void callPeople(picked, "VOICE")} className="inline-flex h-12 items-center gap-2 rounded-xl bg-[#1E8E3E] px-6 text-sm font-semibold text-white hover:bg-[#188038] disabled:opacity-40">{busy ? <Spinner /> : <Phone className="size-5" />}Voice call</button>
          <button type="button" disabled={!canCall} onClick={() => void callPeople(picked, "VIDEO")} className="inline-flex h-12 items-center gap-2 rounded-xl bg-primary px-6 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-40"><Video className="size-5" />Video call</button>
        </div>
      ) : null}
    </div>
  );
}

function RoundControl({ label, onClick, disabled, children, tone = "default" }: { label: string; onClick: () => void; disabled?: boolean; children: React.ReactNode; tone?: "default" | "danger" | "go" }) {
  return (
    <button type="button" onClick={onClick} disabled={disabled} className="group flex w-20 flex-col items-center gap-1.5 disabled:opacity-40">
      <span className={cn("flex size-14 items-center justify-center rounded-full transition-colors [&_svg]:size-6",
        tone === "danger" ? "bg-destructive text-white group-hover:bg-destructive/90" : tone === "go" ? "bg-emerald-600 text-white group-hover:bg-emerald-700" : "bg-muted text-foreground group-hover:bg-muted/70")}>{children}</span>
      <span className="text-xs font-medium text-muted-foreground">{label}</span>
    </button>
  );
}

/** "Started from a chat" with a link back to it (Communications call context). Hidden when unknown. */
function CallOrigin({ callId }: { callId: string }) {
  const ctx = useQuery({ queryKey: ["call-context", callId], queryFn: () => communicationsService.callContext(callId), retry: false, staleTime: 60_000 });
  const c = ctx.data;
  if (!c || c.origin === "CALLING") return null;
  return (
    <div className="mt-6 flex w-full max-w-sm items-center gap-3 rounded-2xl border border-border px-4 py-3 text-left text-sm">
      <span className="min-w-0 flex-1 text-muted-foreground">{c.origin === "CONVERSATION" ? "Started from a chat" : "Started from Contacts"}</span>
      {c.conversationId && c.actions?.openConversation ? <Link href={`/dashboard/messages?conversation=${encodeURIComponent(c.conversationId)}`} className="shrink-0 font-semibold text-primary hover:underline">Open chat</Link> : null}
    </div>
  );
}

/** C8: how the call sounded for each person (estimated MOS: 4+ good, 3–4 fair, below 3 poor). */
function CallQuality({ callId, names }: { callId: string; names: (participantId: string) => string }) {
  const q = useQuery({ queryKey: ["call-quality", callId], queryFn: () => callsService.quality(callId), retry: false, staleTime: 60_000 });
  const rows = (q.data?.participants ?? []).filter((p) => p.sampleCount > 0);
  if (!rows.length) return null;
  const label = (mos: number) => mos >= 4 ? ["Good", "text-emerald-600"] : mos >= 3 ? ["Fair", "text-amber-600"] : ["Poor", "text-destructive"];
  return (
    <section className="mt-6 w-full max-w-sm text-left">
      <p className="mb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">Call quality</p>
      <ul className="divide-y divide-border rounded-2xl border border-border">
        {rows.map((p) => {
          const [text, tone] = label(p.mosEstimate.avg);
          return (
            <li key={p.participantId} className="flex items-center gap-3 px-3 py-2.5 text-sm">
              <span className="min-w-0 flex-1 truncate">{names(p.participantId)}</span>
              <span className={cn("font-semibold", tone)}>{text}</span>
              <span className="text-xs text-muted-foreground">{Math.round(p.packetLossPercent.avg * 10) / 10}% loss</span>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

function CallDetails({ callId, principalId, busy, run, onBack }: { callId: string; principalId: string; busy: boolean; run: Runner; onBack: () => void }) {
  const calling = useCalling()!;
  const call = useCall(callId);
  const actions = useCallActions(callId);
  const queryClient = useQueryClient();
  const mutation = useCallingMutation();
  const [now, setNow] = useState(() => Date.now());
  const [reporting, setReporting] = useState(false);
  const [keypad, setKeypad] = useState(false);
  const [transferTo, setTransferTo] = useState("");
  const unnamed = call.data?.participants.filter((p) => !p.displayName && p.principalId !== principalId) ?? [];
  const names = usePrincipalNames(unnamed.map((p) => p.principalId));
  const state = call.data?.state;

  useEffect(() => {
    if (state !== "CONNECTED") return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [state]);

  const back = <button type="button" onClick={onBack} aria-label="Close" title="Close" className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><ArrowLeft className="size-5 md:hidden" /><X className="hidden size-5 md:block" /></button>;
  if (call.isPending) return <div className="p-4">{back}<div className="flex justify-center py-20"><Spinner /></div></div>;
  if (call.error) return <div className="p-4">{back}<p role="alert" className="mx-auto mt-10 max-w-sm text-center text-sm text-destructive">{callingErrorMessage(call.error)}</p></div>;
  const data = call.data;
  if (!data) return null;

  const endpointId = calling.device.endpoint?.endpointId ?? null;
  const live = !isWindingDown(data.state);
  const available = (name: string) => !!actions.data?.actions.some((a) => a.action === name && a.available);
  const controls = visibleControls(data, principalId, available);
  const mine = activeParticipant(data, principalId);
  const everMine = ownParticipant(data, principalId);
  const session = ownSession(data, principalId, endpointId);
  const joinable = live && session && data.sessions.find((s) => s.sessionId === session.sessionId)?.mediaAccess === "AUTHORIZED" && calling.media?.sessionId !== session.sessionId;
  const durationAsOf = Date.parse(data.durationsAsOf);
  const liveDuration = data.state === "CONNECTED" && Number.isFinite(durationAsOf) ? (data.connectedDurationMs ?? 0) + Math.max(0, now - durationAsOf) : (data.connectedDurationMs ?? 0);
  const command = (operation: "cancel" | "end" | "hold" | "resume") => run(() => mutation.mutateAsync({ operation, callId, body: { expectedVersion: data.version }, key: newIdempotencyKey() }));
  const others = data.participants.filter((p) => p.principalId !== principalId);
  const otherName = (p: (typeof others)[number]) => p.displayName || names.data?.[p.principalId] || "CF Meet user";
  const title = others.length === 0 ? "Call" : others.length === 1 ? otherName(others[0]) : `${otherName(others[0])} and ${others.length - 1} more`;
  const presentation = callStatePresentation[data.state];
  // CF1: who is on another call (the caller's view); the busy person sees their missed call.
  const description = data.state !== "BUSY" ? presentation.description
    : data.initiatorPrincipalId !== principalId ? "You were on another call."
    : others.length === 1 ? `${otherName(others[0])} is on another call.`
    : "Everyone you called is on another call.";
  const ended = !live;
  const pending = busy || mutation.isPending;
  const sendDigit = (digit: string) => endpointId && run(() => mutation.mutateAsync({ operation: "ivrInput", callId, body: { endpointId, digit: digit as IvrInputRequest["digit"] } }));
  const keypadAllowed = !!endpointId && live && !!mine && data.initiatorPrincipalId === principalId;
  const menu = "flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm hover:bg-muted";
  const iAmHost = !!mine && data.hostParticipantId === mine.participantId;

  return (
    <div className="flex min-h-full flex-col">
      <div className="flex h-14 shrink-0 items-center gap-1 px-2 sm:px-4">
        {back}
        <div className="ml-auto flex items-center">
          <Popover.Root>
            <Popover.Trigger asChild><button type="button" aria-label="More" className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><MoreHorizontal className="size-5" /></button></Popover.Trigger>
            <Popover.Portal>
              <Popover.Content align="end" sideOffset={4} className="z-50 w-56 rounded-xl border border-border bg-popover p-1.5 shadow-xl">
                <Link href={`/calling/${encodeURIComponent(callId)}`} className={menu}><ExternalLink className="size-4 text-muted-foreground" />Full call details</Link>
                {everMine ? <Popover.Close asChild><button type="button" className={cn(menu, "text-destructive")} onClick={() => setReporting(true)}><Flag className="size-4" />Report this call</button></Popover.Close> : null}
                {ended ? <Popover.Close asChild><button type="button" className={cn(menu, "text-destructive")} onClick={() => void run(async () => { await callsService.hideFromHistory(callId); await queryClient.invalidateQueries({ queryKey: callingKeys.all }); onBack(); })}><Trash2 className="size-4" />Remove from call history</button></Popover.Close> : null}
              </Popover.Content>
            </Popover.Portal>
          </Popover.Root>
        </div>
      </div>

      <div className="mx-auto flex w-full max-w-lg flex-1 flex-col items-center px-6 pb-10 text-center">
        <div className="relative mt-4">
          {others.length > 1 ? <span className="flex size-28 items-center justify-center rounded-full bg-muted text-muted-foreground"><Users className="size-12" /></span> : <Avatar name={title} seed={others[0]?.principalId ?? title} size="size-28" className="text-4xl" />}
          {(data.state === "RINGING" || data.state === "DIALING") && <span className="absolute inset-0 animate-ping rounded-full ring-4 ring-primary/30" />}
        </div>
        <h2 className="mt-5 text-2xl font-semibold tracking-tight">{title}</h2>
        <p className="mt-1 text-sm text-muted-foreground">{others.length > 1 ? "Group call" : data.kind === "VIDEO" ? "Video call" : "Voice call"}</p>
        <p className={cn("mt-4 flex items-center gap-2 text-base font-semibold", data.state === "CONNECTED" ? "text-emerald-600 dark:text-emerald-400" : ended ? "text-muted-foreground" : "text-foreground")} role="status" aria-live="polite">
          {data.state === "CONNECTED" ? <span className="size-2 animate-pulse rounded-full bg-emerald-500" /> : null}
          {data.state === "CONNECTED" ? formatCallDuration(liveDuration) : presentation.title}
        </p>
        {description ? <p className="mt-1 text-sm text-muted-foreground">{description}</p> : null}
        {data.failureCategory ? <p className="mt-2 text-sm text-destructive">{data.failureCategory.replaceAll("_", " ").toLowerCase().replace(/^./, (c) => c.toUpperCase())}</p> : null}
        {data.state === "ENDED" && data.connectedDurationMs !== null ? <p className="mt-1 text-sm text-muted-foreground">Lasted {formatCallDuration(data.connectedDurationMs)}</p> : null}

        {/* Controls: the call you're on here is controlled from the floating call bar only. */}
        {live && calling.media?.callId === callId ? (
          <p className="mt-8 flex items-center gap-2 rounded-full bg-emerald-500/10 px-4 py-2 text-sm font-medium text-emerald-700 dark:text-emerald-300"><span className="size-2 animate-pulse rounded-full bg-emerald-500" />You&apos;re on this call. Use the call bar to mute or hang up.</p>
        ) : live ? (
          <div className="mt-8 flex flex-wrap justify-center gap-x-2 gap-y-4">
            {!mine && everMine && calling.device.endpoint ? <RoundControl label="Rejoin" tone="go" disabled={pending || !!calling.media} onClick={() => void run(async () => {
              const r = await callsService.rejoin(callId, everMine.participantId, calling.device.endpoint!.endpointId);
              await calling.joinMedia(callId, r.sessionId, data.kind);
            })}>{data.kind === "VIDEO" ? <Video /> : <Phone />}</RoundControl> : null}
            {joinable ? <RoundControl label="Join here" tone="go" disabled={pending || !!calling.media} onClick={() => void run(() => calling.joinMedia(callId, session!.sessionId, data.kind))}>{data.kind === "VIDEO" ? <Video /> : <Phone />}</RoundControl> : null}
            {controls?.hold ? <RoundControl label="Hold" disabled={pending} onClick={() => void command("hold")}><Pause /></RoundControl> : null}
            {controls?.resume ? <RoundControl label="Resume" disabled={pending} onClick={() => void command("resume")}><Play /></RoundControl> : null}
            {keypadAllowed ? <RoundControl label="Keypad" onClick={() => setKeypad(!keypad)}><Grid3x3 /></RoundControl> : null}
            {session ? <RoundControl label="Leave" disabled={pending} onClick={() => void run(async () => {
              await mutation.mutateAsync({ operation: "removeSession", callId, sessionId: session.sessionId, body: { expectedVersion: data.version }, key: newIdempotencyKey() });
              if (calling.media?.callId === callId) calling.leaveMedia();
            })}><PhoneOff /></RoundControl> : null}
            {controls?.cancel ? <RoundControl label="Cancel" tone="danger" disabled={pending} onClick={() => void command("cancel")}><PhoneOff /></RoundControl> : null}
            {controls?.end ? <RoundControl label="End for all" tone="danger" disabled={pending} onClick={() => void command("end")}><PhoneOff /></RoundControl> : null}
          </div>
        ) : others.length > 0 ? (
          <div className="mt-8 flex justify-center gap-2">
            <RoundControl label="Call back" tone="go" disabled={pending || !!calling.media || !calling.device.endpoint} onClick={() => void run(() => calling.startCall("VOICE", { targetPrincipalIds: others.map((p) => p.principalId) }))}><Phone /></RoundControl>
            <RoundControl label="Video" disabled={pending || !!calling.media || !calling.device.endpoint} onClick={() => void run(() => calling.startCall("VIDEO", { targetPrincipalIds: others.map((p) => p.principalId) }))}><Video /></RoundControl>
          </div>
        ) : null}

        {keypad && keypadAllowed ? (
          <div className="mt-6 grid w-full max-w-[15rem] grid-cols-3 place-items-center gap-y-2">
            {KEYS.map((k) => <button key={k} type="button" disabled={pending} onClick={() => void sendDigit(k)} className="flex size-14 items-center justify-center rounded-full bg-muted/70 text-2xl font-light tabular-nums hover:bg-muted active:bg-primary/15 disabled:opacity-50">{k}</button>)}
          </div>
        ) : null}

        {controls?.transfer && mine ? (
          <form className="mt-6 flex w-full max-w-sm gap-2" onSubmit={(e) => { e.preventDefault(); if (transferTo.trim()) void run(() => mutation.mutateAsync({ operation: "transfer", callId, key: newIdempotencyKey(), body: { transferringParticipantId: mine.participantId, expectedVersion: data.version, targetExtension: transferTo.trim() } })); }}>
            <input value={transferTo} onChange={(e) => setTransferTo(e.target.value)} aria-label="Transfer to extension" placeholder="Transfer to extension" maxLength={32} className="h-10 min-w-0 flex-1 rounded-full border border-border bg-background px-4 text-sm outline-none focus:border-primary" />
            <button type="submit" disabled={pending || !transferTo.trim()} className="h-10 rounded-full px-4 text-sm font-semibold text-primary hover:bg-primary/10 disabled:opacity-40">Transfer</button>
          </form>
        ) : null}

        {mine && live && data.canViewRecordings ? <div className="mt-6 w-full max-w-sm text-left"><CallRecordingControls callId={callId} participantId={mine.participantId} endpointId={session ? endpointId : null} live={live} /></div> : null}

        <CallOrigin callId={callId} />

        {ended ? <CallQuality callId={callId} names={(id) => { const p = data.participants.find((x) => x.participantId === id); return !p ? "Someone" : p.principalId === principalId ? "You" : otherName(p); }} /> : null}

        {/* People in the call */}
        <section className="mt-8 w-full max-w-sm text-left">
          <p className="mb-2 text-xs font-semibold uppercase tracking-wider text-muted-foreground">People · {data.participants.length}</p>
          <ul className="divide-y divide-border rounded-2xl border border-border">
            {data.participants.map((p) => {
              const label = p.principalId === principalId ? "You" : otherName(p);
              return (
                <li key={p.participantId} className="flex items-center gap-3 px-3 py-2.5">
                  <Avatar name={label} seed={p.principalId} size="size-8" className="text-xs" />
                  <span className="min-w-0 flex-1 truncate text-sm font-medium">{label}</span>
                  {data.hostParticipantId === p.participantId ? <span className="rounded-full bg-primary/10 px-2 py-0.5 text-[11px] font-semibold text-primary">Host</span> : null}
                  {data.mutedParticipantIds?.includes(p.participantId) ? <MicOff className="size-3.5 text-destructive" aria-label="Muted by the host" /> : null}
                  <span className="text-xs text-muted-foreground">{p.endedAt ? "Left" : p.role === "INITIATOR" ? "Caller" : "Invited"}</span>
                  {iAmHost && live && !p.endedAt && p.principalId !== principalId ? (
                    <Popover.Root>
                      <Popover.Trigger asChild><button type="button" aria-label={`Options for ${label}`} className="flex size-8 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><MoreHorizontal className="size-4" /></button></Popover.Trigger>
                      <Popover.Portal>
                        <Popover.Content align="end" sideOffset={4} className="z-50 w-48 rounded-xl border border-border bg-popover p-1.5 shadow-xl">
                          <Popover.Close asChild><button type="button" className={menu} onClick={() => void run(() => callsService.transferHost(callId, p.participantId, newIdempotencyKey()))}>Make host</button></Popover.Close>
                          <Popover.Close asChild><button type="button" className={menu} onClick={() => void run(() => callsService.muteParticipant(callId, p.participantId, !data.mutedParticipantIds?.includes(p.participantId), newIdempotencyKey()))}>{data.mutedParticipantIds?.includes(p.participantId) ? "Unmute" : "Mute"}</button></Popover.Close>
                          <Popover.Close asChild><button type="button" className={cn(menu, "text-destructive")} onClick={() => void run(() => callsService.removeParticipant(callId, p.participantId, newIdempotencyKey()))}>Remove from call</button></Popover.Close>
                        </Popover.Content>
                      </Popover.Portal>
                    </Popover.Root>
                  ) : null}
                </li>
              );
            })}
          </ul>
        </section>
        {actions.error ? <p className="mt-4 text-sm text-destructive">{callingErrorMessage(actions.error)}</p> : null}
      </div>
      <AbuseReportDialog callId={callId} open={reporting} onOpenChange={setReporting} />
    </div>
  );
}
