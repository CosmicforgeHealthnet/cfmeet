"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { ArrowLeft, Plus, Search, Star, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { toast } from "@/components/ui/toast";
import { CATEGORIES, COUNTRIES, followersText, LANGUAGES } from "@/lib/channels";
import { useChannelSuggestions, useDiscoverChannels, useFollowChannel } from "@/hooks/v2/useChannels";
import type { Channel, Sort } from "@/services/v2/channels.service";
import { errorText } from "@/components/updates/shared";
import { ChannelAvatar, VerifiedBadge } from "./bits";

const SORTS: [Sort, string][] = [["POPULAR", "Popular"], ["TRENDING", "Trending"], ["NEW", "New"], ["VERIFIED", "Verified"]];
const name = (type: "language" | "region", code: string) => {
  try { return new Intl.DisplayNames([typeof navigator === "undefined" ? "en" : navigator.language || "en"], { type }).of(code) ?? code; } catch { return code; }
};

function useDebounced<T>(value: T, ms = 300): T {
  const [v, setV] = useState(value);
  useEffect(() => { const t = setTimeout(() => setV(value), ms); return () => clearTimeout(t); }, [value, ms]);
  return v;
}

function Row({ c }: { c: Channel }) {
  const follow = useFollowChannel();
  const [following, setFollowing] = useState(c.following);
  return (
    <div className="flex items-center gap-3 px-4 py-3" data-testid="discover-channel">
      <Link href={`/dashboard/updates/channels/${c.channelId}`} className="flex min-w-0 flex-1 items-center gap-3">
        <ChannelAvatar name={c.name} avatar={c.avatar} size="size-12" />
        <span className="min-w-0">
          <span className="flex items-center gap-1 font-semibold"><span className="truncate">{c.name}</span>{c.verified ? <VerifiedBadge /> : null}</span>
          <span className="block truncate text-xs text-muted-foreground">{[c.handle ? `@${c.handle}` : null, followersText(c.followerCount), CATEGORIES.find((x) => x.code === c.category)?.label].filter(Boolean).join(" · ")}</span>
          {c.description ? <span className="line-clamp-1 block text-xs text-muted-foreground">{c.description}</span> : null}
        </span>
      </Link>
      {c.isOwner ? <span className="text-xs text-muted-foreground">Yours</span> : (
        <button type="button" disabled={follow.isPending} onClick={() => follow.mutate({ id: c.channelId, on: !following }, { onSuccess: () => setFollowing(!following), onError: (e) => toast(errorText(e), "error") })}
          className={cn("h-9 shrink-0 rounded-full px-4 text-sm font-semibold disabled:opacity-60", following ? "bg-muted" : "bg-primary text-primary-foreground")}>
          {following ? "Following" : "Follow"}
        </button>
      )}
    </div>
  );
}

/** Discover channels: search with suggestions, filters, popular / trending / new / verified, paged. */
export default function DiscoverChannels() {
  const [q, setQ] = useState("");
  const [focus, setFocus] = useState(false);
  const [sort, setSort] = useState<Sort>("POPULAR");
  const [category, setCategory] = useState("");
  const [country, setCountry] = useState("");
  const [language, setLanguage] = useState("");
  const dq = useDebounced(q.trim());
  const list = useDiscoverChannels({ q: dq || undefined, sort, category: category || undefined, country: country || undefined, language: language || undefined, limit: 30 });
  const suggest = useChannelSuggestions(dq);
  const channels = list.data?.pages.flatMap((p) => p.channels) ?? [];
  return (
    <div className="min-h-dvh" data-testid="discover-channels">
      <header className="sticky top-0 z-10 space-y-2 border-b border-border bg-background px-2 py-2">
        <div className="flex items-center gap-2">
          <Link href="/dashboard/updates" aria-label="Back" className="flex size-10 items-center justify-center rounded-full hover:bg-muted"><ArrowLeft className="size-5" /></Link>
          <h1 className="flex-1 text-lg font-semibold">Find channels</h1>
          <Link href="/dashboard/updates/channels/starred" aria-label="Starred posts" className="flex size-10 items-center justify-center rounded-full hover:bg-muted"><Star className="size-5" /></Link>
          <Link href="/dashboard/updates/channels/new" className="flex h-9 items-center gap-1 rounded-full bg-primary px-3 text-sm font-semibold text-primary-foreground" data-testid="create-channel"><Plus className="size-4" />Create</Link>
        </div>
        <div className="relative px-2">
          <Search className="pointer-events-none absolute left-5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <input value={q} onChange={(e) => setQ(e.target.value)} onFocus={() => setFocus(true)} onBlur={() => setTimeout(() => setFocus(false), 150)} maxLength={64}
            placeholder="Search by name, @username or keyword" className="h-11 w-full rounded-full bg-muted pl-10 pr-10 text-[15px] focus:outline-none focus:ring-2 focus:ring-primary/40" data-testid="discover-search" />
          {q ? <button type="button" aria-label="Clear" onClick={() => setQ("")} className="absolute right-4 top-1/2 flex size-7 -translate-y-1/2 items-center justify-center rounded-full hover:bg-background"><X className="size-4" /></button> : null}
          {focus && dq && suggest.data?.length ? (
            <div className="absolute inset-x-2 top-12 z-20 rounded-2xl border border-border bg-popover p-1 shadow-xl" data-testid="discover-suggestions">
              {suggest.data.map((s) => (
                <Link key={s.channelId} href={`/dashboard/updates/channels/${s.channelId}`} className="flex items-center gap-3 rounded-xl px-3 py-2 hover:bg-muted">
                  <ChannelAvatar name={s.name} avatar={s.avatar} size="size-8" />
                  <span className="min-w-0 flex-1"><span className="flex items-center gap-1 text-sm font-medium"><span className="truncate">{s.name}</span>{s.verified ? <VerifiedBadge /> : null}</span>
                    <span className="block text-xs text-muted-foreground">{s.handle ? `@${s.handle} · ` : ""}{followersText(s.followerCount)}</span></span>
                </Link>
              ))}
            </div>
          ) : null}
        </div>
        <div className="flex gap-1 overflow-x-auto px-2">
          {SORTS.map(([s, label]) => (
            <button key={s} type="button" onClick={() => setSort(s)} className={cn("h-8 shrink-0 rounded-full px-3 text-sm font-semibold", sort === s ? "bg-primary text-primary-foreground" : "bg-muted")}>{label}</button>
          ))}
          <select aria-label="Category" value={category} onChange={(e) => setCategory(e.target.value)} className="h-8 shrink-0 rounded-full bg-muted px-3 text-sm">
            <option value="">All categories</option>
            {CATEGORIES.map((c) => <option key={c.code} value={c.code}>{c.label}</option>)}
          </select>
          <select aria-label="Country" value={country} onChange={(e) => setCountry(e.target.value)} className="h-8 shrink-0 rounded-full bg-muted px-3 text-sm">
            <option value="">All countries</option>
            {COUNTRIES.map((c) => <option key={c} value={c}>{name("region", c)}</option>)}
          </select>
          <select aria-label="Language" value={language} onChange={(e) => setLanguage(e.target.value)} className="h-8 shrink-0 rounded-full bg-muted px-3 text-sm">
            <option value="">All languages</option>
            {LANGUAGES.map((l) => <option key={l} value={l}>{name("language", l)}</option>)}
          </select>
        </div>
      </header>
      <div className="mx-auto max-w-2xl divide-y divide-border">
        {list.isPending ? [0, 1, 2].map((i) => <div key={i} className="mx-4 my-3 h-14 animate-pulse rounded-2xl bg-muted" />) : null}
        {list.isError ? <p className="px-6 py-12 text-center text-sm text-muted-foreground">{errorText(list.error, "Channels couldn't be loaded.")}</p> : null}
        {!list.isPending && !list.isError && channels.length === 0 ? (
          <p className="px-6 py-12 text-center text-sm text-muted-foreground">{sort === "TRENDING" ? "Nothing is trending yet." : sort === "VERIFIED" ? "No verified channels match yet." : "No channels match. Try other words or filters."}</p>
        ) : null}
        {channels.map((c) => <Row key={c.channelId} c={c} />)}
      </div>
      {list.hasNextPage ? (
        <div className="py-4 text-center"><button type="button" disabled={list.isFetchingNextPage} onClick={() => void list.fetchNextPage()} className="h-9 rounded-full px-4 text-sm font-semibold text-primary hover:bg-primary/10">{list.isFetchingNextPage ? "Loading…" : "Show more"}</button></div>
      ) : null}
    </div>
  );
}
