"use client";

import { useEffect, useState, useSyncExternalStore } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Lock } from "lucide-react";
import { toast } from "@/components/ui/toast";
import { followersText, inviteToken } from "@/lib/channels";
import { channelsService, type InvitePreview } from "@/services/v2/channels.service";
import { errorText } from "@/components/updates/shared";
import { useChannelsRefresh } from "@/hooks/v2/useChannels";
import { ChannelAvatar, VerifiedBadge } from "./bits";

function subscribeHash(onChange: () => void) {
  window.addEventListener("hashchange", onChange);
  return () => window.removeEventListener("hashchange", onChange);
}

/** An invite link (`…/channels/join#mci_…`): preview the channel, then join it. */
export default function JoinChannel() {
  const router = useRouter();
  const refresh = useChannelsRefresh();
  const hash = useSyncExternalStore(subscribeHash, () => window.location.hash, () => null);
  const token = hash === null ? undefined : inviteToken(hash);
  const [preview, setPreview] = useState<InvitePreview | null>(null);
  const [failed, setFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (!token) return;
    let live = true;
    channelsService.previewInvite(token).then((p) => { if (live) setPreview(p); }, () => { if (live) setFailed(true); });
    return () => { live = false; };
  }, [token]);
  const join = async () => {
    if (!token) return;
    setBusy(true);
    try {
      const c = await channelsService.joinInvite(token);
      await refresh(c.channelId);
      router.replace(`/dashboard/updates/channels/${c.channelId}`);
    } catch (e) {
      toast(errorText(e, "This invite link can't be used."), "error");
      setBusy(false);
    }
  };
  if (token === null || failed) {
    return (
      <div className="mx-auto max-w-md px-6 py-16 text-center" data-testid="channel-join-invalid">
        <p className="text-lg font-semibold">This invite link isn&apos;t valid</p>
        <p className="mt-1 text-sm text-muted-foreground">It may have expired or been revoked. Ask the channel&apos;s admin for a new one.</p>
        <Link href="/dashboard/updates" className="mt-4 inline-block font-semibold text-primary">Back to Updates</Link>
      </div>
    );
  }
  if (!preview) return <div className="mx-auto mt-16 h-48 max-w-md animate-pulse rounded-3xl bg-muted" />;
  return (
    <div className="mx-auto flex max-w-md flex-col items-center gap-3 px-6 py-12 text-center" data-testid="channel-join">
      <ChannelAvatar name={preview.name} avatar={preview.avatar} size="size-24" />
      <p className="flex items-center gap-1.5 text-xl font-semibold">{preview.name}{preview.verified ? <VerifiedBadge className="size-5" /> : null}</p>
      <p className="text-sm text-muted-foreground">{preview.handle ? `@${preview.handle} · ` : ""}{followersText(preview.followerCount)}</p>
      {preview.visibility === "PRIVATE" ? <p className="flex items-center gap-1 text-xs text-muted-foreground"><Lock className="size-3.5" />Private channel</p> : null}
      {preview.description ? <p className="whitespace-pre-wrap text-sm">{preview.description}</p> : null}
      {preview.following ? (
        <Link href={`/dashboard/updates/channels/${preview.channelId}`} className="mt-2 flex h-11 w-full items-center justify-center rounded-xl bg-primary font-semibold text-primary-foreground">Open channel</Link>
      ) : (
        <button type="button" disabled={busy} onClick={() => void join()} className="mt-2 h-11 w-full rounded-xl bg-primary font-semibold text-primary-foreground disabled:opacity-60" data-testid="channel-join-button">{busy ? "Joining…" : "Join channel"}</button>
      )}
    </div>
  );
}
