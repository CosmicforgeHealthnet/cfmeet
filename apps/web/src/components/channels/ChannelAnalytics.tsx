"use client";

import { useState } from "react";
import { compactCount, percentText, rangeFor, sparkPath } from "@/lib/channels";
import { useChannelAnalytics } from "@/hooks/v2/useChannels";
import type { AnalyticsDay } from "@/services/v2/channels.service";

const PRESETS = [7, 30, 90] as const;

/** One small series chart (recorded counts per UTC day). */
function Series({ title, days, pick, total }: { title: string; days: AnalyticsDay[]; pick: (d: AnalyticsDay) => number | null; total?: string }) {
  const values = days.map(pick);
  const d = sparkPath(values, 300, 64);
  return (
    <figure className="rounded-2xl border border-border p-3">
      <figcaption className="flex items-baseline justify-between text-sm"><span className="font-medium">{title}</span>{total ? <span className="text-muted-foreground">{total}</span> : null}</figcaption>
      <svg viewBox="0 0 300 64" className="mt-2 h-16 w-full" role="img" aria-label={`${title} per day`} preserveAspectRatio="none">
        {d ? <path d={d} fill="none" stroke="currentColor" strokeWidth="2" className="text-primary" vectorEffect="non-scaling-stroke" /> : null}
      </svg>
      <div className="mt-1 flex justify-between text-[10px] text-muted-foreground"><span>{days[0]?.date}</span><span>{days.at(-1)?.date}</span></div>
    </figure>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-2xl border border-border p-3">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="mt-1 text-2xl font-semibold tabular-nums">{value}</p>
    </div>
  );
}

/**
 * Analytics for the owner and admins: counts of recorded events only (follows, unfollows,
 * views — one per person and post — reactions, poll voters, forwards). Never a person.
 */
export default function ChannelAnalytics({ channelId }: { channelId: string }) {
  const [preset, setPreset] = useState<number>(30);
  const [custom, setCustom] = useState<{ from: string; to: string } | null>(null);
  const range = custom ?? rangeFor(preset, new Date());
  const a = useChannelAnalytics(channelId, range.from, range.to);
  const r = a.data;
  return (
    <section className="space-y-4 px-4 py-4" data-testid="channel-analytics">
      <div className="flex flex-wrap items-end gap-2">
        {PRESETS.map((p) => (
          <button key={p} type="button" onClick={() => { setCustom(null); setPreset(p); }} className={`h-9 rounded-full px-4 text-sm font-semibold ${!custom && preset === p ? "bg-primary text-primary-foreground" : "bg-muted"}`}>{p} days</button>
        ))}
        <label className="text-xs">From<input type="date" value={range.from} max={range.to} onChange={(e) => setCustom({ from: e.target.value, to: range.to })} className="ml-1 h-9 rounded-lg border border-border bg-background px-2" /></label>
        <label className="text-xs">To<input type="date" value={range.to} min={range.from} onChange={(e) => setCustom({ from: range.from, to: e.target.value })} className="ml-1 h-9 rounded-lg border border-border bg-background px-2" /></label>
      </div>
      {a.isError ? <p className="rounded-xl bg-destructive/10 px-3 py-2 text-sm text-destructive">Analytics couldn&apos;t be loaded for this range.</p> : null}
      {!r ? <div className="h-48 animate-pulse rounded-2xl bg-muted" /> : (
        <>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
            <Stat label="Followers" value={compactCount(r.followers.current)} />
            <Stat label="Net growth" value={`${r.followers.net >= 0 ? "+" : ""}${r.followers.net}`} />
            <Stat label="Post views" value={compactCount(r.posts.views)} />
            <Stat label="Unique viewers" value={compactCount(r.posts.uniqueViewers)} />
            <Stat label="Reactions" value={compactCount(r.posts.reactions)} />
            <Stat label="Poll voters" value={compactCount(r.posts.pollVoters)} />
            <Stat label="Shares to chats" value={compactCount(r.posts.forwards)} />
            <Stat label="Engagement rate" value={percentText(r.engagementRate)} />
          </div>
          <div className="grid gap-3 sm:grid-cols-2">
            <Series title="Followers" days={r.series} pick={(d) => d.followers} />
            <Series title="New follows" days={r.series} pick={(d) => d.follows} total={`${r.followers.follows} total`} />
            <Series title="Unfollows" days={r.series} pick={(d) => d.unfollows} total={`${r.followers.unfollows} total`} />
            <Series title="Views" days={r.series} pick={(d) => d.views} total={`${r.posts.views} total`} />
          </div>
          <div className="rounded-2xl border border-border">
            <p className="border-b border-border px-3 py-2 text-sm font-medium">Top posts</p>
            {r.topPosts.length === 0 ? <p className="px-3 py-4 text-sm text-muted-foreground">No views, reactions or votes in this range yet.</p> : (
              <ol className="divide-y divide-border text-sm">
                {r.topPosts.map((p) => (
                  <li key={p.postId} className="flex items-center gap-3 px-3 py-2.5">
                    <span className="min-w-0 flex-1 truncate">{p.excerpt ?? p.kind.toLowerCase()}</span>
                    <span className="shrink-0 text-xs tabular-nums text-muted-foreground">{p.views} views · {p.reactions} reactions{p.pollVoters ? ` · ${p.pollVoters} votes` : ""}</span>
                  </li>
                ))}
              </ol>
            )}
          </div>
          <p className="text-xs text-muted-foreground">Counts per UTC day. Follower history starts {new Date(r.statsSince).toLocaleDateString()}. A person&apos;s repeat views are counted once; your own and other admins&apos; views aren&apos;t counted.</p>
        </>
      )}
    </section>
  );
}
