"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Bell, BellOff, Copy, Flag, Info, LogOut, MoreVertical, Settings, Star, Trash2 } from "lucide-react";
import { CHANNEL_FEATURES, channelMenu } from "@/lib/section-menus";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import QrCode from "@/components/communications/QrCode";
import { CATEGORIES, channelLink, followersText } from "@/lib/channels";
import { channelsService, type Channel } from "@/services/v2/channels.service";
import { errorText } from "@/components/updates/shared";
import ReportDialog from "./ReportDialog";
import { ChannelAvatar, Sheet, VerifiedBadge } from "./bits";

const language = (code: string | null) => {
  if (!code) return null;
  try { return new Intl.DisplayNames([navigator.language || "en"], { type: "language" }).of(code) ?? code; } catch { return code; }
};
const country = (code: string | null) => {
  if (!code) return null;
  try { return new Intl.DisplayNames([navigator.language || "en"], { type: "region" }).of(code) ?? code; } catch { return code; }
};

/** The channel's info: details, its share link and QR code. */
export function ChannelInfoSheet({ channel, onClose }: { channel: Channel; onClose: () => void }) {
  const link = typeof window === "undefined" ? "" : channelLink(window.location.origin, channel.channelId);
  const copy = async () => {
    try { await navigator.clipboard.writeText(link); toast("Link copied"); } catch { toast("Couldn't copy the link", "error"); }
  };
  const rows: [string, string | null][] = [
    ["Username", channel.handle ? `@${channel.handle}` : null],
    ["Category", CATEGORIES.find((c) => c.code === channel.category)?.label ?? null],
    ["Language", language(channel.language)],
    ["Country", country(channel.country)],
    ["Visibility", channel.visibility === "PRIVATE" ? "Private — members join with an invite link" : "Public — anyone can find and follow it"],
    ["Created", new Date(channel.createdAt).toLocaleDateString()],
  ];
  return (
    <Sheet title="Channel info" onClose={onClose}>
      <div className="flex flex-col items-center gap-2 pb-4 text-center">
        <ChannelAvatar name={channel.name} avatar={channel.avatar} size="size-20" />
        <p className="flex items-center gap-1.5 text-xl font-semibold">{channel.name}{channel.verified ? <VerifiedBadge className="size-5" /> : null}</p>
        <p className="text-sm text-muted-foreground">{followersText(channel.followerCount)}</p>
        {channel.description ? <p className="whitespace-pre-wrap text-sm">{channel.description}</p> : null}
      </div>
      <dl className="divide-y divide-border rounded-2xl border border-border">
        {rows.filter(([, v]) => v).map(([k, v]) => (
          <div key={k} className="flex gap-3 px-3 py-2.5 text-sm"><dt className="w-24 shrink-0 text-muted-foreground">{k}</dt><dd className="min-w-0 flex-1">{v}</dd></div>
        ))}
      </dl>
      {channel.visibility === "PUBLIC" && link ? (
        <div className="mt-4 flex flex-col items-center gap-3">
          <QrCode value={link} size={176} label={`QR code for ${channel.name}`} />
          <button type="button" onClick={() => void copy()} className="flex h-10 items-center gap-2 rounded-full bg-muted px-4 text-sm font-semibold"><Copy className="size-4" />Copy channel link</button>
        </div>
      ) : null}
    </Sheet>
  );
}

/**
 * The channel menu (ADR 0074 section menus over the ADR 0072 screens): info, notifications,
 * share, report, unfollow, management and delete — only what this person may do here.
 */
export default function ChannelMenu({ channel, onChanged }: { channel: Channel; onChanged: () => void }) {
  const router = useRouter();
  const [open, setOpen] = useState(false);
  const [sheet, setSheet] = useState<"info" | "report" | null>(null);
  const owner = channel.myRole === "OWNER";
  const items = new Set(channelMenu({
    ...CHANNEL_FEATURES,
    isOwner: owner,
    admin: channel.myRole === "ADMIN",
    following: channel.following,
    share: true,
  }).map((i) => i.key));
  const remove = async () => {
    setOpen(false);
    if (!window.confirm(`Delete “${channel.name}”? Its posts are removed for every follower.`)) return;
    try {
      await channelsService.remove(channel.channelId);
      toast("Channel deleted");
      router.push("/dashboard/updates");
    } catch (e) {
      toast(errorText(e), "error");
    }
  };
  const run = async (fn: () => Promise<unknown>, ok?: string) => {
    setOpen(false);
    try { await fn(); if (ok) toast(ok); onChanged(); } catch (e) { toast(errorText(e), "error"); }
  };
  const share = async () => {
    setOpen(false);
    const link = channelLink(window.location.origin, channel.channelId);
    try {
      if (navigator.share) await navigator.share({ title: channel.name, url: link });
      else { await navigator.clipboard.writeText(link); toast("Link copied"); }
    } catch {
      // Cancelled by the person.
    }
  };
  const unfollow = async () => {
    setOpen(false);
    if (!window.confirm(channel.visibility === "PRIVATE" ? `Leave “${channel.name}”? You'll need a new invite link to come back.` : `Unfollow “${channel.name}”?`)) return;
    try {
      await channelsService.follow(channel.channelId, false);
      onChanged();
      if (channel.visibility === "PRIVATE") router.push("/dashboard/updates");
    } catch (e) {
      toast(errorText(e), "error");
    }
  };
  return (
    <div className="relative">
      <button type="button" aria-label="Channel options" onClick={() => setOpen(!open)} className="flex size-10 items-center justify-center rounded-full hover:bg-muted" data-testid="channel-menu"><MoreVertical className="size-5" /></button>
      {open ? (
        <div className="absolute right-0 top-11 z-30 w-56 rounded-2xl border border-border bg-popover p-1 shadow-xl">
          {items.has("info") ? <Item icon={Info} label="Channel info" onClick={() => { setOpen(false); setSheet("info"); }} /> : null}
          {items.has("notifications") ? (
            <Item icon={channel.muted ? Bell : BellOff} label={channel.muted ? "Unmute notifications" : "Mute notifications"}
              onClick={() => void run(() => channelsService.mute(channel.channelId, !channel.muted), channel.muted ? "Unmuted" : "Muted")} />
          ) : null}
          {items.has("share") ? <Item icon={Copy} label="Share channel" onClick={() => void share()} /> : null}
          <Link href="/dashboard/updates/channels/starred" className="flex w-full items-center gap-2 rounded-xl px-3 py-2.5 text-sm hover:bg-muted"><Star className="size-4" />Starred posts</Link>
          {items.has("manage") ? <Link href={`/dashboard/updates/channels/${channel.channelId}/manage`} className="flex w-full items-center gap-2 rounded-xl px-3 py-2.5 text-sm hover:bg-muted" data-testid="channel-manage"><Settings className="size-4" />Admin tools</Link> : null}
          {items.has("report") ? <Item icon={Flag} label="Report channel" danger onClick={() => { setOpen(false); setSheet("report"); }} /> : null}
          {items.has("unfollow") ? <Item icon={LogOut} label={channel.visibility === "PRIVATE" ? "Leave channel" : "Unfollow"} danger onClick={() => void unfollow()} /> : null}
          {items.has("delete") ? <Item icon={Trash2} label="Delete channel" danger onClick={() => void remove()} /> : null}
        </div>
      ) : null}
      {sheet === "info" ? <ChannelInfoSheet channel={channel} onClose={() => setSheet(null)} /> : null}
      {sheet === "report" ? <ReportDialog channelId={channel.channelId} onClose={() => setSheet(null)} /> : null}
    </div>
  );
}

function Item({ icon: Icon, label, onClick, danger }: { icon: React.ComponentType<{ className?: string }>; label: string; onClick: () => void; danger?: boolean }) {
  return (
    <button type="button" onClick={onClick} className={cn("flex w-full items-center gap-2 rounded-xl px-3 py-2.5 text-sm hover:bg-muted", danger && "text-destructive")}>
      <Icon className="size-4" />{label}
    </button>
  );
}
