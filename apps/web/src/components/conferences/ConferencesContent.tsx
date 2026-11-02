"use client";

import { useState } from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useQueries } from "@tanstack/react-query";
import { useSpaces } from "@/hooks/v2/useSpaces";
import { conferenceListQueryOptions } from "@/hooks/v2/useConferences";
import { conferencePermission } from "@/lib/meeting-scheduling";
import { Button } from "@/components/ui/button";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import ConferencesPanel, { ConferenceRow } from "./ConferencesPanel";

export default function ConferencesContent() {
  const search = useSearchParams();
  const requested = search.get("space") || "";
  const [selection, setSelection] = useState<string | null>(null);
  const selected = selection ?? requested;
  const spaces = useSpaces();
  const items = spaces.data?.items || [];
  const space = items.find(item => item.id === selected);
  // The backend lists conferences per member space; preserve each query's own cache and errors.
  const queries = useQueries({
    queries: items.map(item => conferenceListQueryOptions(item.id, undefined, !selected)),
  });
  const rows = queries.flatMap((query, index) => (query.data?.items || []).map(conference => ({ conference, space: items[index] })));
  rows.sort((a, b) => (b.conference.scheduledStartAt || b.conference.startedAt || "").localeCompare(a.conference.scheduledStartAt || a.conference.startedAt || ""));
  return <div className="space-y-6 px-4 py-6 sm:p-8 md:p-10"><header><h1 className="text-2xl font-bold">Conferences</h1><p className="mt-1 text-sm text-muted-foreground">Scheduled, live and past sessions. Each conference has its own participants, admission and lifecycle.</p></header>
    {spaces.isLoading && <p role="status">Loading your conferences…</p>}
    {spaces.isError && <RequestError error={spaces.error} retry={() => void spaces.refetch()} />}
    {spaces.isSuccess && <><label className="grid max-w-lg gap-2 text-sm font-medium">Meeting space<select className="h-10 rounded-md border border-input bg-background px-3" value={selected} onChange={event => setSelection(event.target.value)}><option value="">All meeting spaces</option>{items.map(item => <option key={item.id} value={item.id}>{item.title || item.meetingCode}{item.status === "ARCHIVED" ? " (archived)" : ""}</option>)}</select></label>
      <p className="text-sm text-muted-foreground">Choose a meeting space to schedule a conference or filter its session history.</p>
      {selected ? space ? <ConferencesPanel key={space.id} space={space} /> : <p role="alert" className="text-sm text-destructive">This meeting space is unavailable or you no longer belong to it.</p> : <>
        {queries.some(query => query.isLoading) && <p role="status">Loading conference sessions…</p>}
        {queries.map((query, index) => query.isError ? <div key={items[index].id}><p className="text-sm font-medium">{items[index].title || items[index].meetingCode}</p><RequestError error={query.error} retry={() => void query.refetch()} /></div> : null)}
        {rows.map(({ conference, space }) => <div key={conference.id} className="space-y-2"><Link className="text-sm text-primary hover:underline" href={`/dashboard/meetings/${space.id}`}>{space.title || space.meetingCode}</Link><ConferenceRow conference={conference} space={space} canManage={conferencePermission(space, "start")} /></div>)}
        {!queries.some(query => query.isLoading || query.isError) && rows.length === 0 && <div className="rounded-xl border border-dashed border-border p-8 text-center"><p className="text-muted-foreground">No conference sessions yet.</p>{items.length === 0 ? <Button className="mt-4" asChild><Link href="/dashboard/meetings/new">Create a meeting space</Link></Button> : <p className="mt-2 text-sm text-muted-foreground">Select a meeting space above to schedule its first conference.</p>}</div>}
      </>}
    </>}
  </div>;
}
