"use client";

import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { ArrowLeft, Bell, Flag, ImagePlus, Info, Mic, MoreVertical, Send, Settings2, Share2, SmilePlus, Trash2, UserMinus, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { useChannel, useChannelPosts, useFollow, useMediaUrl, useSocialRefresh } from "@/hooks/v2/useSocial";
import { durationOf, kindOf, LIMITS, socialService, type ChannelPost } from "@/services/v2/social.service";
import { errorText, PersonAvatar, shortWhen } from "./shared";
import { channelMenu } from "@/lib/section-menus";

const CHANNEL_MENU_ICONS: Record<string, typeof Info> = { info: Info, notifications: Bell, share: Share2, report: Flag, unfollow: UserMinus, manage: Settings2, delete: Trash2 };

const QUICK = ["👍", "❤️", "😂", "😮", "😢", "🙏"];

/** One channel (WhatsApp): posts newest at the bottom, reactions, and the owner's composer. */
export default function ChannelPage({ channelId }: { channelId: string }) {
  const router = useRouter();
  const channel = useChannel(channelId);
  const posts = useChannelPosts(channelId);
  const follow = useFollow();
  const refresh = useSocialRefresh();
  const [menu, setMenu] = useState(false);
  const [info, setInfo] = useState(false);
  const bottom = useRef<HTMLDivElement>(null);
  const list = [...(posts.data?.pages.flat() ?? [])].reverse();
  const latest = list.at(-1)?.postId;

  useEffect(() => { bottom.current?.scrollIntoView({ block: "end" }); }, [latest]);

  if (channel.isError) {
    return (
      <div className="mx-auto max-w-md px-6 py-16 text-center">
        <p className="text-lg font-semibold">This channel isn&apos;t available</p>
        <Link href="/dashboard/updates" className="mt-4 inline-block font-semibold text-primary">Back to Updates</Link>
      </div>
    );
  }
  const c = channel.data;

  const remove = async () => {
    if (!c || !window.confirm(`Delete “${c.name}” and all its posts?`)) return;
    try {
      await socialService.deleteChannel(c.channelId);
      void refresh();
      router.push("/dashboard/updates");
    } catch (e) {
      toast(errorText(e), "error");
    }
  };

  // The channel menu (ADR 0073 section menus): only what this person may do in this channel.
  // Notifications, reports and the management screen are not on this build yet (ADR 0072).
  const canShare = typeof navigator !== "undefined" && (typeof navigator.share === "function" || !!navigator.clipboard);
  const items = c ? channelMenu({ isOwner: c.isOwner, following: c.following, notifications: false, report: false, manage: false, share: canShare }) : [];
  const share = async () => {
    if (!c) return;
    const url = `${window.location.origin}/dashboard/updates/channels/${encodeURIComponent(c.channelId)}`;
    try {
      if (typeof navigator.share === "function") await navigator.share({ title: c.name, url });
      else {
        await navigator.clipboard.writeText(url);
        toast("Channel link copied");
      }
    } catch {
      // The share sheet was dismissed.
    }
  };
  const pick = (key: string) => {
    if (!c) return;
    if (key === "info") setInfo(true);
    else if (key === "share") void share();
    else if (key === "unfollow") follow.mutate({ id: c.channelId, on: false }, { onError: (e) => toast(errorText(e), "error") });
    else if (key === "delete") void remove();
  };

  return (
    <div className="flex h-[calc(100dvh-4rem)] flex-col md:h-dvh" data-testid="channel-page">
      <header className="flex items-center gap-2 border-b border-border bg-background px-2 py-2">
        <Link href="/dashboard/updates" aria-label="Back" className="flex size-10 items-center justify-center rounded-full hover:bg-muted"><ArrowLeft className="size-5" /></Link>
        {c ? <PersonAvatar name={c.name} size="size-10" /> : <span className="size-10 animate-pulse rounded-full bg-muted" />}
        <div className="min-w-0 flex-1">
          <p className="truncate font-semibold">{c?.name ?? "Channel"}</p>
          <p className="truncate text-xs text-muted-foreground">{c ? `${c.followerCount} ${c.followerCount === 1 ? "follower" : "followers"}` : ""}</p>
        </div>
        {c && !c.isOwner ? (
          <button type="button" disabled={follow.isPending} onClick={() => follow.mutate({ id: c.channelId, on: !c.following }, { onError: (e) => toast(errorText(e), "error") })}
            className={cn("h-9 rounded-full px-4 text-sm font-semibold disabled:opacity-60", c.following ? "bg-muted" : "bg-primary text-primary-foreground")} data-testid="channel-follow">
            {c.following ? "Following" : "Follow"}
          </button>
        ) : null}
        {items.length ? (
          <div className="relative">
            <button type="button" aria-label="Channel options" aria-haspopup="menu" aria-expanded={menu} onClick={() => setMenu(!menu)} data-testid="channel-menu" className="flex size-10 items-center justify-center rounded-full hover:bg-muted"><MoreVertical className="size-5" /></button>
            {menu ? (
              <div role="menu" className="absolute right-0 top-11 z-20 w-52 rounded-2xl border border-border bg-popover p-1 shadow-xl" data-testid="channel-menu-items">
                {items.map(({ key, label, danger }) => {
                  const Icon = CHANNEL_MENU_ICONS[key] ?? Info;
                  return (
                    <button key={key} type="button" role="menuitem" data-testid={`channel-menu-${key}`} onClick={() => { setMenu(false); pick(key); }}
                      className={cn("flex w-full items-center gap-2 rounded-xl px-3 py-2.5 text-sm hover:bg-muted", danger && "text-destructive")}>
                      <Icon className={cn("size-4", !danger && "text-muted-foreground")} />{label}
                    </button>
                  );
                })}
              </div>
            ) : null}
          </div>
        ) : null}
      </header>
      {info && c ? (
        <div className="fixed inset-0 z-[60] flex items-end justify-center bg-black/40 sm:items-center" onClick={() => setInfo(false)}>
          <div role="dialog" aria-label="Channel info" className="w-full max-w-md rounded-t-3xl bg-background p-5 shadow-2xl sm:rounded-3xl" onClick={(e) => e.stopPropagation()} data-testid="channel-info">
            <div className="flex items-center gap-3">
              <PersonAvatar name={c.name} size="size-14" />
              <div className="min-w-0 flex-1">
                <p className="truncate text-lg font-bold">{c.name}</p>
                <p className="text-sm text-muted-foreground">{c.followerCount} {c.followerCount === 1 ? "follower" : "followers"}{c.isOwner ? " · Your channel" : ""}</p>
              </div>
              <button type="button" aria-label="Close" onClick={() => setInfo(false)} className="flex size-9 items-center justify-center rounded-full hover:bg-muted"><X className="size-5" /></button>
            </div>
            {c.description ? <p className="mt-4 whitespace-pre-wrap text-sm">{c.description}</p> : null}
            <p className="mt-4 text-xs text-muted-foreground">Created {new Date(c.createdAt).toLocaleDateString([], { day: "numeric", month: "long", year: "numeric" })}. Only the owner posts; followers can react.</p>
          </div>
        </div>
      ) : null}

      <div className="chat-wallpaper min-h-0 flex-1 overflow-y-auto px-3 py-3">
        <div className="mx-auto max-w-2xl space-y-3">
          {c?.description ? <p className="mx-auto max-w-md rounded-xl bg-background/90 px-4 py-2.5 text-center text-sm text-muted-foreground shadow-sm ring-1 ring-border">{c.description}</p> : null}
          {posts.hasNextPage ? (
            <button type="button" onClick={() => void posts.fetchNextPage()} className="mx-auto block h-8 rounded-full px-4 text-sm font-semibold text-primary hover:bg-primary/10">Load older posts</button>
          ) : null}
          {posts.isPending ? [0, 1].map((i) => <div key={i} className="h-24 w-4/5 animate-pulse rounded-2xl bg-muted" />) : null}
          {!posts.isPending && list.length === 0 ? <p className="py-12 text-center text-sm text-muted-foreground">{c?.isOwner ? "Post the first update for your followers." : "No posts yet."}</p> : null}
          {list.map((p) => <PostBubble key={p.postId} post={p} owner={!!c?.isOwner} onChanged={() => void posts.refetch()} />)}
          <div ref={bottom} />
        </div>
      </div>

      {c?.isOwner ? <Composer channelId={c.channelId} onPosted={() => { void posts.refetch(); void refresh(); }} /> : null}
    </div>
  );
}

function PostBubble({ post, owner, onChanged }: { post: ChannelPost; owner: boolean; onChanged: () => void }) {
  const { url, failed } = useMediaUrl(post.media);
  const [picking, setPicking] = useState(false);
  const react = async (emoji: string | null) => {
    setPicking(false);
    try { await socialService.react(post.channelId, post.postId, emoji); onChanged(); } catch (e) { toast(errorText(e), "error"); }
  };
  const remove = async () => {
    if (!window.confirm("Delete this post?")) return;
    try { await socialService.deletePost(post.channelId, post.postId); onChanged(); } catch (e) { toast(errorText(e), "error"); }
  };
  return (
    <div className="group max-w-[85%]" data-testid="channel-post">
      <div className="overflow-hidden rounded-2xl rounded-tl-md bg-card shadow-sm ring-1 ring-border">
        {post.media ? (
          !url ? (
            <div className="flex h-48 w-72 max-w-full items-center justify-center bg-muted text-sm text-muted-foreground">{failed ? "Couldn't load" : "Loading…"}</div>
          ) : post.kind === "IMAGE" ? (
            // eslint-disable-next-line @next/next/no-img-element
            <img src={url} alt="" className="max-h-96 w-full object-cover" />
          ) : post.kind === "VIDEO" ? (
            <video src={url} controls playsInline className="max-h-96 w-full bg-black" />
          ) : (
            <audio src={url} controls className="m-3 w-64 max-w-[calc(100%-1.5rem)]" />
          )
        ) : null}
        <div className="px-3.5 py-2">
          {post.text ? <p className="whitespace-pre-wrap break-words text-[15px]">{post.text}</p> : null}
          <p className="mt-0.5 text-right text-[11px] text-muted-foreground">{shortWhen(post.createdAt)}</p>
        </div>
      </div>
      <div className="mt-1 flex flex-wrap items-center gap-1">
        {post.reactions.map((r) => (
          <button key={r.emoji} type="button" onClick={() => void react(post.myReaction === r.emoji ? null : r.emoji)}
            className={cn("flex h-7 items-center gap-1 rounded-full px-2 text-sm ring-1", post.myReaction === r.emoji ? "bg-primary/10 ring-primary/40" : "bg-background ring-border")}>
            {r.emoji}<span className="text-xs text-muted-foreground">{r.count}</span>
          </button>
        ))}
        <div className="relative">
          <button type="button" aria-label="React" onClick={() => setPicking(!picking)} className="flex size-7 items-center justify-center rounded-full text-muted-foreground hover:bg-muted" data-testid="react-post"><SmilePlus className="size-4" /></button>
          {picking ? (
            <div className="absolute bottom-8 left-0 z-10 flex gap-1 rounded-full border border-border bg-popover p-1 shadow-xl">
              {QUICK.map((e) => <button key={e} type="button" onClick={() => void react(e)} className="flex size-9 items-center justify-center rounded-full text-xl hover:bg-muted">{e}</button>)}
            </div>
          ) : null}
        </div>
        {owner ? <button type="button" aria-label="Delete post" onClick={() => void remove()} className="flex size-7 items-center justify-center rounded-full text-muted-foreground opacity-0 hover:bg-muted group-hover:opacity-100 [@media(hover:none)]:opacity-100"><Trash2 className="size-4" /></button> : null}
      </div>
    </div>
  );
}

function Composer({ channelId, onPosted }: { channelId: string; onPosted: () => void }) {
  const [text, setText] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [busy, setBusy] = useState<number | null>(null);
  const picker = useRef<HTMLInputElement>(null);
  const send = async () => {
    if (busy !== null || (!text.trim() && !file)) return;
    setBusy(0);
    try {
      let mediaId: string | undefined;
      if (file) {
        const kind = kindOf(file);
        if (!kind) throw new Error("Choose a photo, video or audio file.");
        if (file.size > LIMITS[kind]) throw new Error(kind === "VIDEO" ? "Videos can be at most 64 MB." : "Files can be at most 8 MB.");
        const duration = kind === "IMAGE" ? null : await durationOf(file).catch(() => null);
        mediaId = (await socialService.upload(file, "CHANNEL", kind, duration, (p) => setBusy(p))).mediaId;
      }
      await socialService.post(channelId, { text: text.trim() || undefined, mediaId });
      setText(""); setFile(null);
      onPosted();
    } catch (e) {
      toast(e instanceof Error && !("response" in e) ? e.message : errorText(e, "Your post couldn't be sent."), "error");
    } finally {
      setBusy(null);
    }
  };
  return (
    <div className="border-t border-border bg-background p-2 pb-[max(env(safe-area-inset-bottom),0.5rem)]">
      {file ? (
        <div className="mx-auto mb-2 flex max-w-2xl items-center gap-2 rounded-2xl bg-muted px-3 py-2 text-sm">
          {file.type.startsWith("audio/") ? <Mic className="size-4" /> : <ImagePlus className="size-4" />}
          <span className="min-w-0 flex-1 truncate">{file.name}</span>
          <button type="button" aria-label="Remove file" onClick={() => setFile(null)} className="flex size-7 items-center justify-center rounded-full hover:bg-background"><X className="size-4" /></button>
        </div>
      ) : null}
      <form className="mx-auto flex max-w-2xl items-end gap-1.5 rounded-[26px] border border-border bg-card p-1.5" onSubmit={(e) => { e.preventDefault(); void send(); }}>
        <input ref={picker} type="file" accept="image/*,video/*,audio/*" className="hidden" onChange={(e) => { setFile(e.target.files?.[0] ?? null); e.target.value = ""; }} />
        <button type="button" aria-label="Add photo or video" onClick={() => picker.current?.click()} className="flex size-10 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted"><ImagePlus className="size-5" /></button>
        <textarea value={text} onChange={(e) => setText(e.target.value)} maxLength={4000} rows={1} placeholder={file ? "Add a caption…" : "Write an update"} data-testid="channel-post-text"
          onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && window.matchMedia("(hover: hover)").matches) { e.preventDefault(); void send(); } }}
          className="max-h-32 min-h-10 min-w-0 flex-1 resize-none bg-transparent px-2 py-2 text-[15px] focus:outline-none" />
        <button type="submit" aria-label="Post" disabled={busy !== null || (!text.trim() && !file)} className="flex size-10 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground disabled:opacity-50" data-testid="channel-post-send">
          {busy !== null ? <span className="text-[10px] font-bold">{Math.round(busy * 100)}%</span> : <Send className="size-5" />}
        </button>
      </form>
    </div>
  );
}
