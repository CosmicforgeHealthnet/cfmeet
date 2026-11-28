"use client";

/**
 * My status (WhatsApp): every status of mine in one place — the ones still being added
 * (from this browser's publishing queue: progress, failed, retry, cancel) and the live ones
 * from the server, each with its thumbnail, when it was posted, the time left, its audience,
 * Allow sharing, the people who viewed it (once each, with the time and their reaction) and
 * the reactions. Delete one or several; add a new one. ADR 0073: each status has its menu
 * (Status privacy, Viewers, Sharing, Replies, Delete; Retry / Cancel while still being
 * added) and lists its private replies (only its author sees them) with a jump to the chat.
 */
import { useEffect, useState } from "react";
import Link from "next/link";
import { ArrowLeft, CheckSquare, ChevronDown, Eye, Lock, MessageCircle, MoreVertical, Plus, RotateCw, Share2, Square, Trash2, X, XCircle } from "lucide-react";
import { Popover } from "radix-ui";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { useAccountProfile } from "@/hooks/useAccount";
import { useMediaUrl, useMyStatuses, useSocialRefresh, useStatusReplies } from "@/hooks/v2/useSocial";
import { socialService, type PersonView, type StatusPost } from "@/services/v2/social.service";
import { reactionSummary, snapshotLabel, timeLeft, uniqueViewers } from "@/lib/status-privacy";
import TextStatusView from "@/components/media-editor/TextStatusView";
import { PendingThumb, stateText } from "./MyStatusCard";
import { useStatusQueue } from "./StatusQueueRoot";
import StatusComposer from "./StatusComposer";
import StatusViewer from "./StatusViewer";
import { Toggle } from "./audience";
import { errorText, PersonAvatar, whenText } from "./shared";
import { myStatusMenu, type MenuItem } from "@/lib/section-menus";
import { messageHref } from "@/lib/starred";
import { textStatusOfPost } from "./status-adapter";

/** Re-render every minute (time left). */
function useMinute(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 60_000);
    return () => clearInterval(t);
  }, []);
  return now;
}

export default function MyStatus() {
  const mine = useMyStatuses();
  const refresh = useSocialRefresh();
  const profile = useAccountProfile();
  const { snapshot, retry, cancel } = useStatusQueue();
  const now = useMinute();
  const [composer, setComposer] = useState<null | "media" | "text">(null);
  const [viewing, setViewing] = useState<number | null>(null);
  const [selecting, setSelecting] = useState(false);
  const [selected, setSelected] = useState<string[]>([]);
  const [deleting, setDeleting] = useState(false);
  const posts = mine.data?.posts ?? [];
  const receipts = mine.data?.readReceipts ?? true;
  const pending = snapshot.jobs.filter((j) => j.state !== "done");
  const me: PersonView = { userId: "me", name: profile.data?.effectiveDisplayName || "You", avatarUrl: profile.data?.avatarUrl ?? null, principalId: null };

  const removeMany = async (ids: string[]) => {
    if (!ids.length || deleting) return;
    setDeleting(true);
    try {
      const n = ids.length === 1 ? (await socialService.deleteStatus(ids[0]), 1) : await socialService.deleteStatuses(ids);
      toast(n === 1 ? "Status deleted" : `${n} status updates deleted`);
      setSelected([]);
      setSelecting(false);
      await refresh();
    } catch (e) {
      toast(errorText(e, "Couldn't delete. Try again."), "error");
    } finally {
      setDeleting(false);
    }
  };

  return (
    <div className="mx-auto max-w-2xl px-4 pb-28 pt-6 sm:px-6 lg:py-8" data-testid="my-status-page">
      <header className="flex items-center gap-2">
        <Link href="/dashboard/updates" aria-label="Back to Updates" className="flex size-10 items-center justify-center rounded-full hover:bg-muted"><ArrowLeft className="size-5" /></Link>
        <h1 className="min-w-0 flex-1 truncate text-2xl font-bold tracking-tight">My status</h1>
        {posts.length ? (
          <button type="button" onClick={() => { setSelecting(!selecting); setSelected([]); }} data-testid="my-status-select"
            className="flex h-9 items-center gap-1.5 rounded-full px-3 text-sm font-semibold text-primary hover:bg-primary/10">
            {selecting ? <><X className="size-4" />Done</> : <><CheckSquare className="size-4" />Select</>}
          </button>
        ) : null}
        <button type="button" onClick={() => setComposer("media")} data-testid="my-status-add" className="flex h-9 items-center gap-1.5 rounded-full bg-primary px-3.5 text-sm font-semibold text-primary-foreground"><Plus className="size-4" />Add</button>
      </header>

      {selecting ? (
        <div className="sticky top-0 z-10 mt-3 flex items-center gap-2 rounded-2xl bg-card px-3 py-2 shadow ring-1 ring-border">
          <button type="button" onClick={() => setSelected(selected.length === posts.length ? [] : posts.map((p) => p.statusId))} className="rounded-full px-2 py-1 text-sm font-semibold text-primary hover:bg-primary/10">
            {selected.length === posts.length ? "Deselect all" : "Select all"}
          </button>
          <span className="flex-1 text-sm text-muted-foreground">{selected.length} selected</span>
          <button type="button" disabled={!selected.length || deleting} onClick={() => void removeMany(selected)} data-testid="my-status-delete-selected"
            className="flex h-9 items-center gap-1.5 rounded-full bg-destructive px-3.5 text-sm font-semibold text-white disabled:opacity-50"><Trash2 className="size-4" />Delete</button>
        </div>
      ) : null}

      {pending.length ? (
        <section className="mt-6" aria-labelledby="pending-heading">
          <h2 id="pending-heading" className="px-1 text-sm font-semibold text-muted-foreground">Being added</h2>
          <ul className="mt-2 divide-y divide-border rounded-2xl bg-card ring-1 ring-border">
            {[...pending].reverse().map((job) => (
              <li key={job.id} className="flex items-center gap-3 px-3 py-3" data-testid="my-status-pending" data-state={job.state}>
                <PendingThumb preview={job.preview} small />
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-semibold">{job.postCount > 1 ? `${job.postCount} status updates` : job.preview.kind === "TEXT" ? "Text status" : job.preview.kind === "IMAGE" ? "Photo" : job.preview.kind === "VIDEO" ? "Video" : "Voice status"}</span>
                  <span className={cn("block truncate text-xs", job.state === "failed" ? "text-destructive" : "text-muted-foreground")}>{stateText(job, snapshot.online)}</span>
                  <span className="block truncate text-xs text-muted-foreground">{job.audienceLabel ?? "Default audience"}</span>
                </span>
                {job.state === "failed" && !job.fatal ? (
                  <button type="button" onClick={() => retry(job.id)} className="flex h-9 items-center gap-1.5 rounded-full bg-primary px-3 text-sm font-semibold text-primary-foreground" data-testid="status-retry"><RotateCw className="size-4" />Retry</button>
                ) : null}
                <button type="button" onClick={() => cancel(job.id)} className="h-9 rounded-full bg-muted px-3 text-sm font-semibold" data-testid="status-cancel">Cancel</button>
                <RowMenu items={myStatusMenu({ kind: "pending", failed: job.state === "failed", fatal: !!job.fatal })} onPick={(key) => (key === "retry" ? retry(job.id) : cancel(job.id))} />
              </li>
            ))}
          </ul>
        </section>
      ) : null}

      <section className="mt-6" aria-labelledby="live-heading">
        <h2 id="live-heading" className="px-1 text-sm font-semibold text-muted-foreground">{posts.length ? `Live · disappear 24 hours after posting` : "Live"}</h2>
        {mine.isPending ? <div className="mt-2 space-y-2">{[0, 1].map((i) => <div key={i} className="h-20 animate-pulse rounded-2xl bg-muted" />)}</div> : null}
        {mine.isError ? <p role="alert" className="mt-2 px-1 text-sm text-destructive">Your status updates couldn&apos;t load. <button type="button" className="font-semibold underline" onClick={() => void mine.refetch()}>Try again</button></p> : null}
        {!mine.isPending && !mine.isError && !posts.length ? (
          <div className="mt-2 rounded-2xl bg-card px-4 py-8 text-center ring-1 ring-border">
            <p className="text-sm text-muted-foreground">You have no live status updates.</p>
            <button type="button" onClick={() => setComposer("media")} className="mt-3 h-10 rounded-full bg-primary/10 px-5 text-sm font-semibold text-primary">Add status</button>
          </div>
        ) : null}
        <ul className="mt-2 space-y-2">
          {posts.map((p, i) => (
            <MyStatusRow key={p.statusId} post={p} now={now} receipts={receipts} selecting={selecting} selected={selected.includes(p.statusId)}
              onToggle={() => setSelected(selected.includes(p.statusId) ? selected.filter((x) => x !== p.statusId) : [...selected, p.statusId])}
              onOpen={() => setViewing(i)} onDelete={() => void removeMany([p.statusId])} onChanged={() => void mine.refetch()} />
          ))}
        </ul>
        {posts.length ? (
          <p className="mt-3 flex items-center gap-1.5 px-1 text-xs text-muted-foreground"><MessageCircle className="size-3.5" />Replies to your status arrive privately in your <Link href="/dashboard/messages" className="font-semibold text-primary">chats</Link>.</p>
        ) : null}
        {!receipts && posts.length ? <p className="mt-1 px-1 text-xs text-muted-foreground">Read receipts are off, so viewers aren&apos;t shown.</p> : null}
      </section>

      {composer ? <StatusComposer initial={composer} onClose={() => setComposer(null)} onPosted={() => void refresh()} /> : null}
      {viewing !== null && posts.length ? (
        <StatusViewer person={me} posts={posts} mine startAt={viewing} onClose={() => { setViewing(null); void refresh(); }} onChanged={() => void mine.refetch()} />
      ) : null}
    </div>
  );
}

function Thumb({ post }: { post: StatusPost }) {
  const { url } = useMediaUrl(post.kind === "IMAGE" ? post.media : null);
  return (
    <span aria-hidden className="relative block size-14 shrink-0 overflow-hidden rounded-xl bg-muted">
      {post.kind === "TEXT" ? <TextStatusView status={textStatusOfPost(post)} />
        // eslint-disable-next-line @next/next/no-img-element
        : url ? <img src={url} alt="" className="size-full object-cover" />
        : <span className={cn("flex size-full items-center justify-center text-[10px] font-semibold uppercase text-white", post.kind === "AUDIO" ? "bg-emerald-700" : "bg-gradient-to-b from-slate-600 to-slate-900")}>{post.kind === "VIDEO" ? "Video" : post.kind === "AUDIO" ? "Voice" : ""}</span>}
    </span>
  );
}

function MyStatusRow({ post, now, receipts, selecting, selected, onToggle, onOpen, onDelete, onChanged }: {
  post: StatusPost; now: number; receipts: boolean; selecting: boolean; selected: boolean;
  onToggle: () => void; onOpen: () => void; onDelete: () => void; onChanged: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [section, setSection] = useState<"viewers" | "replies" | null>(null);
  const [sharing, setSharing] = useState(post.allowSharing !== false);
  const [saving, setSaving] = useState(false);
  const viewers = uniqueViewers((post.viewers ?? []).map((v) => ({ ...v, principalId: v.person.principalId ?? v.person.userId })));
  const reactions = reactionSummary(viewers);
  const count = post.uniqueViewerCount ?? (receipts ? viewers.length : null);
  // A reshare's sharing follows the original too; the server refuses forwards when either is off.
  const changeSharing = async () => {
    const v = !sharing;
    setSharing(v);
    setSaving(true);
    try {
      await socialService.updateStatus(post.statusId, { allowSharing: v });
      onChanged();
    } catch (e) {
      setSharing(!v);
      toast(errorText(e, "Couldn't change sharing."), "error");
    } finally {
      setSaving(false);
    }
  };

  return (
    <li className={cn("rounded-2xl bg-card ring-1 ring-border", selected && "ring-2 ring-primary")} data-testid="my-status-item">
      <div className="flex items-center gap-3 p-3">
        {selecting ? (
          <button type="button" role="checkbox" aria-checked={selected} aria-label="Select status" onClick={onToggle} className="flex size-8 items-center justify-center text-primary">
            {selected ? <CheckSquare className="size-5" /> : <Square className="size-5 text-muted-foreground" />}
          </button>
        ) : null}
        <button type="button" onClick={selecting ? onToggle : onOpen} aria-label="View status" className="shrink-0"><Thumb post={post} /></button>
        <button type="button" onClick={() => setOpen(!open)} className="min-w-0 flex-1 text-left" aria-expanded={open} data-testid="my-status-details">
          <span className="block truncate text-sm font-semibold">{whenText(post.createdAt)}</span>
          <span className="block truncate text-xs text-muted-foreground">
            <span data-testid="my-status-left">{timeLeft(post.expiresAt, now)}</span> · <span data-testid="my-status-audience">{snapshotLabel(post.audience)}</span>
            {post.origin ? " · Reshared" : ""}
          </span>
          <span className="mt-0.5 flex items-center gap-2 text-xs text-muted-foreground">
            <span className="inline-flex items-center gap-1" data-testid="my-status-views"><Eye className="size-3.5" />{count === null ? "Receipts off" : `${count} ${count === 1 ? "view" : "views"}`}</span>
            {reactions.slice(0, 3).map((r) => <span key={r.emoji}>{r.emoji} {r.count}</span>)}
            {post.shareCount ? <span>{post.shareCount} shared</span> : null}
          </span>
        </button>
        <ChevronDown className={cn("size-5 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")} aria-hidden />
        {!selecting ? <button type="button" aria-label="Delete status" onClick={onDelete} className="flex size-9 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-destructive/10 hover:text-destructive" data-testid="my-status-delete"><Trash2 className="size-4" /></button> : null}
        {!selecting ? (
          <RowMenu items={myStatusMenu({ kind: "live", readReceipts: receipts, allowSharing: sharing })} onPick={(key) => {
            if (key === "delete") onDelete();
            else if (key === "sharing") void changeSharing();
            else if (key === "viewers" || key === "replies") { setOpen(true); setSection(key); }
          }} />
        ) : null}
      </div>
      {open ? (
        <div className="border-t border-border px-3 pb-3 pt-2">
          <button type="button" role="switch" aria-checked={sharing} disabled={saving} onClick={() => void changeSharing()} data-testid="my-status-sharing"
            className="flex w-full items-center gap-3 py-2 text-left disabled:opacity-60">
            <span className="min-w-0 flex-1">
              <span className="block text-sm font-semibold">Allow sharing</span>
              <span className="block text-xs text-muted-foreground">Viewers can forward or reshare this status within its audience.</span>
            </span>
            <Toggle on={sharing} />
          </button>
          <StatusReplies statusId={post.statusId} count={post.replyCount ?? 0} focus={section === "replies"} />
          <p className="mt-2 text-sm font-semibold" id={`viewers-${post.statusId}`}>Viewed by {count ?? 0}</p>
          {!viewers.length ? <p className="py-3 text-sm text-muted-foreground">{receipts ? "No views yet" : "Read receipts are off"}</p> : null}
          <ul className="divide-y divide-border" data-testid="my-status-viewers">
            {viewers.map((v) => (
              <li key={v.principalId} className="flex items-center gap-3 py-2">
                <PersonAvatar person={v.person} size="size-9" />
                <span className="min-w-0 flex-1"><span className="block truncate text-sm font-medium">{v.person.name}</span><span className="block text-xs text-muted-foreground">{whenText(v.viewedAt)}</span></span>
                {v.reaction ? <span className="text-lg" aria-label={`Reacted ${v.reaction}`}>{v.reaction}</span> : v.liked ? <span className="text-lg" aria-label="Liked">{"❤️"}</span> : null}
              </li>
            ))}
          </ul>
        </div>
      ) : null}
    </li>
  );
}

const MENU_ICONS: Record<string, typeof Lock> = { privacy: Lock, viewers: Eye, sharing: Share2, replies: MessageCircle, delete: Trash2, retry: RotateCw, cancel: XCircle };

/** One status's ⋮ menu (lib/section-menus: only what the author may do with it now). */
function RowMenu({ items, onPick }: { items: MenuItem[]; onPick: (key: string) => void }) {
  const [open, setOpen] = useState(false);
  const row = "flex w-full items-center gap-3 rounded-lg px-2.5 py-2 text-left text-sm hover:bg-muted";
  return (
    <Popover.Root open={open} onOpenChange={setOpen}>
      <Popover.Trigger asChild>
        <button type="button" aria-label="Status options" data-testid="my-status-menu" className="flex size-9 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><MoreVertical className="size-4" /></button>
      </Popover.Trigger>
      <Popover.Portal>
        <Popover.Content side="bottom" align="end" sideOffset={6} collisionPadding={12} className="z-50 w-52 rounded-xl border border-border bg-popover p-1.5 shadow-xl" data-testid="my-status-menu-items">
          {items.map(({ key, label, href, danger }) => {
            const Icon = MENU_ICONS[key] ?? MoreVertical;
            const body = <><Icon className={cn("size-4", danger ? "text-destructive" : "text-muted-foreground")} />{label}</>;
            return href ? (
              <Link key={key} href={href} className={row} data-testid={`my-status-menu-${key}`} onClick={() => setOpen(false)}>{body}</Link>
            ) : (
              <button key={key} type="button" className={cn(row, danger && "text-destructive")} data-testid={`my-status-menu-${key}`} onClick={() => { setOpen(false); onPick(key); }}>{body}</button>
            );
          })}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}

/**
 * The private replies to one of my statuses (ADR 0073): only I see them; each opens the chat
 * at the reply. Loaded when the details open (the list ends with the status).
 */
function StatusReplies({ statusId, count, focus }: { statusId: string; count: number; focus: boolean }) {
  const replies = useStatusReplies(statusId);
  const list = replies.data ?? [];
  return (
    <section className={cn("mt-2 rounded-xl", focus && "bg-primary/5 p-2")} aria-label="Replies" data-testid="my-status-replies">
      <p className="text-sm font-semibold">Replies {replies.data ? list.length : count ? count : ""}</p>
      {replies.isPending ? <div className="my-2 h-10 animate-pulse rounded-xl bg-muted" /> : null}
      {replies.isError ? <p className="py-2 text-sm text-destructive">Replies couldn&apos;t load. <button type="button" className="font-semibold underline" onClick={() => void replies.refetch()}>Try again</button></p> : null}
      {replies.data && !list.length ? <p className="py-2 text-sm text-muted-foreground">No replies yet. Replies arrive privately in your chats.</p> : null}
      <ul className="divide-y divide-border">
        {list.map((r) => (
          <li key={r.replyId} className="flex items-center gap-3 py-2" data-testid="my-status-reply">
            <PersonAvatar person={r.person} size="size-9" />
            <span className="min-w-0 flex-1">
              <span className="flex items-baseline justify-between gap-2"><span className="truncate text-sm font-medium">{r.person.name}</span><span className="shrink-0 text-xs text-muted-foreground">{whenText(r.createdAt)}</span></span>
              <span className="block truncate text-sm text-muted-foreground">{r.text ?? "Open the chat to read it"}</span>
            </span>
            <Link href={messageHref(r.conversationId, r.messageId)} className="h-8 shrink-0 rounded-full bg-primary/10 px-3 text-xs font-semibold leading-8 text-primary hover:bg-primary/15" data-testid="my-status-reply-open">Open chat</Link>
          </li>
        ))}
      </ul>
    </section>
  );
}
