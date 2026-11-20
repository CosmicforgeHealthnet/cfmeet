"use client";

/**
 * F4-F7 in private chats (WhatsApp): polls, locations, contact cards and link previews,
 * all inside the end-to-end encrypted content. The bubble part shows one; the composer part
 * (a "+" next to the paperclip, private chats only) creates one.
 */
import { useState } from "react";
import { BarChart3, Check, ExternalLink, Link2, MapPin, Plus, UserRound } from "lucide-react";
import { Popover } from "radix-ui";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { mapUrl, type RichContent, type RichPayload } from "@/lib/messaging/rich-content";
import { cn } from "@/lib/utils";

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
    return (
      <a href={mapUrl(rich.lat, rich.lng)} target="_blank" rel="noopener noreferrer nofollow" className={cn(card(mine), "flex items-center gap-2.5 hover:opacity-90")} data-testid="location">
        <MapPin className="size-5 shrink-0" />
        <span className="min-w-0"><span className="block truncate text-sm font-medium">{rich.label ?? "Location"}</span><span className="block text-xs opacity-70">{rich.lat.toFixed(5)}, {rich.lng.toFixed(5)} · Open map <ExternalLink className="inline size-3" /></span></span>
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

/** "+" in a private chat's composer: share a poll, your location or a contact. */
export function ShareMenu({ disabled, onSend }: { disabled: boolean; onSend: (payload: RichPayload) => Promise<void> }) {
  const [open, setOpen] = useState<"poll" | "location" | "contact" | null>(null);
  const [menu, setMenu] = useState(false);
  const item = "flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left text-sm hover:bg-muted";
  return (
    <>
      <Popover.Root open={menu} onOpenChange={setMenu}>
        <Popover.Trigger asChild>
          <button type="button" disabled={disabled} aria-label="Share" title="Poll, location or contact" data-testid="share-menu" className="flex size-10 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-40"><Plus className="size-5" /></button>
        </Popover.Trigger>
        <Popover.Portal>
          <Popover.Content side="top" align="start" sideOffset={6} className="z-50 w-48 rounded-xl border border-border bg-popover p-1.5 shadow-xl">
            <button type="button" className={item} data-testid="share-poll" onClick={() => { setMenu(false); setOpen("poll"); }}><BarChart3 className="size-4 text-muted-foreground" />Poll</button>
            <button type="button" className={item} data-testid="share-location" onClick={() => { setMenu(false); setOpen("location"); }}><MapPin className="size-4 text-muted-foreground" />Location</button>
            <button type="button" className={item} data-testid="share-contact" onClick={() => { setMenu(false); setOpen("contact"); }}><UserRound className="size-4 text-muted-foreground" />Contact</button>
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
  const [name, setName] = useState("");
  const [cfMeetId, setCfMeetId] = useState("");
  const [label, setLabel] = useState("");
  const [position, setPosition] = useState<{ lat: number; lng: number } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const filled = options.map((o) => o.trim()).filter(Boolean);
  const payload: RichPayload | null =
    kind === "poll" ? (question.trim() && filled.length >= 2 ? { t: "poll", question: question.trim(), options: filled, multiple } : null)
      : kind === "contact" ? (name.trim() && /^[A-Za-z0-9._-]{3,40}$/.test(cfMeetId.trim()) ? { t: "contact", name: name.trim(), cfMeetId: cfMeetId.trim() } : null)
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
  const send = () => {
    if (!payload) return;
    setBusy(true);
    setError(null);
    onSend(payload).then(onClose, () => { setBusy(false); setError("That didn't send. Try again."); });
  };
  return (
    <Dialog open onOpenChange={(v) => !v && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{kind === "poll" ? "Create poll" : kind === "location" ? "Share location" : "Share contact"}</DialogTitle>
          <DialogDescription>End-to-end encrypted: only people in this chat can see it.</DialogDescription>
        </DialogHeader>
        <div className="space-y-2.5">
          {kind === "poll" ? (
            <>
              <input aria-label="Question" placeholder="Ask a question" maxLength={300} value={question} onChange={(e) => setQuestion(e.target.value)} className={field} />
              {options.map((o, i) => (
                <input key={i} aria-label={`Option ${i + 1}`} placeholder={`Option ${i + 1}`} maxLength={100} value={o}
                  onChange={(e) => { const next = options.map((x, j) => (j === i ? e.target.value : x)); setOptions(i === next.length - 1 && e.target.value && next.length < 12 ? [...next, ""] : next); }} className={field} />
              ))}
              <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={multiple} onChange={(e) => setMultiple(e.target.checked)} />Allow more than one answer</label>
            </>
          ) : kind === "contact" ? (
            <>
              <input aria-label="Name" placeholder="Name" maxLength={120} value={name} onChange={(e) => setName(e.target.value)} className={field} />
              <input aria-label="CF Meet ID" placeholder="CF Meet ID" maxLength={40} value={cfMeetId} onChange={(e) => setCfMeetId(e.target.value)} className={field} />
            </>
          ) : (
            <>
              <Button type="button" variant="outline" className="h-10 w-full rounded-xl" onClick={locate}><MapPin className="size-4" />{position ? `${position.lat.toFixed(5)}, ${position.lng.toFixed(5)}` : "Use my current location"}</Button>
              <input aria-label="Place name" placeholder="Place name (optional)" maxLength={200} value={label} onChange={(e) => setLabel(e.target.value)} className={field} />
            </>
          )}
          {error ? <p role="alert" className="rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">{error}</p> : null}
          <Button className="h-11 w-full rounded-xl" disabled={!payload} loading={busy} onClick={send}>Send</Button>
        </div>
      </DialogContent>
    </Dialog>
  );
}
