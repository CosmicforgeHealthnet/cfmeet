"use client";

import Link from "next/link";
import { useSpace } from "@/hooks/v2/useSpaces";
import { Button } from "@/components/ui/button";
import MeetingSpaceActions from "@/components/dashboard/MeetingSpaceActions";
import RecurrencePanel from "./RecurrencePanel";
import MembersPanel from "./MembersPanel";
import InvitationsPanel from "./InvitationsPanel";
import RegistrationPanel from "./RegistrationPanel";
import ConferencesPanel from "@/components/conferences/ConferencesPanel";
import { RequestError } from "./ManagementUI";
import SpaceSchedule from "@/components/meetings/SpaceSchedule";
import { useCalendarDownload } from "@/hooks/v2/useCalendar";

export default function MeetingSpaceWorkspace({ id }: { id: string }) {
  const query = useSpace(id);
  const space = query.data;
  const download = useCalendarDownload();
  return <div className="space-y-8 px-4 py-6 sm:p-8 md:p-10">
    <Button variant="outline" size="sm" asChild><Link href="/dashboard/meetings">Back to meeting spaces</Link></Button>
    {query.isLoading && <p role="status">Loading meeting space…</p>}
    {query.isError && <RequestError error={query.error} retry={() => void query.refetch()} />}
    {space && <><header className="flex flex-wrap items-center justify-between gap-4"><div><h1 className="text-2xl font-bold">{space.title || "Meeting space"}</h1><p className="text-sm text-muted-foreground">{space.meetingCode} · {space.timezone || "UTC"} · {space.status}{space.organizationId ? " · Organization meeting" : ""}</p><div className="mt-1 text-sm"><SpaceSchedule space={space} /></div></div><div className="flex flex-wrap items-center gap-2">{space.viewerRole ? <Button variant="outline" size="sm" disabled={download.isPending} onClick={() => download.mutate({ kind: "space", id: space.id, name: space.title || space.meetingCode })}>Add schedule to calendar (.ics)</Button> : null}<MeetingSpaceActions space={space} /></div></header>
      {download.isError ? <RequestError error={download.error} /> : null}
      <p className="text-sm text-muted-foreground">Manage this meeting space, its schedule, and the people who belong to it.</p>
      {space.viewerRole ? <><ConferencesPanel space={space} /><div id="recurrence" className="scroll-mt-20"><RecurrencePanel space={space} /></div><InvitationsPanel space={space} /><RegistrationPanel space={space} />{["OWNER", "HOST", "CO_HOST"].includes(space.viewerRole) && <><div className="border-t border-border" /><MembersPanel space={space} /></>}</> : <p className="rounded-xl border border-border p-6 text-muted-foreground">This is a meeting summary. A link or code does not grant membership or access to its schedules and members.</p>}
    </>}
  </div>;
}
