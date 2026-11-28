"use client";

import { useEffect, useState } from "react";
import { Clock, Copy, FileText, Flag, Forward, MoreVertical, Pencil, Pin, PinOff, SmilePlus, Star, StarOff, Trash2 } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { useMediaUrl } from "@/hooks/v2/useSocial";
import { pollPercents, postLink } from "@/lib/channels";
import { channelsService, mediaView, type Channel, type Post } from "@/services/v2/channels.service";
import { socialService, type PersonView } from "@/services/v2/social.service";
import { errorText, shortWhen, whenText } from "@/components/updates/shared";
import ReportDialog from "./ReportDialog";
import { Sheet } from "./bits";

const QUICK = ["👍", "❤️", "😂", "😮", "😢", "🙏"];

function size(bytes: number): string {
  if (bytes < 1024 * 1024) return `${Math.max(1, Math.round(bytes / 1024))} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function Media({ post }: { post: Post }) {
  const m = post.media;
  const { url, failed } = useMediaUrl(m ? mediaView(m) : null);
  if (!m) return null;
  if (m.kind === "DOCUMENT") {
    return (
      <a href={url ?? undefined} download={m.fileName ?? "document"} aria-disabled={!url}
        className={cn("m-2 flex items-center gap-3 rounded-xl bg-muted px-3 py-3", !url && "pointer-events-none opacity-70")} data-testid="channel-document">
        <FileText className="size-8 shrink-0 text-primary" />
        <span className="min-w-0 flex-1">
          <span className="block truncate text-sm font-medium">{m.fileName ?? "Document"}</span>
          <span className="block text-xs text-muted-foreground">{failed ? "Couldn't load" : size(m.sizeBytes)}</span>
        </span>
      </a>
    );
  }
  if (!url) return <div className="flex h-40 w-72 max-w-full items-center justify-center bg-muted text-sm text-muted-foreground">{failed ? "Couldn't load" : "Loading…"}</div>;
  if (post.kind === "STICKER") {
    // eslint-disable-next-line @next/next/no-img-element
    return <img src={url} alt="Sticker" className="m-2 size-40 object-contain" />;
  }
  if (m.kind === "IMAGE") {
    // eslint-disable-next-line @next/next/no-img-element
    return <img src={url} alt="" className="max-h-96 w-full object-cover" />;
  }
  if (m.kind === "VIDEO") {
    return post.kind === "GIF"
      ? <video src={url} autoPlay loop muted playsInline className="max-h-96 w-full bg-black" />
      : <video src={url} controls playsInline className="max-h-96 w-full bg-black" />;
  }
  return (
    <div className="m-3">
      {post.kind === "VOICE" ? <p className="mb-1 text-xs font-medium text-muted-foreground">Voice message</p> : null}
      <audio src={url} controls className="w-64 max-w-full" />
    </div>
  );
}

function PollView({ post, canVote, onChanged }: { post: Post; canVote: boolean; onChanged: (p: Post) => void }) {
  const poll = post.poll!;
  const [picked, setPicked] = useState<number[]>(poll.myVotes);
  const [busy, setBusy] = useState(false);
  const percents = pollPercents(poll.options.map((o) => o.votes));
  const voted = poll.myVotes.length > 0;
  const submit = async (choice: number[] | null) => {
    setBusy(true);
    try {
      const p = await channelsService.vote(post.channelId, post.postId, choice);
      setPicked(p.poll?.myVotes ?? []);
      onChanged(p);
    } catch (e) {
      toast(errorText(e, "Your vote didn't go through."), "error");
    } finally {
      setBusy(false);
    }
  };
  const toggle = (i: number) => {
    if (!canVote || poll.closed || busy) return;
    if (!poll.multiple) return void submit([i]);
    setPicked((cur) => (cur.includes(i) ? cur.filter((x) => x !== i) : [...cur, i].sort((a, b) => a - b)));
  };
  return (
    <div className="space-y-2 px-3.5 py-3" data-testid="channel-poll">
      <p className="font-semibold">{poll.question}</p>
      <p className="text-xs text-muted-foreground">{poll.multiple ? "Select one or more" : "Select one"}{poll.closed ? " · Closed" : ""}</p>
      {poll.options.map((o, i) => (
        <button key={o.index} type="button" onClick={() => toggle(o.index)} disabled={!canVote || poll.closed || busy}
          className={cn("relative block w-full overflow-hidden rounded-xl border px-3 py-2 text-left text-sm", picked.includes(o.index) ? "border-primary" : "border-border")}>
          {voted || !canVote ? <span aria-hidden className="absolute inset-y-0 left-0 bg-primary/15" style={{ width: `${percents[i]}%` }} /> : null}
          <span className="relative flex items-center justify-between gap-2">
            <span>{o.text}</span>
            {voted || !canVote ? <span className="text-xs text-muted-foreground">{percents[i]}%</span> : null}
          </span>
        </button>
      ))}
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <span className="flex-1">{poll.totalVoters} {poll.totalVoters === 1 ? "vote" : "votes"}</span>
        {poll.multiple && canVote && !poll.closed ? (
          <button type="button" disabled={busy || picked.length === 0} onClick={() => void submit(picked)} className="rounded-full px-3 py-1 font-semibold text-primary hover:bg-primary/10 disabled:opacity-50">Vote</button>
        ) : null}
        {voted && canVote && !poll.closed ? <button type="button" disabled={busy} onClick={() => void submit(null)} className="rounded-full px-3 py-1 font-semibold hover:bg-muted">Retract</button> : null}
      </div>
      {!canVote ? <p className="text-xs text-muted-foreground">Follow the channel to vote.</p> : null}
    </div>
  );
}

function ForwardSheet({ post, onClose }: { post: Post; onClose: () => void }) {
  const [people, setPeople] = useState<PersonView[] | null>(null);
  const [picked, setPicked] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  useEffect(() => { socialService.eligibleContacts().then(setPeople, () => setPeople([])); }, []);
  const send = async () => {
    setBusy(true);
    try {
      const r = await channelsService.forward(post.channelId, post.postId, picked);
      toast(r.forwarded > 0 ? `Shared with ${r.forwarded} ${r.forwarded === 1 ? "chat" : "chats"}` : "Couldn't share with those contacts", r.forwarded > 0 ? "success" : "error");
      onClose();
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      setBusy(false);
    }
  };
  return (
    <Sheet title="Share in chats" onClose={onClose}>
      {people === null ? <p className="py-6 text-center text-sm text-muted-foreground">Loading contacts…</p> : null}
      {people?.length === 0 ? <p className="py-6 text-center text-sm text-muted-foreground">No contacts to share with yet.</p> : null}
      <div className="max-h-80 space-y-1 overflow-y-auto">
        {people?.map((p) => (
          <label key={p.userId} className="flex items-center gap-3 rounded-xl px-2 py-2 hover:bg-muted">
            <input type="checkbox" className="accent-primary" checked={picked.includes(p.userId)} disabled={!picked.includes(p.userId) && picked.length >= 20}
              onChange={(e) => setPicked((cur) => (e.target.checked ? [...cur, p.userId] : cur.filter((x) => x !== p.userId)))} />
            <span className="text-sm">{p.name}</span>
          </label>
        ))}
      </div>
      <button type="button" disabled={busy || picked.length === 0} onClick={() => void send()} className="mt-3 h-11 w-full rounded-xl bg-primary font-semibold text-primary-foreground disabled:opacity-50">
        {busy ? "Sharing…" : `Share${picked.length ? ` (${picked.length})` : ""}`}
      </button>
    </Sheet>
  );
}

function EditSheet({ post, onClose, onSaved }: { post: Post; onClose: () => void; onSaved: (p: Post) => void }) {
  const [text, setText] = useState(post.text ?? "");
  const [busy, setBusy] = useState(false);
  const save = async () => {
    setBusy(true);
    try {
      onSaved(await channelsService.edit(post.channelId, post.postId, { text }));
      onClose();
    } catch (e) {
      toast(errorText(e, "Your change wasn't saved."), "error");
    } finally {
      setBusy(false);
    }
  };
  return (
    <Sheet title={post.media ? "Edit caption" : "Edit post"} onClose={onClose}>
      <textarea value={text} onChange={(e) => setText(e.target.value)} maxLength={4000} rows={5} className="w-full rounded-xl border border-border bg-background px-3 py-2 text-[15px] focus:outline-none focus:ring-2 focus:ring-primary/40" />
      <button type="button" disabled={busy || (!post.media && !text.trim())} onClick={() => void save()} className="mt-3 h-11 w-full rounded-xl bg-primary font-semibold text-primary-foreground disabled:opacity-50">{busy ? "Saving…" : "Save"}</button>
    </Sheet>
  );
}

/** One channel post: every kind, poll voting, reactions and the actions the caller may take. */
export default function ChannelPostCard({ post: initial, channel, onChanged, onRemoved }: { post: Post; channel: Channel; onChanged?: () => void; onRemoved?: () => void }) {
  const [post, setPost] = useState(initial);
  const [shown, setShown] = useState(initial);
  if (shown !== initial) {
    setShown(initial);
    setPost(initial);
  }
  const [picking, setPicking] = useState(false);
  const [menu, setMenu] = useState(false);
  const [sheet, setSheet] = useState<"forward" | "edit" | "report" | null>(null);
  const perms = channel.permissions;
  const canChange = perms.edit || (perms.post && !!post.mine);
  const scheduled = post.state === "SCHEDULED";
  const update = (p: Post) => { setPost(p); onChanged?.(); };
  const act = async (fn: () => Promise<Post | unknown>, ok?: string) => {
    setMenu(false);
    try {
      const r = await fn();
      if (r && typeof r === "object" && "postId" in r) update(r as Post);
      if (ok) toast(ok);
    } catch (e) {
      toast(errorText(e), "error");
    }
  };
  const remove = async () => {
    setMenu(false);
    if (!window.confirm(scheduled ? "Cancel this scheduled post?" : "Delete this post for everyone?")) return;
    try {
      if (scheduled) await channelsService.cancelScheduled(post.channelId, post.postId);
      else await channelsService.deletePost(post.channelId, post.postId);
      onRemoved?.();
    } catch (e) {
      toast(errorText(e), "error");
    }
  };
  const copyLink = async () => {
    setMenu(false);
    try {
      await navigator.clipboard.writeText(postLink(window.location.origin, post.channelId, post.postId));
      toast("Link copied");
    } catch {
      toast("Couldn't copy the link", "error");
    }
  };
  return (
    <div className="group max-w-[85%]" data-testid="channel-post" id={`post-${post.postId}`}>
      <div className="overflow-hidden rounded-2xl rounded-tl-md bg-card shadow-sm ring-1 ring-border">
        {post.pinned ? <p className="flex items-center gap-1 px-3.5 pt-2 text-[11px] font-medium text-muted-foreground"><Pin className="size-3" />Pinned</p> : null}
        <Media post={post} />
        {post.poll ? <PollView post={post} canVote={channel.following || channel.myRole === "OWNER" || channel.myRole === "ADMIN"} onChanged={update} /> : null}
        {post.text ? <p className="whitespace-pre-wrap break-words px-3.5 pt-2 text-[15px]">{post.text}</p> : null}
        {post.linkPreview ? (
          <a href={post.linkPreview.url} target="_blank" rel="noopener noreferrer nofollow" className="mx-3 mt-2 block rounded-xl border-l-4 border-primary bg-muted px-3 py-2" data-testid="channel-link-preview">
            {post.linkPreview.siteName ? <span className="block text-xs text-muted-foreground">{post.linkPreview.siteName}</span> : null}
            <span className="block text-sm font-semibold">{post.linkPreview.title ?? post.linkPreview.url}</span>
            {post.linkPreview.description ? <span className="line-clamp-2 block text-xs text-muted-foreground">{post.linkPreview.description}</span> : null}
          </a>
        ) : null}
        <p className="flex items-center justify-end gap-1.5 px-3.5 py-1.5 text-[11px] text-muted-foreground">
          {scheduled ? <><Clock className="size-3" />Scheduled for {post.publishAt ? whenText(post.publishAt) : ""}</> : (
            <>
              {post.viewCount !== null ? <span>{post.viewCount} {post.viewCount === 1 ? "view" : "views"} ·</span> : null}
              {post.edited ? <span>Edited ·</span> : null}
              <span>{shortWhen(post.createdAt)}</span>
            </>
          )}
        </p>
      </div>
      {!scheduled ? (
        <div className="mt-1 flex flex-wrap items-center gap-1">
          {post.reactions.map((r) => (
            <button key={r.emoji} type="button" onClick={() => void act(() => channelsService.react(post.channelId, post.postId, post.myReaction === r.emoji ? null : r.emoji))}
              className={cn("flex h-7 items-center gap-1 rounded-full px-2 text-sm ring-1", post.myReaction === r.emoji ? "bg-primary/10 ring-primary/40" : "bg-background ring-border")}>
              {r.emoji}<span className="text-xs text-muted-foreground">{r.count}</span>
            </button>
          ))}
          <div className="relative">
            <button type="button" aria-label="React" onClick={() => setPicking(!picking)} className="flex size-7 items-center justify-center rounded-full text-muted-foreground hover:bg-muted" data-testid="react-post"><SmilePlus className="size-4" /></button>
            {picking ? (
              <div className="absolute bottom-8 left-0 z-10 flex gap-1 rounded-full border border-border bg-popover p-1 shadow-xl">
                {QUICK.map((e) => <button key={e} type="button" onClick={() => { setPicking(false); void act(() => channelsService.react(post.channelId, post.postId, e)); }} className="flex size-9 items-center justify-center rounded-full text-xl hover:bg-muted">{e}</button>)}
              </div>
            ) : null}
          </div>
          <button type="button" aria-label={post.starred ? "Unstar" : "Star"} onClick={() => void act(() => channelsService.star(post.channelId, post.postId, !post.starred), post.starred ? "Removed from starred" : "Starred")}
            className="flex size-7 items-center justify-center rounded-full text-muted-foreground hover:bg-muted" data-testid="star-post">
            {post.starred ? <Star className="size-4 fill-amber-400 text-amber-400" /> : <Star className="size-4" />}
          </button>
          <div className="relative">
            <button type="button" aria-label="Post options" onClick={() => setMenu(!menu)} className="flex size-7 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><MoreVertical className="size-4" /></button>
            {menu ? (
              <div className="absolute bottom-8 left-0 z-20 w-52 rounded-2xl border border-border bg-popover p-1 shadow-xl">
                <MenuItem icon={Forward} label="Share in chats" onClick={() => { setMenu(false); setSheet("forward"); }} />
                <MenuItem icon={Copy} label="Copy link" onClick={() => void copyLink()} />
                {perms.post || perms.edit ? <MenuItem icon={post.pinned ? PinOff : Pin} label={post.pinned ? "Unpin" : "Pin"} onClick={() => void act(() => channelsService.pin(post.channelId, post.postId, !post.pinned))} /> : null}
                {canChange && post.kind !== "POLL" ? <MenuItem icon={Pencil} label="Edit" onClick={() => { setMenu(false); setSheet("edit"); }} /> : null}
                {post.starred ? <MenuItem icon={StarOff} label="Unstar" onClick={() => void act(() => channelsService.star(post.channelId, post.postId, false))} /> : null}
                {channel.myRole !== "OWNER" ? <MenuItem icon={Flag} label="Report post" onClick={() => { setMenu(false); setSheet("report"); }} /> : null}
                {canChange ? <MenuItem icon={Trash2} label="Delete" danger onClick={() => void remove()} /> : null}
              </div>
            ) : null}
          </div>
        </div>
      ) : canChange ? (
        <button type="button" onClick={() => void remove()} className="mt-1 flex items-center gap-1 rounded-full px-2 py-1 text-xs text-destructive hover:bg-muted"><Trash2 className="size-3.5" />Cancel</button>
      ) : null}
      {sheet === "forward" ? <ForwardSheet post={post} onClose={() => setSheet(null)} /> : null}
      {sheet === "edit" ? <EditSheet post={post} onClose={() => setSheet(null)} onSaved={update} /> : null}
      {sheet === "report" ? <ReportDialog channelId={post.channelId} postId={post.postId} onClose={() => setSheet(null)} /> : null}
    </div>
  );
}

function MenuItem({ icon: Icon, label, onClick, danger }: { icon: React.ComponentType<{ className?: string }>; label: string; onClick: () => void; danger?: boolean }) {
  return (
    <button type="button" onClick={onClick} className={cn("flex w-full items-center gap-2 rounded-xl px-3 py-2.5 text-sm hover:bg-muted", danger && "text-destructive")}>
      <Icon className="size-4" />{label}
    </button>
  );
}
