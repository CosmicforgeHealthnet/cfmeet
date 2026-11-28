"use client";

import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { BellOff, Camera, ChevronDown, ChevronRight, Compass, Lock, MoreVertical, Pencil, Plus, Search, Settings, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { useChannelDirectory, useFollow, useFollowedChannels, useMediaUrl, useSocialRefresh, useStatusFeed } from "@/hooks/v2/useSocial";
import { useAccountProfile } from "@/hooks/useAccount";
import { socialService, type Channel, type PersonView, type StatusAuthor, type StatusPost } from "@/services/v2/social.service";
import { searchStatusAuthors } from "@/lib/status-privacy";
import { ringOfPosts, StatusRingFrame } from "./StatusRing";
import MyStatusCard from "./MyStatusCard";
import StatusComposer from "./StatusComposer";
import StatusViewer from "./StatusViewer";
import TextStatusView from "@/components/media-editor/TextStatusView";
import { errorText, PersonAvatar, shortWhen } from "./shared";
import { textStatusOfPost } from "./status-adapter";

/**
 * Updates (WhatsApp): Status on top (my status, contacts' statuses with rings, muted updates),
 * Channels below. Status search, a menu (Create channel, Status privacy, Settings, Find
 * channels) and two floating buttons: text status and camera / media.
 */
export default function UpdatesContent() {
  const feed = useStatusFeed();
  const channels = useFollowedChannels();
  const refresh = useSocialRefresh();
  const profile = useAccountProfile();
  const [composer, setComposer] = useState<null | "text" | "media" | "voice">(null);
  const [viewing, setViewing] = useState<{ person: PersonView; posts: StatusPost[]; mine: boolean; muted?: boolean; startAt: number } | null>(null);
  const [searching, setSearching] = useState(false);
  const [query, setQuery] = useState("");
  const [menu, setMenu] = useState(false);
  const [showMuted, setShowMuted] = useState(false);
  const [finding, setFinding] = useState(false);
  const [creating, setCreating] = useState(false);

  const me: PersonView = {
    userId: "me",
    name: profile.data?.effectiveDisplayName || "You",
    avatarUrl: profile.data?.avatarUrl ?? null,
    principalId: null,
  };
  const mine = feed.data?.mine ?? [];
  const contacts = searchStatusAuthors(feed.data?.contacts ?? [], query);
  const muted = searchStatusAuthors(feed.data?.muted ?? [], query);
  const followed = channels.data ?? [];
  const startOf = (a: StatusAuthor) => Math.max(0, a.posts.findIndex((p) => !p.viewed));

  return (
    <div className="mx-auto max-w-3xl px-4 pb-28 pt-6 sm:px-6 lg:py-8" data-testid="updates-page">
      <header className="flex items-center justify-between gap-2">
        <h1 className="text-2xl font-bold tracking-tight">Updates</h1>
        <div className="flex items-center gap-1">
          <button type="button" aria-label="Search status updates" aria-pressed={searching} onClick={() => { setSearching(!searching); setQuery(""); }} data-testid="status-search-toggle"
            className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><Search className="size-5" /></button>
          <UpdatesMenu open={menu} onOpenChange={setMenu} onCreateChannel={() => setCreating(true)} onFindChannels={() => setFinding(true)} />
        </div>
      </header>
      {searching ? (
        <label className="mt-3 flex h-11 items-center gap-2 rounded-full bg-muted px-4">
          <Search className="size-4 text-muted-foreground" />
          <input autoFocus value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search status updates" aria-label="Search status updates" data-testid="status-search"
            className="min-w-0 flex-1 bg-transparent text-sm focus:outline-none" />
          {query ? <button type="button" aria-label="Clear search" onClick={() => setQuery("")} className="text-muted-foreground"><X className="size-4" /></button> : null}
        </label>
      ) : null}

      <section className="mt-5" aria-labelledby="status-heading">
        <div className="flex items-center justify-between">
          <h2 id="status-heading" className="text-lg font-bold">Status</h2>
          <Link href="/dashboard/updates/mine" className="flex h-9 items-center gap-0.5 rounded-full px-3 text-sm font-semibold text-primary hover:bg-primary/10" data-testid="my-status-link">My status<ChevronRight className="size-4" /></Link>
        </div>
        <div className="no-scrollbar -mx-4 mt-3 flex gap-3 overflow-x-auto px-4 pb-1" data-testid="status-row">
          {/* Pending statuses (background queue) show here first: preview, "Adding…" + ring, or "Tap to retry". */}
          <MyStatusCard me={me} postCount={mine.length} latest={<StatusThumb post={mine[mine.length - 1]} />}
            onView={() => setViewing({ person: me, posts: mine, mine: true, startAt: 0 })} onAdd={() => setComposer("media")} />
          {feed.isPending ? [0, 1, 2].map((i) => <span key={i} className="h-44 w-28 shrink-0 animate-pulse rounded-2xl bg-muted" />) : null}
          {contacts.map((c) => (
            <button key={c.person.userId} type="button" data-testid="contact-status" data-seen={c.allViewed || undefined}
              onClick={() => setViewing({ person: c.person, posts: c.posts, mine: false, startAt: startOf(c) })}
              className="relative flex h-44 w-28 shrink-0 flex-col justify-between overflow-hidden rounded-2xl bg-muted p-2.5 text-left">
              <StatusThumb post={c.posts[c.posts.length - 1]} />
              <StatusRingFrame ring={ringOfPosts(c.person.userId, c.posts, c.closeFriends)} className="relative self-start"><PersonAvatar person={c.person} size="size-9" /></StatusRingFrame>
              <span className="relative truncate text-sm font-semibold text-white drop-shadow">{c.person.name}</span>
            </button>
          ))}
        </div>
        {!feed.isPending && contacts.length === 0 ? (
          <p className="mt-2 text-sm text-muted-foreground">{query ? `No status updates match “${query}”.` : "Status updates from your contacts show here for 24 hours."}</p>
        ) : null}
        {muted.length ? (
          <div className="mt-4" data-testid="muted-updates">
            <button type="button" onClick={() => setShowMuted(!showMuted)} aria-expanded={showMuted} className="flex w-full items-center gap-2 rounded-xl px-1 py-2 text-left text-sm font-semibold text-muted-foreground hover:text-foreground">
              <BellOff className="size-4" /><span className="flex-1">Muted updates ({muted.length})</span><ChevronDown className={cn("size-4 transition-transform", showMuted && "rotate-180")} />
            </button>
            {showMuted || query ? (
              <ul>
                {muted.map((c) => <MutedRow key={c.person.userId} author={c} onOpen={() => setViewing({ person: c.person, posts: c.posts, mine: false, muted: true, startAt: startOf(c) })} onChanged={() => void refresh()} />)}
              </ul>
            ) : null}
          </div>
        ) : null}
        {feed.isError ? <p role="alert" className="mt-2 text-sm text-destructive">Status updates couldn&apos;t load. <button type="button" className="font-semibold underline" onClick={() => void feed.refetch()}>Try again</button></p> : null}
      </section>

      <section className="mt-8" aria-labelledby="channels-heading">
        <div className="flex items-center justify-between">
          <h2 id="channels-heading" className="text-lg font-bold">Channels</h2>
          <button type="button" onClick={() => setCreating(true)} className="flex h-9 items-center gap-1.5 rounded-full px-3 text-sm font-semibold text-primary hover:bg-primary/10" data-testid="create-channel"><Plus className="size-4" />Create channel</button>
        </div>
        {channels.isPending ? <div className="mt-3 space-y-2">{[0, 1].map((i) => <div key={i} className="h-16 animate-pulse rounded-2xl bg-muted" />)}</div> : null}
        {followed.length ? (
          <ul className="mt-2" data-testid="followed-channels">
            {followed.map((c) => <ChannelRow key={c.channelId} channel={c} />)}
          </ul>
        ) : !channels.isPending ? (
          <p className="mt-2 text-sm text-muted-foreground">Stay updated on topics that matter to you. Find channels to follow below.</p>
        ) : null}
        <Suggestions onFind={() => setFinding(true)} hide={followed.map((c) => c.channelId)} />
      </section>

      <div className="fixed bottom-20 right-4 z-30 flex flex-col items-end gap-3 md:bottom-8 md:right-8">
        <button type="button" aria-label="Text status" onClick={() => setComposer("text")} className="flex size-11 items-center justify-center rounded-2xl bg-card text-foreground shadow-lg ring-1 ring-border" data-testid="text-status"><Pencil className="size-5" /></button>
        <button type="button" aria-label="Photo or video status" onClick={() => setComposer("media")} className="flex size-14 items-center justify-center rounded-2xl bg-primary text-primary-foreground shadow-xl" data-testid="media-status"><Camera className="size-6" /></button>
      </div>

      {composer ? <StatusComposer initial={composer} onClose={() => setComposer(null)} onPosted={() => void refresh()} /> : null}
      {viewing ? <StatusViewer {...viewing} onClose={() => { setViewing(null); void refresh(); }} onChanged={() => void feed.refetch()} /> : null}
      {finding ? <FindChannels onClose={() => setFinding(false)} /> : null}
      {creating ? <CreateChannel onClose={() => setCreating(false)} /> : null}
    </div>
  );
}

/** The latest status as the card's background. */
function StatusThumb({ post }: { post: StatusPost | undefined }) {
  const { url } = useMediaUrl(post?.kind === "IMAGE" ? post.media : null);
  if (!post) return null;
  return (
    <span aria-hidden className="absolute inset-0">
      {post.kind === "TEXT" ? (
        <TextStatusView status={textStatusOfPost(post)} />
      ) : url ? (
        // eslint-disable-next-line @next/next/no-img-element
        <img src={url} alt="" className="size-full object-cover" />
      ) : (
        <span className="block size-full bg-gradient-to-b from-slate-600 to-slate-900" />
      )}
      <span className="absolute inset-0 bg-gradient-to-b from-black/30 via-transparent to-black/60" />
    </span>
  );
}

function ChannelAvatar({ channel, size = "size-12" }: { channel: Pick<Channel, "name">; size?: string }) {
  return <PersonAvatar name={channel.name} size={size} className="rounded-full" />;
}

function preview(c: Channel): string {
  const p = c.lastPost;
  if (!p) return c.description ?? "No posts yet";
  if (p.text) return p.text;
  return p.kind === "IMAGE" ? "Photo" : p.kind === "VIDEO" ? "Video" : p.kind === "AUDIO" ? "Voice message" : "";
}

function ChannelRow({ channel }: { channel: Channel }) {
  return (
    <li>
      <Link href={`/dashboard/updates/channels/${encodeURIComponent(channel.channelId)}`} className="flex items-center gap-3 rounded-2xl px-2 py-2.5 hover:bg-muted/60" data-testid="channel-row">
        <ChannelAvatar channel={channel} />
        <span className="min-w-0 flex-1">
          <span className="flex items-center justify-between gap-2">
            <span className="truncate text-[15px] font-semibold">{channel.name}</span>
            <span className={cn("shrink-0 text-xs", channel.unreadCount ? "font-semibold text-primary" : "text-muted-foreground")}>{channel.lastPost ? shortWhen(channel.lastPost.createdAt) : ""}</span>
          </span>
          <span className="flex items-center justify-between gap-2">
            <span className="truncate text-sm text-muted-foreground">{preview(channel)}</span>
            {channel.unreadCount ? <span className="flex h-5 min-w-5 shrink-0 items-center justify-center rounded-full bg-primary px-1.5 text-[11px] font-bold text-primary-foreground">{channel.unreadCount > 99 ? "99+" : channel.unreadCount}</span> : null}
          </span>
        </span>
      </Link>
    </li>
  );
}

function FollowButton({ channel }: { channel: Channel }) {
  const follow = useFollow();
  if (channel.isOwner) return <span className="shrink-0 text-xs font-semibold text-muted-foreground">Yours</span>;
  return (
    <button type="button" disabled={follow.isPending} onClick={() => follow.mutate({ id: channel.channelId, on: !channel.following }, { onError: (e) => toast(errorText(e), "error") })}
      className={cn("h-9 shrink-0 rounded-full px-4 text-sm font-semibold disabled:opacity-60", channel.following ? "bg-muted text-foreground" : "bg-primary/10 text-primary hover:bg-primary/15")} data-testid="follow-channel">
      {channel.following ? "Following" : "Follow"}
    </button>
  );
}

function Suggestions({ onFind, hide }: { onFind: () => void; hide: string[] }) {
  const directory = useChannelDirectory("");
  const list = (directory.data ?? []).filter((c) => !hide.includes(c.channelId) && !c.following).slice(0, 5);
  return (
    <div className="mt-4">
      {list.length ? (
        <>
          <p className="mb-1 text-sm font-semibold text-muted-foreground">Find channels to follow</p>
          <ul>
            {list.map((c) => (
              <li key={c.channelId} className="flex items-center gap-3 px-2 py-2">
                <ChannelAvatar channel={c} size="size-11" />
                <Link href={`/dashboard/updates/channels/${encodeURIComponent(c.channelId)}`} className="min-w-0 flex-1">
                  <span className="block truncate font-semibold">{c.name}</span>
                  <span className="block text-xs text-muted-foreground">{c.followerCount} {c.followerCount === 1 ? "follower" : "followers"}</span>
                </Link>
                <FollowButton channel={c} />
              </li>
            ))}
          </ul>
        </>
      ) : null}
      <button type="button" onClick={onFind} className="mt-2 h-10 rounded-full bg-primary/10 px-5 text-sm font-semibold text-primary hover:bg-primary/15" data-testid="find-channels">Explore more</button>
    </div>
  );
}

function Sheet({ title, onClose, children }: { title: string; onClose: () => void; children: React.ReactNode }) {
  return (
    <div className="fixed inset-0 z-[60] flex items-end justify-center bg-black/40 sm:items-center" onClick={onClose}>
      <div role="dialog" aria-label={title} className="flex max-h-[85vh] w-full max-w-lg flex-col rounded-t-3xl bg-background p-4 shadow-2xl sm:rounded-3xl" onClick={(e) => e.stopPropagation()}>
        <div className="mb-3 flex items-center justify-between">
          <h2 className="text-lg font-bold">{title}</h2>
          <button type="button" aria-label="Close" onClick={onClose} className="flex size-9 items-center justify-center rounded-full hover:bg-muted"><X className="size-5" /></button>
        </div>
        {children}
      </div>
    </div>
  );
}

function FindChannels({ onClose }: { onClose: () => void }) {
  const [q, setQ] = useState("");
  const directory = useChannelDirectory(q);
  return (
    <Sheet title="Find channels" onClose={onClose}>
      <label className="flex h-11 items-center gap-2 rounded-full bg-muted px-4">
        <Search className="size-4 text-muted-foreground" />
        <input autoFocus value={q} onChange={(e) => setQ(e.target.value)} placeholder="Search channels" className="min-w-0 flex-1 bg-transparent text-sm focus:outline-none" data-testid="channel-search" />
      </label>
      <ul className="mt-3 min-h-0 flex-1 overflow-y-auto">
        {directory.isPending ? [0, 1, 2].map((i) => <li key={i} className="my-2 h-12 animate-pulse rounded-xl bg-muted" />) : null}
        {directory.data?.length === 0 ? <li className="py-8 text-center text-sm text-muted-foreground">No channels found</li> : null}
        {directory.data?.map((c) => (
          <li key={c.channelId} className="flex items-center gap-3 px-1 py-2">
            <ChannelAvatar channel={c} size="size-11" />
            <Link href={`/dashboard/updates/channels/${encodeURIComponent(c.channelId)}`} className="min-w-0 flex-1" onClick={onClose}>
              <span className="block truncate font-semibold">{c.name}</span>
              <span className="block truncate text-xs text-muted-foreground">{c.followerCount} {c.followerCount === 1 ? "follower" : "followers"}{c.description ? ` · ${c.description}` : ""}</span>
            </Link>
            <FollowButton channel={c} />
          </li>
        ))}
      </ul>
    </Sheet>
  );
}

function CreateChannel({ onClose }: { onClose: () => void }) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [busy, setBusy] = useState(false);
  const refresh = useSocialRefresh();
  const router = useRouter();
  const create = async () => {
    if (!name.trim() || busy) return;
    setBusy(true);
    try {
      const c = await socialService.createChannel(name.trim(), description.trim());
      void refresh();
      onClose();
      router.push(`/dashboard/updates/channels/${encodeURIComponent(c.channelId)}`);
    } catch (e) {
      toast(errorText(e, "The channel couldn't be created."), "error");
    } finally {
      setBusy(false);
    }
  };
  return (
    <Sheet title="New channel" onClose={onClose}>
      <form className="space-y-3" onSubmit={(e) => { e.preventDefault(); void create(); }}>
        <input autoFocus value={name} maxLength={80} onChange={(e) => setName(e.target.value)} placeholder="Channel name" className="h-12 w-full rounded-xl border border-border bg-background px-4 text-base focus:border-primary focus:outline-none" data-testid="channel-name" />
        <textarea value={description} maxLength={500} onChange={(e) => setDescription(e.target.value)} placeholder="Describe your channel (optional)" rows={3} className="w-full resize-none rounded-xl border border-border bg-background px-4 py-3 text-base focus:border-primary focus:outline-none" />
        <p className="text-xs text-muted-foreground">Anyone on CF Meet can find and follow your channel. Only you can post in it.</p>
        <button type="submit" disabled={!name.trim() || busy} className="h-12 w-full rounded-full bg-primary text-base font-semibold text-primary-foreground disabled:opacity-50" data-testid="channel-create-submit">{busy ? "Creating…" : "Create channel"}</button>
      </form>
    </Sheet>
  );
}

/** The Updates menu: only actions every signed-in user may take. */
function UpdatesMenu({ open, onOpenChange, onCreateChannel, onFindChannels }: { open: boolean; onOpenChange: (v: boolean) => void; onCreateChannel: () => void; onFindChannels: () => void }) {
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: PointerEvent) => { if (!box.current?.contains(e.target as Node)) onOpenChange(false); };
    const esc = (e: KeyboardEvent) => { if (e.key === "Escape") onOpenChange(false); };
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", esc);
    return () => { document.removeEventListener("pointerdown", close); document.removeEventListener("keydown", esc); };
  }, [open, onOpenChange]);
  const item = "flex w-full items-center gap-3 px-4 py-3 text-left text-sm hover:bg-muted";
  return (
    <div className="relative" ref={box}>
      <button type="button" aria-label="More options" aria-haspopup="menu" aria-expanded={open} onClick={() => onOpenChange(!open)} data-testid="updates-menu"
        className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><MoreVertical className="size-5" /></button>
      {open ? (
        <div role="menu" className="absolute right-0 top-11 z-40 w-56 overflow-hidden rounded-2xl bg-popover py-1 shadow-xl ring-1 ring-border" data-testid="updates-menu-list">
          <button type="button" role="menuitem" className={item} onClick={() => { onOpenChange(false); onCreateChannel(); }}><Plus className="size-4 text-muted-foreground" />Create channel</button>
          <button type="button" role="menuitem" className={item} onClick={() => { onOpenChange(false); onFindChannels(); }}><Compass className="size-4 text-muted-foreground" />Find channels</button>
          <Link role="menuitem" href="/dashboard/updates/privacy" className={item} data-testid="menu-status-privacy"><Lock className="size-4 text-muted-foreground" />Status privacy</Link>
          <Link role="menuitem" href="/dashboard/settings" className={item}><Settings className="size-4 text-muted-foreground" />Settings</Link>
        </div>
      ) : null}
    </div>
  );
}

/** A muted author: view (ring stays neutral), or unmute. */
function MutedRow({ author, onOpen, onChanged }: { author: StatusAuthor; onOpen: () => void; onChanged: () => void }) {
  const [busy, setBusy] = useState(false);
  const unmute = async () => {
    if (!author.person.principalId) return;
    setBusy(true);
    try {
      await socialService.mute(author.person.principalId, false);
      toast(`${author.person.name}'s updates unmuted`);
      onChanged();
    } catch (e) {
      toast(errorText(e), "error");
    } finally {
      setBusy(false);
    }
  };
  const last = author.posts[author.posts.length - 1];
  return (
    <li className="flex items-center gap-3 px-1 py-2" data-testid="muted-author">
      <button type="button" onClick={onOpen} className="flex min-w-0 flex-1 items-center gap-3 text-left opacity-80">
        <StatusRingFrame ring={{ ...ringOfPosts(author.person.userId, author.posts), unseenCount: 0 }}><PersonAvatar person={author.person} size="size-10" /></StatusRingFrame>
        <span className="min-w-0 flex-1">
          <span className="block truncate font-semibold">{author.person.name}</span>
          <span className="block text-xs text-muted-foreground">{last ? shortWhen(last.createdAt) : ""}</span>
        </span>
      </button>
      <button type="button" disabled={busy} onClick={() => void unmute()} className="h-9 shrink-0 rounded-full bg-muted px-3 text-sm font-semibold disabled:opacity-50" data-testid="unmute-author">Unmute</button>
    </li>
  );
}
