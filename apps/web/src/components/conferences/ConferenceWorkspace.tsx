"use client";

import Link from "next/link";
import { useConference } from "@/hooks/v2/useConferences";
import { useSpace } from "@/hooks/v2/useSpaces";
import { conferencePermission } from "@/lib/meeting-scheduling";
import { Button } from "@/components/ui/button";
import { ConferenceRow } from "./ConferencesPanel";
import ParticipantsPanel from "./ParticipantsPanel";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";

export default function ConferenceWorkspace({ id }: { id: string }) {
  const conference = useConference(id);
  const space = useSpace(conference.data?.meetingSpaceId || "");
  return <div className="space-y-6 px-4 py-6 sm:p-8 md:p-10"><div className="flex flex-wrap gap-2"><Button variant="outline" size="sm" asChild><Link href="/dashboard/conferences">Back to conferences</Link></Button>{conference.data && <Button variant="outline" size="sm" asChild><Link href={`/dashboard/meetings/${conference.data.meetingSpaceId}`}>Meeting settings &amp; members</Link></Button>}</div><h1 className="text-2xl font-bold">Conference details</h1>
    {conference.isLoading && <p role="status">Loading conference…</p>}
    {conference.isError && <RequestError error={conference.error} retry={() => void conference.refetch()} />}
    {space.isError && <RequestError error={space.error} retry={() => void space.refetch()} />}
    {conference.data && <>{space.data ? <ConferenceRow conference={conference.data} space={space.data} canManage={conferencePermission(space.data, "start")} /> : <p>{conference.data.status}</p>}
      {conference.data.view === "FULL" ? <ParticipantsPanel conferenceId={id} spaceRole={space.data?.viewerRole} policy={space.data?.rolePolicy} /> : <p className="text-muted-foreground">You can view the conference summary. Participant and moderation access requires conference authority.</p>}
    </>}
  </div>;
}
