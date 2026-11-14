"use client";

/**
 * Calls (Teams / Zoom Phone / Google Voice patterns): All · Missed · Voicemail on the left with
 * one-tap call back; on the right a permanent dialer (people search or keypad, voice / video)
 * with starred contacts, or the selected call. In-call controls live in the floating call bar
 * (ActiveCallDock) only, so they are never shown twice.
 */
import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import Link from "next/link";
import {
  ArrowLeft, Check, Delete, ExternalLink, Plus, Settings2, Star, UserPlus, Voicemail, Flag, Grid3x3, MoreHorizontal, Pause, Phone, PhoneCall, PhoneIncoming, PhoneMissed, PhoneOff, PhoneOutgoing,
  Play, RefreshCw, Search, Users, Video, X,
} from "lucide-react";
import { Popover } from "radix-ui";
import { useQuery } from "@tanstack/react-query";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { useCalling, type CallTarget } from "@/context/CallingContext";
import { useCall, useCallActions, useCallHistory, useCallingIdentity, useCallingMutation } from "@/hooks/calling";
import { usePrincipalNames } from "@/hooks/calling/usePrincipalNames";
import { identityApi } from "@/lib/messaging/api";
import { callingErrorMessage } from "@/lib/calling/errors";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import { activeParticipant, isWindingDown, ownParticipant, ownSession, visibleControls } from "@/lib/calling/call-permissions";
import { hubCallsService } from "@/services/calling/hub-calls.service";
import type { CallKind, CallState, IvrInputRequest } from "@/types/calling";
import type { CallHistoryEntry } from "@/types/calling/history";
import CallRecordingControls from "./CallRecordingControls";
import AbuseReportDialog from "./AbuseReportDialog";
import VoicemailInbox from "./VoicemailInbox";
import { CALL_SETTINGS_LINKS } from "./CallingNavigation";
import { rememberCallName } from "@/lib/calling/call-names";

/** Calling's group-call target bound (`target_principal_ids` ≤ 64; deployments may set a lower maximum). */
const MAX_GROUP_TARGETS = 64;

const callStatePresentation: Record<CallState, { title: string; description: string }> = {
  REQUESTED: { title: "Calling…", description: "Placing your call." },
  AUTHORIZED: { title: "Calling…", description: "Finding the best way to reach them." },
  RESOLVING: { title: "Calling…", description: "Finding the best way to reach them." },
  ROUTING: { title: "Calling…", description: "Finding the best way to reach them." },
  DIALING: { title: "Dialing…", description: "Reaching their devices." },
  RINGING: { title: "Ringing…", description: "Waiting for them to answer." },
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

type Launch = { contactId: string; kind: CallKind } | { conversationId: string; kind: CallKind };

export default function CallsContent({ initialCallId = "", launch = null }: { initialCallId?: string; launch?: Launch | null }) {
  const identity = useCallingIdentity();
  const calling = useCalling();
  if (identity.isPending) return <div className="flex h-full items-center justify-center gap-2 text-sm text-muted-foreground"><Spinner /> Loading calls…</div>;
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
    open(await calling.startCall(kind, { targetPrincipalIds: ids }));
  });
  const rightVisible = !!selected || dialerOpen;

  // Arriving from a Call button (Contacts, Messages): place that call once this browser is ready.
  const router = useRouter();
  const launched = useRef(false);
  useEffect(() => {
    if (!launch || launched.current || !device.endpoint || calling.media) return;
    launched.current = true;
    void run(async () => {
      const id = "contactId" in launch ? await calling.callContact(launch.contactId, launch.kind) : await calling.callConversation(launch.conversationId, launch.kind);
      if ("contactId" in launch) {
        const contact = (await hubCallsService.contacts().catch(() => null))?.data.find((c) => c.contactId === launch.contactId);
        rememberCallName(id, contact?.alias || contact?.displayName);
      }
      open(id);
    });
    router.replace("/calling");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [launch, device.endpoint, calling.media]);

  return (
    <div className="flex h-full min-h-[32rem] flex-col bg-background">
      {error || device.error ? <p role="alert" className="flex items-center gap-2 bg-destructive/10 px-4 py-2 text-sm text-destructive">{error || device.error}<button type="button" aria-label="Dismiss" onClick={() => setError(null)} className="ml-auto rounded-full p-1 hover:bg-destructive/10"><X className="size-4" /></button></p> : null}
      <div className="flex min-h-0 flex-1">
        <aside className={cn("flex w-full min-h-0 flex-col border-r border-border md:w-[22rem] md:shrink-0", rightVisible && "hidden md:flex")} aria-label="Calls">
          <div className="space-y-3 px-4 pb-2 pt-4">
            <div className="flex items-start justify-between gap-2">
              <div className="min-w-0">
                <h1 className="text-2xl font-bold tracking-tight">Calls</h1>
                <p role="status" className="mt-0.5 flex items-center gap-1.5 text-xs text-muted-foreground"><span className={cn("size-2 rounded-full", status.tone)} />{status.text}
                  {device.status === "disabled" ? <button type="button" onClick={() => void device.setReceiving(true)} className="font-semibold text-primary hover:underline">· Turn on</button> : null}
                  {device.status === "offline" ? <button type="button" onClick={() => void device.register()} className="inline-flex items-center gap-1 font-semibold text-primary hover:underline">· <RefreshCw className="size-3" />Reconnect</button> : null}
                </p>
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
                <button type="button" onClick={() => { setSelected(""); setDialerOpen(true); }} aria-label="New call" title="New call" className={cn("flex size-10 items-center justify-center rounded-full bg-primary text-primary-foreground shadow-sm hover:bg-primary/90", !selected && "md:hidden")}><PhoneCall className="size-5" /></button>
              </div>
            </div>
            <div role="tablist" className="flex rounded-full bg-muted p-1">
              {([["all", "All"], ["missed", "Missed"], ["voicemail", "Voicemail"]] as const).map(([value, label]) => (
                <button key={value} role="tab" type="button" aria-selected={tab === value} onClick={() => setTab(value)} className={cn("h-8 flex-1 rounded-full text-sm font-medium transition-colors", tab === value ? "bg-background text-foreground shadow-sm" : "text-muted-foreground hover:text-foreground")}>{label}</button>
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
                const detail = [outcomeText[entry.participation.outcome] ?? "", entry.durationMs ? formatCallDuration(entry.durationMs) : ""].filter(Boolean).join(" · ");
                return (
                  <Fragment key={entry.callId}>
                    {newDay && <li className="px-3 pb-1 pt-3 text-xs font-semibold uppercase tracking-wider text-muted-foreground">{dayLabel(entry.createdAt)}</li>}
                    <li className="group relative">
                      <button type="button" onClick={() => open(entry.callId)} aria-pressed={selected === entry.callId} className={cn("flex w-full items-center gap-3 rounded-2xl px-2.5 py-2.5 text-left transition-colors", selected === entry.callId ? "bg-primary/10" : "hover:bg-muted/70")}>
                        {group ? <span className="flex size-11 shrink-0 items-center justify-center rounded-full bg-muted text-muted-foreground"><Users className="size-5" /></span> : <Avatar name={name} seed={entry.counterparts[0]?.principalId ?? name} />}
                        <span className="min-w-0 flex-1 pr-10">
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
                        className="absolute right-3 top-1/2 flex size-9 -translate-y-1/2 items-center justify-center rounded-full text-primary hover:bg-primary/10 disabled:opacity-30 md:opacity-0 md:group-hover:opacity-100 md:focus-visible:opacity-100">
                        {entry.kind === "VIDEO" ? <Video className="size-5" /> : <Phone className="size-5" />}
                      </button>
                    </li>
                  </Fragment>
                );
              })}
              {history.hasNextPage ? <li className="py-2 text-center"><button type="button" disabled={history.isFetchingNextPage} onClick={() => void history.fetchNextPage()} className="h-9 rounded-full px-4 text-sm font-semibold text-primary hover:bg-primary/10 disabled:opacity-50">{history.isFetchingNextPage ? "Loading…" : "Show older calls"}</button></li> : null}
            </ul>
          )}
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
 * The dialer (Zoom Phone pattern): search people or use the keypad, choose voice / video, call.
 * Starred contacts sit below as speed dial (Communications Hub launch, CAL-014).
 * Direct, group (several people) or extension calls go through the adapter (CAL-014 group calling).
 */
function Dialer({ busy, canCall, run, onStarted, onBack }: { busy: boolean; canCall: boolean; run: Runner; onStarted: (callId: string) => void; onBack: () => void }) {
  const calling = useCalling()!;
  const [mode, setMode] = useState<"people" | "keypad">("people");
  const [picked, setPicked] = useState<string[]>([]);
  const [filter, setFilter] = useState("");
  const [extension, setExtension] = useState("");
  const [kind, setKind] = useState<CallKind>("VOICE");
  const directory = useQuery({ queryKey: ["calling", "directory", calling.principalId], queryFn: () => identityApi.directory(), retry: false });
  const contacts = useQuery({ queryKey: ["calling", "hub-contacts"], queryFn: () => hubCallsService.contacts(), retry: false, staleTime: 60_000 });
  const people = useMemo(() => (directory.data?.people ?? []).filter((p) => p.principalId !== calling.principalId), [directory.data, calling.principalId]);
  const shown = people.filter((p) => p.displayName.toLowerCase().includes(filter.trim().toLowerCase())).slice(0, 50);
  const toggle = (userId: string) => setPicked((current) => current.includes(userId) ? current.filter((id) => id !== userId) : current.length >= MAX_GROUP_TARGETS ? current : [...current, userId]);
  const starred = contacts.data?.data ?? [];
  const canSubmit = canCall && (mode === "keypad" ? !!extension.trim() : picked.length > 0);

  const submit = () => run(async () => {
    let target: CallTarget;
    if (mode === "keypad") {
      target = { targetExtension: extension.trim() };
    } else {
      const chosen = people.filter((p) => picked.includes(p.userId));
      const missing = chosen.filter((p) => !p.principalId).map((p) => p.userId);
      const resolved = missing.length ? (await identityApi.resolve(missing)).people : [];
      const principalIds = chosen.map((p) => p.principalId ?? resolved.find((r) => r.userId === p.userId)?.principalId).filter((id): id is string => !!id);
      if (!principalIds.length || principalIds.length !== chosen.length) throw new Error("Some of the people you chose can't receive calls yet.");
      target = { targetPrincipalIds: principalIds };
    }
    onStarted(await calling.startCall(kind, target));
    setPicked([]); setExtension(""); setFilter("");
  });

  return (
    <div className="mx-auto flex min-h-full max-w-2xl flex-col px-4 py-5 sm:px-8 sm:py-8">
      <div className="mb-6 flex items-center gap-2">
        <button type="button" onClick={onBack} aria-label="Back to calls" className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted md:hidden"><ArrowLeft className="size-5" /></button>
        <h2 className="text-2xl font-bold tracking-tight">New call</h2>
      </div>

      {/* Speed dial: starred contacts, one tap (Teams favorites row). */}
      {starred.length >= 0 && (
        <section className="mb-6">
          <p className="mb-3 flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wider text-muted-foreground"><Star className="size-3.5" />Speed dial</p>
          <ul className="no-scrollbar -mx-1 flex gap-1 overflow-x-auto px-1 pb-1">
            {starred.map((contact) => {
              const name = contact.alias || contact.displayName;
              return (
                <li key={contact.contactId} className="w-20 shrink-0">
                  <Popover.Root>
                    <Popover.Trigger asChild>
                      <button type="button" disabled={!canCall} className="flex w-full flex-col items-center gap-1.5 rounded-2xl p-2 hover:bg-muted disabled:opacity-50">
                        <Avatar name={name} seed={contact.contactId} size="size-14" className="text-lg" />
                        <span className="w-full truncate text-center text-xs font-medium">{name.split(" ")[0]}</span>
                      </button>
                    </Popover.Trigger>
                    <Popover.Portal>
                      <Popover.Content sideOffset={4} className="z-50 flex gap-1 rounded-full border border-border bg-popover p-1 shadow-xl">
                        <Popover.Close asChild><button type="button" aria-label={`Voice call ${name}`} onClick={() => void run(async () => { const id = await calling.callContact(contact.contactId, "VOICE"); rememberCallName(id, name); onStarted(id); })} className="flex h-9 items-center gap-1.5 rounded-full px-3 text-sm font-semibold hover:bg-muted"><Phone className="size-4 text-emerald-600" />Voice</button></Popover.Close>
                        <Popover.Close asChild><button type="button" aria-label={`Video call ${name}`} onClick={() => void run(async () => { const id = await calling.callContact(contact.contactId, "VIDEO"); rememberCallName(id, name); onStarted(id); })} className="flex h-9 items-center gap-1.5 rounded-full px-3 text-sm font-semibold hover:bg-muted"><Video className="size-4 text-primary" />Video</button></Popover.Close>
                      </Popover.Content>
                    </Popover.Portal>
                  </Popover.Root>
                </li>
              );
            })}
            <li className="w-20 shrink-0">
              <Link href="/dashboard/contacts" className="flex w-full flex-col items-center gap-1.5 rounded-2xl p-2 hover:bg-muted">
                <span className="flex size-14 items-center justify-center rounded-full border-2 border-dashed border-muted-foreground/30 text-muted-foreground"><Plus className="size-5" /></span>
                <span className="text-xs font-medium text-muted-foreground">Add</span>
              </Link>
            </li>
          </ul>
        </section>
      )}

      <section className="flex flex-1 flex-col">
        <div className="flex items-center gap-2">
          <div role="tablist" className="flex flex-1 rounded-full bg-muted p-1">
            {([["people", "People", Users], ["keypad", "Keypad", Grid3x3]] as const).map(([value, label, Icon]) => (
              <button key={value} role="tab" type="button" aria-selected={mode === value} onClick={() => setMode(value)} className={cn("flex h-9 flex-1 items-center justify-center gap-1.5 rounded-full text-sm font-medium", mode === value ? "bg-background text-foreground shadow-sm" : "text-muted-foreground")}><Icon className="size-4" />{label}</button>
            ))}
          </div>
        </div>

        {mode === "people" ? (
          <>
            <label className="mt-4 flex h-11 items-center gap-2 rounded-full bg-muted/60 px-4 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
              <Search className="size-4 shrink-0" />
              <input value={filter} placeholder="Search colleagues" onChange={(e) => setFilter(e.target.value)} aria-label="Search colleagues" className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground" />
            </label>
            {picked.length > 0 && (
              <div className="mt-3 flex flex-wrap gap-1.5">
                {people.filter((p) => picked.includes(p.userId)).map((p) => (
                  <button key={p.userId} type="button" onClick={() => toggle(p.userId)} className="inline-flex h-8 items-center gap-1.5 rounded-full bg-primary/10 pl-1 pr-2.5 text-sm font-medium text-primary hover:bg-primary/15">
                    <Avatar name={p.displayName} seed={p.userId} size="size-6" className="text-[10px]" />{p.displayName}<X className="size-3.5" />
                  </button>
                ))}
              </div>
            )}
            <ul aria-label="Choose people to call" className="no-scrollbar mt-2 min-h-48 flex-1 overflow-y-auto">
              {directory.isPending ? [0, 1, 2].map((i) => <li key={i} className="flex animate-pulse items-center gap-3 px-2 py-2"><span className="size-9 rounded-full bg-muted" /><span className="h-3.5 w-32 rounded-full bg-muted" /></li>) : null}
              {directory.error ? <li className="px-2 py-4 text-sm text-muted-foreground">Colleagues couldn&apos;t load. <button className="font-semibold text-primary hover:underline" onClick={() => void directory.refetch()}>Try again</button></li> : null}
              {!directory.isPending && !directory.error && !shown.length ? (
                <li className="flex flex-col items-center px-4 py-10 text-center">
                  {people.length ? <p className="text-sm text-muted-foreground">No one matches &quot;{filter}&quot;.</p> : (
                    <>
                      <span className="flex size-12 items-center justify-center rounded-2xl bg-muted text-muted-foreground"><Users className="size-5" /></span>
                      <p className="mt-3 text-sm font-semibold">No colleagues to show</p>
                      <p className="mt-1 max-w-xs text-sm text-muted-foreground">People in your organizations appear here. Call your contacts from speed dial above.</p>
                      <Link href="/dashboard/contacts" className="mt-4 inline-flex h-9 items-center gap-1.5 rounded-full px-4 text-sm font-semibold text-primary ring-1 ring-primary/30 hover:bg-primary/10"><UserPlus className="size-4" />Add a contact</Link>
                    </>
                  )}
                </li>
              ) : null}
              {shown.map((person) => {
                const on = picked.includes(person.userId);
                return (
                  <li key={person.userId}>
                    <label className="flex cursor-pointer items-center gap-3 rounded-xl px-2 py-2 hover:bg-muted/60">
                      <input type="checkbox" className="sr-only" checked={on} onChange={() => toggle(person.userId)} />
                      <Avatar name={person.displayName} seed={person.userId} size="size-9" />
                      <span className="min-w-0 flex-1 truncate text-sm font-medium">{person.displayName}</span>
                      <span className={cn("flex size-5 shrink-0 items-center justify-center rounded-full border", on ? "border-primary bg-primary text-primary-foreground" : "border-muted-foreground/40")}>{on && <Check className="size-3.5" strokeWidth={3} />}</span>
                    </label>
                  </li>
                );
              })}
            </ul>
          </>
        ) : (
          <div className="flex flex-1 flex-col items-center pt-4">
            <div className="flex h-14 w-full max-w-xs items-center gap-2 border-b border-border">
              <input autoFocus value={extension} maxLength={32} placeholder="Extension" aria-label="Extension" onChange={(e) => setExtension(e.target.value.replace(/[^0-9*#]/g, ""))}
                className="min-w-0 flex-1 bg-transparent text-center text-3xl font-light tracking-[0.15em] tabular-nums outline-none placeholder:text-xl placeholder:tracking-normal placeholder:text-muted-foreground" />
              {extension ? <button type="button" aria-label="Delete digit" onClick={() => setExtension(extension.slice(0, -1))} className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><Delete className="size-5" /></button> : null}
            </div>
            <div className="mt-4 grid w-full max-w-[16rem] grid-cols-3 place-items-center gap-y-2">
              {KEYS.map((k) => <button key={k} type="button" onClick={() => setExtension((e) => (e + k).slice(0, 32))} className="flex size-16 items-center justify-center rounded-full bg-muted/70 text-2xl font-light tabular-nums transition-colors hover:bg-muted active:bg-primary/15">{k}</button>)}
            </div>
          </div>
        )}

        <div className="mt-4 flex items-center gap-2 border-t border-border pt-4">
          <div className="flex rounded-full bg-muted p-1">
            {([["VOICE", "Voice", Phone], ["VIDEO", "Video", Video]] as const).map(([value, label, Icon]) => (
              <button key={value} type="button" aria-pressed={kind === value} onClick={() => setKind(value)} className={cn("flex h-9 items-center gap-1.5 rounded-full px-3 text-sm font-medium", kind === value ? "bg-background text-foreground shadow-sm" : "text-muted-foreground")}><Icon className="size-4" />{label}</button>
            ))}
          </div>
          <button type="button" disabled={!canSubmit} onClick={() => void submit()} className="ml-auto inline-flex h-11 items-center gap-2 rounded-full bg-emerald-600 px-6 text-sm font-semibold text-white shadow-sm hover:bg-emerald-700 disabled:opacity-40">
            {busy ? <Spinner /> : picked.length > 1 && mode === "people" ? <Users className="size-4" /> : kind === "VIDEO" ? <Video className="size-4" /> : <Phone className="size-4" />}
            {mode === "people" && picked.length > 1 ? `Call ${picked.length} people` : "Call"}
          </button>
        </div>
        {calling.media ? <p className="mt-2 text-xs text-muted-foreground">You&apos;re on a call. Hang up to start another.</p> : !calling.device.endpoint ? <p className="mt-2 text-xs text-muted-foreground">Connect this browser to make calls.</p> : null}
      </section>
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

function CallDetails({ callId, principalId, busy, run, onBack }: { callId: string; principalId: string; busy: boolean; run: Runner; onBack: () => void }) {
  const calling = useCalling()!;
  const call = useCall(callId);
  const actions = useCallActions(callId);
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
  const ended = !live;
  const pending = busy || mutation.isPending;
  const sendDigit = (digit: string) => endpointId && run(() => mutation.mutateAsync({ operation: "ivrInput", callId, body: { endpointId, digit: digit as IvrInputRequest["digit"] } }));
  const keypadAllowed = !!endpointId && live && !!mine && data.initiatorPrincipalId === principalId;
  const menu = "flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm hover:bg-muted";

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
        {presentation.description ? <p className="mt-1 text-sm text-muted-foreground">{presentation.description}</p> : null}
        {data.failureCategory ? <p className="mt-2 text-sm text-destructive">{data.failureCategory.replaceAll("_", " ").toLowerCase().replace(/^./, (c) => c.toUpperCase())}</p> : null}
        {data.state === "ENDED" && data.connectedDurationMs !== null ? <p className="mt-1 text-sm text-muted-foreground">Lasted {formatCallDuration(data.connectedDurationMs)}</p> : null}

        {/* Controls: the call you're on here is controlled from the floating call bar only. */}
        {live && calling.media?.callId === callId ? (
          <p className="mt-8 flex items-center gap-2 rounded-full bg-emerald-500/10 px-4 py-2 text-sm font-medium text-emerald-700 dark:text-emerald-300"><span className="size-2 animate-pulse rounded-full bg-emerald-500" />You&apos;re on this call. Use the call bar to mute or hang up.</p>
        ) : live ? (
          <div className="mt-8 flex flex-wrap justify-center gap-x-2 gap-y-4">
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

        {mine && live ? <div className="mt-6 w-full max-w-sm text-left"><CallRecordingControls callId={callId} participantId={mine.participantId} endpointId={session ? endpointId : null} live={live} /></div> : null}

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
                  <span className="text-xs text-muted-foreground">{p.endedAt ? "Left" : p.role === "INITIATOR" ? "Caller" : "Invited"}</span>
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
