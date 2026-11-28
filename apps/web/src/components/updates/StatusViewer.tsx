"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowLeft, BellOff, Bell, Eye, Forward, MoreVertical, Mic, Repeat2, Send, SmilePlus, Star, Trash2, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { useEligibleContacts, useMediaUrl, useStatusViewers } from "@/hooks/v2/useSocial";
import { socialService, type PersonView, type StatusPost } from "@/services/v2/social.service";
import { canShare, reactionSummary, uniqueViewers } from "@/lib/status-privacy";
import TextStatusView from "@/components/media-editor/TextStatusView";
import { AudienceButton, Overlay, PeopleChecklist, useStatusAudience } from "./audience";
import { errorText, PersonAvatar, whenText } from "./shared";
import { textStatusOfPost } from "./status-adapter";

const PHOTO_MS = 5000;
/** Quick reactions (WhatsApp's status reactions). */
export const STATUS_REACTIONS = ["❤️", "😂", "😮", "😢", "🙏", "👏", "🔥", "🎉"];
const MAX_FORWARD = 20;

/**
 * Full-screen status viewer (WhatsApp): progress bars, tap right / left for next / previous,
 * hold to pause. Others' statuses: reply privately (a message in your chat with them), react,
 * forward or reshare when the author allows sharing (never beyond the original audience: the
 * server checks every recipient), mute or unmute their updates; "mentioned you" shows only to
 * the person mentioned. Yours: who viewed it (once each, with times and reactions), delete.
 */
export default function StatusViewer({ person, posts, mine, startAt = 0, muted, onClose, onChanged }: {
  person: PersonView; posts: StatusPost[]; mine: boolean; startAt?: number;
  /** Whether the author's updates are muted (others' statuses only). */
  muted?: boolean;
  onClose: () => void; onChanged: () => void;
}) {
  const [index, setIndex] = useState(Math.min(startAt, Math.max(0, posts.length - 1)));
  const [progress, setProgress] = useState(0);
  const [paused, setPaused] = useState(false);
  const [reply, setReply] = useState("");
  const [sending, setSending] = useState(false);
  const [reactions, setReactions] = useState<Record<string, string | null>>({});
  const [panel, setPanel] = useState<null | "viewers" | "menu" | "react" | "forward" | "reshare">(null);
  const [isMuted, setMuted] = useState(!!muted);
  const post = posts[index];
  const { url } = useMediaUrl(post?.media);
  const media = useRef<HTMLVideoElement | HTMLAudioElement | null>(null);
  const seen = useRef(new Set<string>());

  const next = useCallback(() => { setProgress(0); if (index + 1 < posts.length) setIndex(index + 1); else onClose(); }, [index, posts.length, onClose]);
  const prev = () => { setProgress(0); if (index > 0) setIndex(index - 1); };

  // Seen by others (their owner sees "Viewed by").
  useEffect(() => {
    if (!post || mine || seen.current.has(post.postId)) return;
    seen.current.add(post.postId);
    void socialService.viewStatus(post.postId).then(onChanged, () => {});
  }, [post, mine, onChanged]);

  // Text and photos run on a timer; videos and voice notes follow their playback.
  const hold = paused || panel !== null || reply.length > 0;
  useEffect(() => {
    if (!post || post.kind === "VIDEO" || post.kind === "AUDIO" || hold) return;
    if (post.kind === "IMAGE" && !url) return;
    const total = post.kind === "TEXT" ? Math.min(10_000, 4000 + (post.text?.length ?? 0) * 40) : PHOTO_MS;
    const started = Date.now() - progress * total;
    const t = setInterval(() => {
      const p = (Date.now() - started) / total;
      if (p >= 1) { clearInterval(t); next(); } else setProgress(p);
    }, 50);
    return () => clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [post?.postId, hold, url]);

  useEffect(() => {
    const el = media.current;
    if (!el) return;
    if (hold) el.pause(); else void el.play().catch(() => {});
  }, [hold, url]);

  if (!post) return null;
  const start = (post.startMs ?? 0) / 1000;
  const end = post.endMs !== null ? post.endMs / 1000 : null;
  const onTime = () => {
    const el = media.current;
    if (!el) return;
    const stop = end ?? (Number.isFinite(el.duration) ? el.duration : null);
    if (stop === null) return;
    setProgress(Math.max(0, Math.min(1, (el.currentTime - start) / Math.max(0.1, stop - start))));
    if (el.currentTime >= stop - 0.05) next();
  };
  const reaction = post.postId in reactions ? reactions[post.postId] : post.reaction ?? (post.liked ? STATUS_REACTIONS[0] : null);
  const shareable = canShare(post, mine);
  const author = person.principalId;

  const sendReply = async () => {
    const text = reply.trim();
    if (!text || sending) return;
    setSending(true);
    try {
      // Messaging sends it as a private message in the direct chat with the author.
      await socialService.replyToStatus(post.postId, text);
      setReply("");
      toast("Reply sent");
    } catch (e) {
      toast(errorText(e, "Your reply couldn't be sent."), "error");
    } finally {
      setSending(false);
    }
  };

  const react = async (emoji: string | null) => {
    const before = reaction;
    setReactions({ ...reactions, [post.postId]: emoji });
    setPanel(null);
    try { await socialService.reactToStatus(post.postId, emoji); onChanged(); } catch (e) {
      setReactions({ ...reactions, [post.postId]: before });
      toast(errorText(e, "Your reaction couldn't be sent."), "error");
    }
  };

  const toggleMute = async () => {
    if (!author) return;
    const v = !isMuted;
    setMuted(v);
    setPanel(null);
    try {
      await socialService.mute(author, v);
      toast(v ? `${person.name}'s updates muted` : `${person.name}'s updates unmuted`);
      onChanged();
    } catch (e) {
      setMuted(!v);
      toast(errorText(e), "error");
    }
  };

  const remove = async () => {
    try {
      await socialService.deleteStatus(post.postId);
      toast("Status deleted");
      onChanged();
      if (posts.length <= 1) onClose(); else if (index >= posts.length - 1) setIndex(index - 1);
    } catch (e) {
      toast(errorText(e), "error");
    }
  };

  return (
    <div className="fixed inset-0 z-[70] flex select-none flex-col bg-black text-white" role="dialog" aria-label={`Status from ${person.name}`} data-testid="status-viewer">
      <div className="absolute inset-x-0 top-0 z-20 space-y-3 bg-gradient-to-b from-black/70 to-transparent px-3 pb-6 pt-3">
        <div className="flex gap-1">
          {posts.map((p, i) => (
            <span key={p.postId} className="h-[3px] flex-1 overflow-hidden rounded-full bg-white/30">
              <span className="block h-full bg-white" style={{ width: `${i < index ? 100 : i === index ? progress * 100 : 0}%` }} />
            </span>
          ))}
        </div>
        <div className="flex items-center gap-3">
          <button type="button" aria-label="Back" onClick={onClose} className="flex size-9 items-center justify-center"><ArrowLeft className="size-6" /></button>
          <PersonAvatar person={person} size="size-10" />
          <div className="min-w-0 flex-1">
            <p className="flex items-center gap-1.5 truncate font-semibold">
              <span className="truncate">{mine ? "My status" : person.name}</span>
              {post.closeFriends ? <span className="inline-flex shrink-0 items-center gap-0.5 rounded-full bg-emerald-500 px-1.5 py-0.5 text-[10px] font-bold text-white"><Star className="size-2.5 fill-current" />Close friends</span> : null}
            </p>
            <p className="truncate text-xs text-white/75">{whenText(post.createdAt)}{post.origin ? " · Reshared" : ""}</p>
          </div>
          {mine ? <button type="button" aria-label="Delete status" onClick={() => void remove()} className="flex size-9 items-center justify-center"><Trash2 className="size-5" /></button> : (
            <button type="button" aria-label="More options" onClick={() => setPanel(panel === "menu" ? null : "menu")} className="flex size-9 items-center justify-center" data-testid="status-menu"><MoreVertical className="size-5" /></button>
          )}
          <button type="button" aria-label="Close" onClick={onClose} className="flex size-9 items-center justify-center"><X className="size-6" /></button>
        </div>
        {post.mentionedMe ? <p className="mx-auto w-fit rounded-full bg-primary px-3 py-1 text-xs font-semibold text-primary-foreground" data-testid="status-mentioned-you">{person.name} mentioned you</p> : null}
      </div>

      {panel === "menu" ? (
        <div className="absolute right-3 top-24 z-30 w-56 overflow-hidden rounded-2xl bg-background text-foreground shadow-2xl" data-testid="status-menu-list">
          {author ? (
            <button type="button" onClick={() => void toggleMute()} className="flex w-full items-center gap-3 px-4 py-3 text-left text-sm hover:bg-muted" data-testid="status-mute">
              {isMuted ? <Bell className="size-4" /> : <BellOff className="size-4" />}{isMuted ? "Unmute updates" : "Mute updates"}
            </button>
          ) : null}
          {shareable ? (
            <>
              <button type="button" onClick={() => setPanel("forward")} className="flex w-full items-center gap-3 px-4 py-3 text-left text-sm hover:bg-muted" data-testid="status-forward"><Forward className="size-4" />Forward</button>
              <button type="button" onClick={() => setPanel("reshare")} className="flex w-full items-center gap-3 px-4 py-3 text-left text-sm hover:bg-muted" data-testid="status-reshare"><Repeat2 className="size-4" />Reshare to my status</button>
            </>
          ) : <p className="px-4 py-3 text-xs text-muted-foreground">{person.name} doesn&apos;t allow sharing this status.</p>}
        </div>
      ) : null}

      <div className="relative flex min-h-0 flex-1 items-center justify-center"
        onPointerDown={() => setPaused(true)} onPointerUp={() => setPaused(false)} onPointerLeave={() => setPaused(false)} onContextMenu={(e) => e.preventDefault()}>
        {post.kind === "TEXT" ? (
          <TextStatusView status={textStatusOfPost(post)} />
        ) : !url ? (
          <span className="size-10 animate-spin rounded-full border-2 border-white/30 border-t-white" aria-label="Loading" />
        ) : post.kind === "IMAGE" ? (
          // eslint-disable-next-line @next/next/no-img-element
          <img src={url} alt="" className="max-h-full max-w-full object-contain" />
        ) : post.kind === "VIDEO" ? (
          <video key={post.postId} ref={(el) => { media.current = el; }} src={url} playsInline autoPlay className="max-h-full max-w-full"
            onLoadedMetadata={(e) => { e.currentTarget.currentTime = start; }} onTimeUpdate={onTime} onEnded={next} />
        ) : (
          <div className="flex size-full flex-col items-center justify-center gap-6" style={{ backgroundColor: post.background ?? "#128C7E" }}>
            <span className="flex size-24 items-center justify-center rounded-full bg-black/30"><Mic className="size-10" /></span>
            <audio key={post.postId} ref={(el) => { media.current = el; }} src={url} autoPlay onTimeUpdate={onTime} onEnded={next} />
          </div>
        )}
        {post.caption ? <p className="absolute inset-x-0 bottom-4 mx-4 rounded-xl bg-black/50 px-4 py-3 text-center text-base">{post.caption}</p> : null}
        <button type="button" aria-label="Previous" className="absolute inset-y-0 left-0 w-1/3" onClick={prev} />
        <button type="button" aria-label="Next" className="absolute inset-y-0 right-0 w-1/3" onClick={next} />
      </div>

      <div className="px-3 pb-[max(env(safe-area-inset-bottom),0.75rem)] pt-2">
        {mine ? (
          <button type="button" onClick={() => setPanel("viewers")} className="mx-auto flex items-center gap-2 rounded-full px-4 py-2 text-sm font-semibold hover:bg-white/10" data-testid="status-views">
            <Eye className="size-5" />{post.viewCount ?? 0} {post.viewCount === 1 ? "view" : "views"}
          </button>
        ) : (
          <>
            {panel === "react" ? (
              <div className="mb-2 flex flex-wrap justify-center gap-1" role="group" aria-label="React" data-testid="status-reactions">
                {STATUS_REACTIONS.map((e) => (
                  <button key={e} type="button" aria-label={`React ${e}`} aria-pressed={reaction === e} onClick={() => void react(reaction === e ? null : e)}
                    className={cn("flex size-11 items-center justify-center rounded-full text-2xl hover:bg-white/15", reaction === e && "bg-white/25")}>{e}</button>
                ))}
              </div>
            ) : null}
            <form className="flex items-center gap-2" onSubmit={(e) => { e.preventDefault(); void sendReply(); }}>
              <input value={reply} onChange={(e) => setReply(e.target.value)} placeholder="Reply privately" aria-label="Reply" maxLength={1000} data-testid="status-reply"
                className="h-12 min-w-0 flex-1 rounded-full bg-white/10 px-5 text-base text-white placeholder:text-white/60 focus:outline-none" />
              {reply.trim() ? (
                <button type="submit" disabled={sending} aria-label="Send reply" className="flex size-12 items-center justify-center rounded-full bg-primary text-primary-foreground disabled:opacity-60"><Send className="size-5" /></button>
              ) : (
                <button type="button" aria-label={reaction ? `Your reaction ${reaction}` : "React"} onClick={() => setPanel(panel === "react" ? null : "react")} data-testid="status-react"
                  className="flex size-12 items-center justify-center rounded-full bg-white/10 text-2xl">
                  {reaction ?? <SmilePlus className="size-6" />}
                </button>
              )}
            </form>
          </>
        )}
      </div>

      {panel === "viewers" ? <Viewers postId={post.postId} onClose={() => setPanel(null)} /> : null}
      {panel === "forward" ? <ForwardSheet post={post} author={author} onClose={() => setPanel(null)} /> : null}
      {panel === "reshare" ? <ReshareSheet post={post} onClose={() => setPanel(null)} onDone={() => { setPanel(null); onChanged(); }} /> : null}
    </div>
  );
}

function Viewers({ postId, onClose }: { postId: string; onClose: () => void }) {
  const viewers = useStatusViewers(postId);
  const list = uniqueViewers((viewers.data ?? []).map((v) => ({ ...v, principalId: v.person.principalId ?? v.person.userId })));
  const summary = reactionSummary(list);
  return (
    <div className="absolute inset-0 z-30 flex flex-col justify-end bg-black/40" onClick={onClose}>
      <div className="max-h-[70%] overflow-y-auto rounded-t-3xl bg-background p-4 text-foreground" onClick={(e) => e.stopPropagation()} data-testid="status-viewers">
        <div className="mb-3 flex items-center justify-between">
          <p className="text-base font-semibold">Viewed by {list.length}</p>
          <button type="button" aria-label="Close" onClick={onClose} className="flex size-9 items-center justify-center rounded-full hover:bg-muted"><X className="size-5" /></button>
        </div>
        {summary.length ? <p className="mb-2 flex flex-wrap gap-3 text-sm" data-testid="status-reaction-summary">{summary.map((r) => <span key={r.emoji}>{r.emoji} {r.count}</span>)}</p> : null}
        {viewers.isPending ? <div className="h-12 animate-pulse rounded-xl bg-muted" /> : null}
        {viewers.isError ? <p className="py-6 text-center text-sm text-muted-foreground">Viewers couldn&apos;t load.</p> : null}
        {viewers.data?.length === 0 ? <p className="py-6 text-center text-sm text-muted-foreground">No views yet</p> : null}
        <ul className="divide-y divide-border">
          {list.map((v) => (
            <li key={v.principalId} className="flex items-center gap-3 py-2.5">
              <PersonAvatar person={v.person} size="size-10" />
              <span className="min-w-0 flex-1"><span className="block truncate font-medium">{v.person.name}</span><span className="block text-xs text-muted-foreground">{whenText(v.viewedAt)}</span></span>
              {v.reaction ? <span className="text-xl" aria-label={`Reacted ${v.reaction}`}>{v.reaction}</span> : v.liked ? <span className="text-xl" aria-label="Liked">{STATUS_REACTIONS[0]}</span> : null}
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

/** Forward to up to 20 contacts; the server skips anyone outside the status's audience. */
function ForwardSheet({ post, author, onClose }: { post: StatusPost; author: string | null; onClose: () => void }) {
  const eligible = useEligibleContacts();
  const [selected, setSelected] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const people = (eligible.data ?? []).filter((p) => (p.principalId ?? p.userId) !== author);
  const send = async () => {
    if (!selected.length || busy) return;
    setBusy(true);
    try {
      const r = await socialService.forwardStatus(post.postId, selected);
      toast(r.forwarded ? `Forwarded to ${r.forwarded}${r.skipped ? ` · ${r.skipped} outside the status's audience` : ""}` : "No one you chose can see this status", r.forwarded ? "success" : "error");
      onClose();
    } catch (e) {
      toast(errorText(e, "The status couldn't be forwarded."), "error");
    } finally {
      setBusy(false);
    }
  };
  return (
    <Overlay title="Forward status" onClose={onClose} testId="status-forward-sheet"
      footer={(
        <div className="flex items-center gap-3">
          <p className="min-w-0 flex-1 text-sm text-muted-foreground">{selected.length > MAX_FORWARD ? `At most ${MAX_FORWARD} people` : "Only people who can already see it will get it."}</p>
          <button type="button" disabled={!selected.length || selected.length > MAX_FORWARD || busy} onClick={() => void send()} data-testid="status-forward-send"
            className="h-11 shrink-0 rounded-full bg-primary px-6 text-sm font-semibold text-primary-foreground disabled:opacity-50">{busy ? "Sending…" : "Send"}</button>
        </div>
      )}>
      <PeopleChecklist people={people} loading={eligible.isPending} selected={selected} onChange={setSelected} />
    </Overlay>
  );
}

/** Reshare as my status, attributed to the author; readers must also pass the original's audience. */
function ReshareSheet({ post, onClose, onDone }: { post: StatusPost; onClose: () => void; onDone: () => void }) {
  const audience = useStatusAudience();
  const [caption, setCaption] = useState("");
  const [busy, setBusy] = useState(false);
  const share = async () => {
    setBusy(true);
    try {
      const o = audience.jobOptions();
      await socialService.reshareStatus(post.postId, { ...(caption.trim() ? { caption: caption.trim() } : {}), ...(o.audience ? { audience: o.audience } : {}) });
      toast("Reshared to your status");
      onDone();
    } catch (e) {
      toast(errorText(e, "The status couldn't be reshared."), "error");
    } finally {
      setBusy(false);
    }
  };
  return (
    <Overlay title="Reshare to my status" onClose={onClose} testId="status-reshare-sheet"
      footer={<button type="button" disabled={busy} onClick={() => void share()} data-testid="status-reshare-send" className="h-11 w-full rounded-full bg-primary text-sm font-semibold text-primary-foreground disabled:opacity-50">{busy ? "Sharing…" : "Reshare"}</button>}>
      <input value={caption} maxLength={700} onChange={(e) => setCaption(e.target.value)} placeholder="Add a caption (optional)" aria-label="Caption"
        className="h-12 w-full rounded-xl border border-border bg-background px-4 text-base focus:border-primary focus:outline-none" />
      <div className="mt-3 flex items-center justify-between gap-2">
        <span className="text-sm text-muted-foreground">Who can see it</span>
        <AudienceButton audience={audience} tone="light" showSharing={false} />
      </div>
      <p className="mt-3 text-xs text-muted-foreground">Only people who can also see the original status will see your reshare.</p>
    </Overlay>
  );
}
