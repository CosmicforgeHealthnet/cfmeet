"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { ArrowLeft, Eye, Heart, Mic, Send, Trash2, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { currentMessagingEngine } from "@/lib/messaging/engine";
import { useMediaUrl, useStatusViewers } from "@/hooks/v2/useSocial";
import { socialService, type PersonView, type StatusPost } from "@/services/v2/social.service";
import TextStatusView from "@/components/media-editor/TextStatusView";
import { errorText, PersonAvatar, whenText } from "./shared";
import { textStatusOfPost } from "./status-adapter";

const PHOTO_MS = 5000;

/**
 * Full-screen status viewer (WhatsApp): progress bars, tap right / left for next / previous,
 * hold to pause. Others' statuses: reply in a chat or like; yours: who saw it, delete.
 */
export default function StatusViewer({ person, posts, mine, startAt = 0, onClose, onChanged }: {
  person: PersonView; posts: StatusPost[]; mine: boolean; startAt?: number; onClose: () => void; onChanged: () => void;
}) {
  const [index, setIndex] = useState(Math.min(startAt, Math.max(0, posts.length - 1)));
  const [progress, setProgress] = useState(0);
  const [paused, setPaused] = useState(false);
  const [reply, setReply] = useState("");
  const [liked, setLiked] = useState<Record<string, boolean>>({});
  const [viewersOpen, setViewersOpen] = useState(false);
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
  const hold = paused || viewersOpen || reply.length > 0;
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
  const isLiked = liked[post.postId] ?? post.liked;

  const sendReply = async () => {
    const text = reply.trim();
    if (!text) return;
    const engine = currentMessagingEngine();
    if (!engine || !person.principalId) { toast("Open Chats once, then reply again.", "error"); return; }
    try {
      const chats = engine.getSnapshot().conversations;
      const existing = chats.find((c) => c.contextClass === "DIRECT" && c.securityProfile === "MANAGED" && c.lifecycleState === "ACTIVE" && c.members.includes(person.principalId!));
      const id = existing?.id ?? await engine.createConversation([], "MANAGED", { principalIds: [person.principalId] });
      const what = post.kind === "TEXT" ? `“${(post.text ?? "").slice(0, 60)}”` : post.kind === "IMAGE" ? "photo" : post.kind === "VIDEO" ? "video" : "voice note";
      await engine.send(id, `Replying to your status (${what}): ${text}`);
      setReply("");
      toast("Reply sent");
    } catch (e) {
      toast(errorText(e, "Your reply couldn't be sent."), "error");
    }
  };

  const like = async () => {
    const v = !isLiked;
    setLiked({ ...liked, [post.postId]: v });
    try { await socialService.viewStatus(post.postId, v); } catch { setLiked({ ...liked, [post.postId]: !v }); }
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
            <p className="truncate font-semibold">{mine ? "My status" : person.name}</p>
            <p className="text-xs text-white/75">{whenText(post.createdAt)}</p>
          </div>
          {mine ? <button type="button" aria-label="Delete status" onClick={() => void remove()} className="flex size-9 items-center justify-center"><Trash2 className="size-5" /></button> : null}
          <button type="button" aria-label="Close" onClick={onClose} className="flex size-9 items-center justify-center"><X className="size-6" /></button>
        </div>
      </div>

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
          <div className="flex size-full flex-col items-center justify-center gap-6" style={{ backgroundColor: "#128C7E" }}>
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
          <button type="button" onClick={() => setViewersOpen(true)} className="mx-auto flex items-center gap-2 rounded-full px-4 py-2 text-sm font-semibold hover:bg-white/10" data-testid="status-views">
            <Eye className="size-5" />{post.viewCount ?? 0} {post.viewCount === 1 ? "view" : "views"}
          </button>
        ) : (
          <form className="flex items-center gap-2" onSubmit={(e) => { e.preventDefault(); void sendReply(); }}>
            <input value={reply} onChange={(e) => setReply(e.target.value)} placeholder="Reply" maxLength={1000} className="h-12 min-w-0 flex-1 rounded-full bg-white/10 px-5 text-base text-white placeholder:text-white/60 focus:outline-none" />
            {reply.trim() ? (
              <button type="submit" aria-label="Send reply" className="flex size-12 items-center justify-center rounded-full bg-primary text-primary-foreground"><Send className="size-5" /></button>
            ) : (
              <button type="button" aria-label={isLiked ? "Unlike" : "Like"} onClick={() => void like()} className="flex size-12 items-center justify-center rounded-full bg-white/10">
                <Heart className={cn("size-6", isLiked && "fill-red-500 text-red-500")} />
              </button>
            )}
          </form>
        )}
      </div>

      {viewersOpen ? <Viewers postId={post.postId} onClose={() => setViewersOpen(false)} /> : null}
    </div>
  );
}

function Viewers({ postId, onClose }: { postId: string; onClose: () => void }) {
  const viewers = useStatusViewers(postId);
  return (
    <div className="absolute inset-0 z-30 flex flex-col justify-end bg-black/40" onClick={onClose}>
      <div className="max-h-[70%] overflow-y-auto rounded-t-3xl bg-background p-4 text-foreground" onClick={(e) => e.stopPropagation()} data-testid="status-viewers">
        <div className="mb-3 flex items-center justify-between">
          <p className="text-base font-semibold">Viewed by {viewers.data?.length ?? 0}</p>
          <button type="button" aria-label="Close" onClick={onClose} className="flex size-9 items-center justify-center rounded-full hover:bg-muted"><X className="size-5" /></button>
        </div>
        {viewers.isPending ? <div className="h-12 animate-pulse rounded-xl bg-muted" /> : null}
        {viewers.data?.length === 0 ? <p className="py-6 text-center text-sm text-muted-foreground">No views yet</p> : null}
        <ul className="divide-y divide-border">
          {viewers.data?.map((v) => (
            <li key={v.person.userId} className="flex items-center gap-3 py-2.5">
              <PersonAvatar person={v.person} size="size-10" />
              <span className="min-w-0 flex-1"><span className="block truncate font-medium">{v.person.name}</span><span className="block text-xs text-muted-foreground">{whenText(v.viewedAt)}</span></span>
              {v.liked ? <Heart className="size-5 fill-red-500 text-red-500" aria-label="Liked" /> : null}
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
