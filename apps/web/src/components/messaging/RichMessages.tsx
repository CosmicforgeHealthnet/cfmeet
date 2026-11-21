"use client";

/**
 * F4-F7 in private chats (WhatsApp): polls, locations, contact cards and link previews,
 * all inside the end-to-end encrypted content. The bubble part shows one; the composer part
 * (the paperclip menu, private chats only) creates one.
 */
import { useState } from "react";
import { BarChart3, Check, ExternalLink, FileText, Link2, MapPin, Paperclip, Search, UserRound, X } from "lucide-react";
import { Popover } from "radix-ui";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { mapUrl, type RichContent, type RichPayload } from "@/lib/messaging/rich-content";
import { cn } from "@/lib/utils";
import { useHubContacts } from "@/hooks/v2/useCommunications";
import { connectedContacts, contactLabel } from "@/lib/communications/comms-core";

const card = (mine: boolean) => cn("mt-1 block w-full min-w-52 rounded-xl p-2.5 text-left", mine ? "bg-white/15" : "bg-background/70");

/** One structured message inside its bubble. */
export function RichContentView({ rich, mine, onVote, onMessageContact }: {
  rich: RichContent;
  mine: boolean;
  onVote?: (choices: number[]) => void;
  onMessageContact?: (cfMeetId: string) => void;
}) {
  if (rich.kind === "link") {
    const p = rich.preview;
    return (
      <a href={p.url} target="_blank" rel="noopener noreferrer nofollow" className={cn(card(mine), "flex items-center gap-2.5 hover:opacity-90")} data-testid="link-preview">
        <Link2 className="size-4 shrink-0 opacity-70" />
        <span className="min-w-0"><span className="block truncate text-sm font-medium">{p.title ?? p.siteName ?? p.url}</span>{p.description ? <span className="line-clamp-2 block text-xs opacity-75">{p.description}</span> : null}<span className="block truncate text-xs opacity-70">{p.siteName ?? p.url}</span></span>
      </a>
    );
  }
  if (rich.kind === "location") {
    // A drawn map, not a map image: loading tiles would tell a map provider where this person is.
    return (
      <a href={mapUrl(rich.lat, rich.lng)} target="_blank" rel="noopener noreferrer nofollow" className={cn("mt-1 block w-64 max-w-full overflow-hidden rounded-xl hover:opacity-95", mine ? "bg-white/15" : "bg-background/70")} data-testid="location">
        <span aria-hidden className="relative block h-28 bg-[#E3ECDF] dark:bg-[#1F2A24]">
          <svg viewBox="0 0 256 112" preserveAspectRatio="xMidYMid slice" className="absolute inset-0 size-full">
            <path d="M0 78 C60 70 90 96 150 84 S230 60 256 66" fill="none" stroke="#fff" strokeOpacity=".9" strokeWidth="9" />
            <path d="M92 0 L118 112" fill="none" stroke="#fff" strokeOpacity=".9" strokeWidth="6" />
            <path d="M0 28 L256 40" fill="none" stroke="#fff" strokeOpacity=".7" strokeWidth="4" />
            <path d="M190 0 L176 112" fill="none" stroke="#fff" strokeOpacity=".7" strokeWidth="4" />
            <rect x="20" y="40" width="50" height="26" rx="4" fill="#C9DCC2" />
            <rect x="200" y="78" width="44" height="30" rx="4" fill="#BFD6E6" />
          </svg>
          <MapPin className="absolute left-1/2 top-1/2 size-8 -translate-x-1/2 -translate-y-full fill-red-500 text-white drop-shadow" />
        </span>
        <span className="flex items-center gap-2 px-2.5 py-2">
          <span className="min-w-0 flex-1"><span className="block truncate text-sm font-medium">{rich.label ?? "Shared location"}</span><span className="block text-xs opacity-70">Open in Maps</span></span>
          <ExternalLink className="size-4 shrink-0 opacity-60" />
        </span>
      </a>
    );
  }
  if (rich.kind === "contact") {
    return (
      <div className={cn(card(mine), "flex items-center gap-2.5")} data-testid="contact-card">
        <UserRound className="size-5 shrink-0" />
        <span className="min-w-0 flex-1"><span className="block truncate text-sm font-medium">{rich.name}</span><span className="block truncate text-xs opacity-70">CF Meet ID {rich.cfMeetId}</span></span>
        {!mine && onMessageContact ? <button type="button" onClick={() => onMessageContact(rich.cfMeetId)} className="shrink-0 rounded-full px-2.5 py-1 text-xs font-semibold hover:bg-black/5">Message</button> : null}
      </div>
    );
  }
  const total = rich.tally.voters;
  const toggle = (i: number) => {
    if (!onVote) return;
    const mineNow = rich.tally.myChoices;
    if (rich.multiple) onVote(mineNow.includes(i) ? mineNow.filter((c) => c !== i) : [...mineNow, i].sort((a, b) => a - b));
    else onVote(mineNow.includes(i) ? [] : [i]);
  };
  return (
    <div className={card(mine)} data-testid="poll">
      <p className="flex items-center gap-1.5 text-sm font-semibold"><BarChart3 className="size-4" />{rich.question}</p>
      <p className="mb-1.5 text-[11px] opacity-70">{rich.multiple ? "Select one or more" : "Select one"}</p>
      <ul className="space-y-1">
        {rich.options.map((o, i) => {
          const chosen = rich.tally.myChoices.includes(i);
          const share = total ? Math.round((rich.tally.counts[i] / total) * 100) : 0;
          return (
            <li key={i}>
              <button type="button" disabled={!onVote} onClick={() => toggle(i)} aria-pressed={chosen} className="relative flex w-full items-center gap-2 overflow-hidden rounded-lg px-2 py-1.5 text-left text-sm disabled:cursor-default">
                <span aria-hidden className={cn("absolute inset-y-0 left-0", mine ? "bg-white/20" : "bg-primary/15")} style={{ width: `${share}%` }} />
                <span className={cn("relative flex size-4 shrink-0 items-center justify-center rounded-full border", chosen ? "border-current bg-current/20" : "border-current/50")}>{chosen ? <Check className="size-3" /> : null}</span>
                <span className="relative min-w-0 flex-1 truncate">{o}</span>
                <span className="relative text-xs tabular-nums opacity-75">{rich.tally.counts[i]}</span>
              </button>
            </li>
          );
        })}
      </ul>
      <p className="mt-1 text-[11px] opacity-70">{total === 1 ? "1 vote" : `${total} votes`}</p>
    </div>
  );
}

const field = "h-10 w-full rounded-xl border border-border bg-background px-3 text-sm outline-none focus:border-primary";

/**
 * The chat's one attach button (WhatsApp): a file straight away in chats that only take files;
 * in a private chat a menu of Document, Poll, Location and Contact.
 */
export function AttachMenu({ canAttach, canShare, disabled, onFile, onSend, fileTitle }: {
  canAttach: boolean;
  canShare: boolean;
  disabled: boolean;
  onFile: () => void;
  onSend: (payload: RichPayload) => Promise<void>;
  fileTitle: string;
}) {
  const [open, setOpen] = useState<"poll" | "location" | "contact" | null>(null);
  const [menu, setMenu] = useState(false);
  const button = "flex size-10 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-40";
  if (!canShare) {
    return <button type="button" className={button} disabled={!canAttach} onClick={onFile} aria-label="Attach a file" title={fileTitle}><Paperclip className="size-5" /></button>;
  }
  const items: { key: string; label: string; icon: typeof FileText; tint: string; run: () => void; off?: boolean }[] = [
    { key: "file", label: "Document", icon: FileText, tint: "bg-violet-600", run: onFile, off: !canAttach },
    { key: "poll", label: "Poll", icon: BarChart3, tint: "bg-amber-500", run: () => setOpen("poll") },
    { key: "location", label: "Location", icon: MapPin, tint: "bg-emerald-600", run: () => setOpen("location") },
    { key: "contact", label: "Contact", icon: UserRound, tint: "bg-sky-600", run: () => setOpen("contact") },
  ];
  return (
    <>
      <Popover.Root open={menu} onOpenChange={setMenu}>
        <Popover.Trigger asChild>
          <button type="button" disabled={disabled} aria-label="Attach" title="Attach" data-testid="share-menu" className={button}><Paperclip className="size-5" /></button>
        </Popover.Trigger>
        <Popover.Portal>
          <Popover.Content side="top" align="start" sideOffset={8} collisionPadding={12} className="z-50 w-56 rounded-2xl border border-border bg-popover p-1.5 shadow-xl">
            {items.map(({ key, label, icon: Icon, tint, run, off }) => (
              <button key={key} type="button" disabled={off} data-testid={`share-${key}`} onClick={() => { setMenu(false); run(); }}
                className="flex w-full items-center gap-3 rounded-xl px-2.5 py-2 text-left text-[15px] hover:bg-muted disabled:opacity-40">
                <span className={cn("flex size-9 items-center justify-center rounded-full text-white", tint)}><Icon className="size-[18px]" /></span>{label}
              </button>
            ))}
          </Popover.Content>
        </Popover.Portal>
      </Popover.Root>
      {open ? <ShareDialog kind={open} onClose={() => setOpen(null)} onSend={onSend} /> : null}
    </>
  );
}

function ShareDialog({ kind, onClose, onSend }: { kind: "poll" | "location" | "contact"; onClose: () => void; onSend: (payload: RichPayload) => Promise<void> }) {
  const [question, setQuestion] = useState("");
  const [options, setOptions] = useState(["", ""]);
  const [multiple, setMultiple] = useState(false);
  const [search, setSearch] = useState("");
  const contacts = useHubContacts(kind === "contact");
  const [label, setLabel] = useState("");
  const [position, setPosition] = useState<{ lat: number; lng: number } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const filled = options.map((o) => o.trim()).filter(Boolean);
  const payload: RichPayload | null =
    kind === "poll" ? (question.trim() && filled.length >= 2 ? { t: "poll", question: question.trim(), options: filled, multiple } : null)
      : kind === "contact" ? null
        : position ? { t: "location", lat: position.lat, lng: position.lng, label: label.trim() || null } : null;
  const locate = () => {
    setError(null);
    if (!navigator.geolocation) return setError("This browser can't share a location.");
    navigator.geolocation.getCurrentPosition(
      (p) => setPosition({ lat: p.coords.latitude, lng: p.coords.longitude }),
      () => setError("Location access was not allowed."),
      { enableHighAccuracy: true, timeout: 15_000 },
    );
  };
  const send = (p: RichPayload | null = payload) => {
    if (!p) return;
    setBusy(true);
    setError(null);
    onSend(p).then(onClose, () => { setBusy(false); setError("That didn't send. Try again."); });
  };
  const needle = search.trim().toLocaleLowerCase();
  const people = connectedContacts(contacts.data ?? []).filter((c) => !needle || contactLabel(c).toLocaleLowerCase().includes(needle));
  const removeOption = (i: number) => {
    const next = options.filter((_, j) => j !== i);
    setOptions(next.length < 2 || next[next.length - 1] ? [...next, ""] : next);
  };
  return (
    <Dialog open onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{kind === "poll" ? "Create poll" : kind === "location" ? "Share location" : "Share a contact"}</DialogTitle>
          <DialogDescription>End-to-end encrypted: only people in this chat can see it.</DialogDescription>
        </DialogHeader>
        <div className="space-y-2.5">
          {kind === "poll" ? (
            <>
              <input aria-label="Question" placeholder="Ask a question" maxLength={300} value={question} onChange={(e) => setQuestion(e.target.value)} className={field} />
              {options.map((o, i) => (
                <div key={i} className="flex items-center gap-1.5">
                  <input aria-label={`Option ${i + 1}`} placeholder={i === options.length - 1 && i >= 2 ? "Add option" : `Option ${i + 1}`} maxLength={100} value={o}
                    onChange={(e) => { const next = options.map((x, j) => (j === i ? e.target.value : x)); setOptions(i === next.length - 1 && e.target.value && next.length < 12 ? [...next, ""] : next); }} className={field} />
                  {o && options.length > 2 ? <button type="button" aria-label={`Remove option ${i + 1}`} onClick={() => removeOption(i)} className="flex size-10 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><X className="size-4" /></button> : null}
                </div>
              ))}
              <button type="button" role="switch" aria-checked={multiple} onClick={() => setMultiple(!multiple)} className="flex w-full items-center gap-3 rounded-xl py-1.5 text-left text-sm">
                <span className="flex-1">Allow more than one answer</span>
                <span aria-hidden className={cn("relative h-6 w-10 shrink-0 rounded-full transition-colors", multiple ? "bg-primary" : "bg-muted-foreground/30")}><span className={cn("absolute top-0.5 size-5 rounded-full bg-white shadow transition-all", multiple ? "left-[1.125rem]" : "left-0.5")} /></span>
              </button>
            </>
          ) : kind === "contact" ? (
            <>
              <label className="relative block">
                <Search className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                <input aria-label="Search contacts" placeholder="Search contacts" value={search} onChange={(e) => setSearch(e.target.value)} className={cn(field, "pl-9")} />
              </label>
              <ul className="-mx-1 max-h-72 overflow-y-auto">
                {contacts.isPending ? <li className="px-3 py-6 text-center text-sm text-muted-foreground">Loading contacts…</li>
                  : !people.length ? <li className="px-3 py-6 text-center text-sm text-muted-foreground">{needle ? "No one matches." : "You have no contacts yet."}</li>
                  : people.map((c) => (
                    <li key={c.cfMeetId}>
                      <button type="button" disabled={busy} onClick={() => send({ t: "contact", name: contactLabel(c), cfMeetId: c.cfMeetId })} className="flex w-full items-center gap-3 rounded-xl px-2 py-2 text-left hover:bg-muted disabled:opacity-50">
                        <span className="flex size-10 shrink-0 items-center justify-center rounded-full bg-primary/10 text-sm font-semibold text-primary">{contactLabel(c).slice(0, 1).toUpperCase()}</span>
                        <span className="min-w-0 flex-1"><span className="block truncate text-[15px] font-medium">{contactLabel(c)}</span><span className="block truncate font-mono text-xs text-muted-foreground">{c.cfMeetId}</span></span>
                      </button>
                    </li>
                  ))}
              </ul>
            </>
          ) : (
            <>
              <Button type="button" variant="outline" className="h-10 w-full rounded-xl" onClick={locate}><MapPin className="size-4" />{position ? "Location found" : "Use my current location"}</Button>
              <input aria-label="Place name" placeholder="Place name (optional)" maxLength={200} value={label} onChange={(e) => setLabel(e.target.value)} className={field} />
            </>
          )}
          {error ? <p role="alert" className="rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</p> : null}
          {kind !== "contact" ? <Button className="h-11 w-full rounded-xl" disabled={!payload} loading={busy} onClick={() => send()}>Send</Button> : null}
        </div>
      </DialogContent>
    </Dialog>
  );
}
