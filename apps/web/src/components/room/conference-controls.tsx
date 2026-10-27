"use client";

import { useRoomContext } from "@livekit/components-react";
import { useEffect, useState } from "react";
import { RefreshCw } from "lucide-react";
import { useConference, useEndConference, useLockConference, useUnlockConference } from "@/hooks/v2/useConferences";
import { ActionDialog, RequestError } from "@/components/meeting-spaces/ManagementUI";
import type { Role } from "@/types/v2";
import { useCurrentParticipant } from "@/hooks/v2/useParticipants";
import { useSpace } from "@/hooks/v2/useSpaces";
import { useMediaCredential } from "@/hooks/v2/useCollaboration";
import { effectiveConferenceRole, meetingAllows } from "@/lib/meeting-authority";
import CollaborationPolicyControls from "@/components/conferences/CollaborationPolicyControls";
import RoomEngagementControls from "./RoomEngagementControls";
import { Button } from "@/components/ui/button";

export default function ConferenceControls({ id, role, sessionId, onCredentialRefreshStateChange }: { id: string; role?: Role; sessionId?: string; onCredentialRefreshStateChange: (refreshing: boolean) => void }) {
  const query = useConference(id);
  const end = useEndConference(), lock = useLockConference(), unlock = useUnlockConference();
  const refreshCredential = useMediaCredential(id);
  const room = useRoomContext();
  const [credentialError, setCredentialError] = useState<unknown>();
  const c = query.data;
  const me = useCurrentParticipant(id);
  const space = useSpace(c?.meetingSpaceId || "");
  const currentRole = effectiveConferenceRole(space.data?.viewerRole || role, me.data?.role, me.data?.admissionState);
  const canLock = meetingAllows(currentRole, "conference.lock", space.data?.rolePolicy);
  const canEnd = meetingAllows(currentRole, "conference.end", space.data?.rolePolicy);
  const refresh = async () => {
    setCredentialError(undefined);
    try {
      if (!sessionId) throw new Error("This session cannot refresh its media credential. Leave and rejoin the conference.");
      const result = await refreshCredential.mutateAsync({ participantSessionId: sessionId });
      onCredentialRefreshStateChange(true);
      await room.disconnect(false);
      await room.connect(result.rtc.url, result.rtc.credential);
    } catch (error) {
      setCredentialError(error);
    } finally {
      onCredentialRefreshStateChange(false);
    }
  };
  useEffect(() => {
    if ((c && ["ENDED", "CANCELLED", "FAILED"].includes(c.status)) || (me.data && ["REMOVED", "BANNED", "DENIED"].includes(me.data.admissionState))) void room.disconnect();
  }, [c, me.data, room]);
  if (!c) return null;
  return (
    <>
      <div className="flex flex-wrap items-center justify-center gap-2 border-b border-border bg-card px-4 py-2 text-sm">
        <span>{c.status}{c.isLocked ? " · Locked" : ""}</span>
        {c.status === "ENDING" && <span className="text-muted-foreground">Admission stopped. Waiting for provider termination.</span>}
        {c.status === "LIVE" && <>
          {canLock && <ActionDialog label={c.isLocked ? "Unlock" : "Lock"} description="Change admission for new non-exempt entrants. Existing sessions stay connected." submit={() => (c.isLocked ? unlock : lock).mutateAsync(id)} />}
          {canEnd && <ActionDialog label="End for everyone" destructive description="Stop new admission and request provider termination for all participants." submit={() => end.mutateAsync(id)} />}
          <Button size="sm" variant="outline" disabled={refreshCredential.isPending || !sessionId} onClick={() => void refresh()}><RefreshCw className="mr-2 size-4" />{refreshCredential.isPending ? "Refreshing…" : "Refresh media access"}</Button>
          <CollaborationPolicyControls conferenceId={id} role={currentRole} rolePolicy={space.data?.rolePolicy} />
        </>}
      </div>
      {credentialError !== undefined && <div className="bg-card px-3 py-2"><RequestError error={credentialError} /></div>}
      {c.status === "LIVE" && <RoomEngagementControls conferenceId={id} />}
    </>
  );
}
