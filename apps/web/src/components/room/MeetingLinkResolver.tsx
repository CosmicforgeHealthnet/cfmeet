"use client";

/**
 * `/{meetingCode}`: the meeting code is a navigational entry identifier only (FE-017). It is
 * resolved (`GET /api/v2/resolve/{identifier}`) to the space's canonical conference and the
 * browser moves to the conference-keyed room `/conferences/{conferenceId}`. An explicit
 * `?conference=` (older links) is honoured only when it belongs to the resolved space.
 */
import { useEffect } from "react";
import { useRouter, useSearchParams } from "next/navigation";
import { useConference } from "@/hooks/v2/useConferences";
import { useResolveSpace } from "@/hooks/v2/useSpaces";
import { conferenceRoomPath } from "@/lib/prejoin";
import { Spinner } from "@/components/ui/spinner";
import { RoomMessage } from "./pre-join-screen";

export default function MeetingLinkResolver({ identifier }: { identifier: string }) {
  const router = useRouter();
  const search = useSearchParams();
  const requested = search.get("conference") || "";
  const resolved = useResolveSpace(identifier);
  const selected = useConference(requested);
  const spaceId = resolved.data?.meetingSpace.id;
  const target = requested
    ? selected.data && selected.data.meetingSpaceId === spaceId ? selected.data.id : null
    : resolved.data?.conference?.id ?? null;
  const selectionInvalid = !!requested && (selected.isError || (!!selected.data && !!spaceId && selected.data.meetingSpaceId !== spaceId));

  useEffect(() => {
    if (target) router.replace(conferenceRoomPath(target));
  }, [target, router]);

  if (resolved.isError || selectionInvalid) {
    return <RoomMessage icon="missing" title="Meeting Not Found">The meeting <span className="font-medium text-foreground">{identifier}</span> doesn&apos;t exist or you don&apos;t have access to it.</RoomMessage>;
  }
  if (resolved.data && !resolved.data.conference && !requested) {
    return <RoomMessage icon="ended" title="Nothing to join yet">{resolved.data.meetingSpace.title ? <>&quot;{resolved.data.meetingSpace.title}&quot; has no occurrence to join right now.</> : <>This meeting has no occurrence to join right now.</>} Ask the organizer for the meeting time.</RoomMessage>;
  }
  return <div className="flex min-h-screen items-center justify-center bg-background"><Spinner label="Opening meeting..." /></div>;
}
