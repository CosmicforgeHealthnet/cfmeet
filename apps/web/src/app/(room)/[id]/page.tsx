"use client";

import { useParams } from "next/navigation";
import MeetingLinkResolver from "@/components/room/MeetingLinkResolver";

/** `/{meetingCode}`: resolves the share link to the conference-keyed room (FE-017). */
export default function MeetingLinkPage() {
  const params = useParams();
  const id = params?.id as string;
  if (!id) return null;
  return <MeetingLinkResolver identifier={decodeURIComponent(id)} />;
}
