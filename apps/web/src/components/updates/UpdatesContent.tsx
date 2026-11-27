"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Camera, Mic, Pencil, Plus, Search, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { useChannelDirectory, useFollow, useFollowedChannels, useMediaUrl, useSocialRefresh, useStatusFeed } from "@/hooks/v2/useSocial";
import { useAccountProfile } from "@/hooks/useAccount";
import { socialService, type Channel, type PersonView, type StatusPost } from "@/services/v2/social.service";
import StatusComposer from "./StatusComposer";
import StatusViewer from "./StatusViewer";
import { errorText, FONTS, PersonAvatar, shortWhen } from "./shared";

/** Updates (WhatsApp): Status on top, Channels below. */
export default function UpdatesContent() {
  const feed = useStatusFeed();
  const channels = useFollowedChannels();
  const refresh = useSocialRefresh();
  const profile = useAccountProfile();
  const [composer, setComposer] = useState<null | "text" | "media" | "voice">(null);
  const [viewing, setViewing] = useState<{ person: PersonView; posts: StatusPost[]; mine: boolean; startAt: number } | null>(null);
  const [finding, setFinding] = useState(false);
  const [creating, setCreating] = useState(false);

  const me: PersonView = {
    userId: "me",
    name: profile.data?.effectiveDisplayName || "You",
    avatarUrl: profile.data?.avatarUrl ?? null,
    principalId: null,
  };
  const mine = feed.data?.mine ?? [];
  const contacts = feed.data?.contacts ?? [];
  const followed = channels.data ?? [];

  return (
    <div className="mx-auto max-w-3xl px-4 pb-28 pt-6 sm:px-6 lg:py-8" data-testid="updates-page">
      <header className="flex items-center justify-between">
        <h1 className="text-2xl font-bold tracking-tight">Updates</h1>
        <button type="button" aria-label="Find channels" onClick={() => setFinding(true)} className="flex size-10 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground"><Search className="size-5" /></button>
      </header>

      <section className="mt-5" aria-labelledby="status-heading">
        <h2 id="status-heading" className="text-lg font-bold">Status</h2>
        <div className="no-scrollbar -mx-4 mt-3 flex gap-3 overflow-x-auto px-4 pb-1" data-testid="status-row">
          <button type="button" data-testid="my-status"
            onClick={() => (mine.length ? setViewing({ person: me, posts: mine, mine: true, startAt: 0 }) : setComposer("media"))}
            className="relative flex h-44 w-28 shrink-0 flex-col justify-between overflow-hidden rounded-2xl border border-border bg-card p-2.5 text-left">
            {mine.length ? <StatusThumb post={mine[mine.length - 1]} /> : null}
            <span className="relative self-start">
              <PersonAvatar person={me} size="size-11" ring={mine.length ? "seen" : null} />
              <span className="absolute -bottom-0.5 -right-0.5 flex size-5 items-center justify-center rounded-full bg-primary text-primary-foreground ring-2 ring-card"
                onClick={(e) => { e.stopPropagation(); setComposer("media"); }} role="presentation"><Plus className="size-3.5" strokeWidth={3} /></span>
            </span>
            <span className={cn("relative text-sm font-semibold", mine.length && "text-white drop-shadow")}>{mine.length ? "My status" : "Add status"}</span>
          </button>
          {feed.isPending ? [0, 1, 2].map((i) => <span key={i} className="h-44 w-28 shrink-0 animate-pulse rounded-2xl bg-muted" />) : null}
          {contacts.map((c) => (
            <button key={c.person.userId} type="button" data-testid="contact-status"
              onClick={() => setViewing({ person: c.person, posts: c.posts, mine: false, startAt: Math.max(0, c.posts.findIndex((p) => !p.viewed)) })}
              className="relative flex h-44 w-28 shrink-0 flex-col justify-between overflow-hidden rounded-2xl bg-muted p-2.5 text-left">
              <StatusThumb post={c.posts[c.posts.length - 1]} />
              <PersonAvatar person={c.person} size="size-10" ring={c.allViewed ? "seen" : "unseen"} className="relative self-start" />
              <span className="relative truncate text-sm font-semibold text-white drop-shadow">{c.person.name}</span>
            </button>
          ))}
        </div>
        {!feed.isPending && contacts.length === 0 ? (
          <p className="mt-2 text-sm text-muted-foreground">Status updates from your contacts show here for 24 hours.</p>
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
        <button type="button" aria-label="Voice status" onClick={() => setComposer("voice")} className="flex size-11 items-center justify-center rounded-2xl bg-card text-foreground shadow-lg ring-1 ring-border"><Mic className="size-5" /></button>
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
        <span className={cn("flex size-full items-center justify-center p-3 text-center text-xs leading-tight text-white", FONTS[post.font] ?? FONTS[0])} style={{ backgroundColor: post.background ?? "#128C7E" }}>
          <span className="line-clamp-5">{post.text}</span>
        </span>
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
