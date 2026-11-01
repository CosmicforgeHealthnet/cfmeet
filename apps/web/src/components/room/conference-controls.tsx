"use client";

import { useRoomContext } from "@livekit/components-react";
import { useEffect, useState } from "react";
import { MoreHorizontal, RefreshCw } from "lucide-react";
import { useConference, useEndConference, useLockConference, useUnlockConference } from "@/hooks/v2/useConferences";
import { ActionDialog, RequestError } from "@/components/meeting-spaces/ManagementUI";
import type { Role } from "@/types/v2";
import { useCurrentParticipant } from "@/hooks/v2/useParticipants";
import { useSpace } from "@/hooks/v2/useSpaces";
import { useMediaCredential } from "@/hooks/v2/useCollaboration";
import { effectiveConferenceRole, meetingAllows } from "@/lib/meeting-authority";
import CollaborationPolicyControls from "@/components/conferences/CollaborationPolicyControls";
import type { ReactNode } from "react";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogTrigger } from "@/components/ui/dialog";
import RoomQualityPanel from "./RoomQualityPanel";
import { profilesService } from "@/services/v2/profiles.service";
import type { MediaContextView } from "@/types/v2";
import { Button } from "@/components/ui/button";

export default function ConferenceControls({ id, role, sessionId, initialMediaContext, onCredentialRefreshStateChange, tools }: { tools?: ReactNode; id: string; role?: Role; sessionId?: string; initialMediaContext?: MediaContextView; onCredentialRefreshStateChange: (refreshing: boolean) => void }) {
  const query = useConference(id);
  const end = useEndConference(), lock = useLockConference(), unlock = useUnlockConference();
  const refreshCredential = useMediaCredential(id);
  const room = useRoomContext();
  const [credentialError, setCredentialError] = useState<unknown>();
  const [backstageError, setBackstageError] = useState<unknown>();
  const [backstagePending, setBackstagePending] = useState(false);
  const [mediaContext, setMediaContext] = useState(initialMediaContext);
  const c = query.data;
  const me = useCurrentParticipant(id);
  const space = useSpace(c?.meetingSpaceId || "");
  const currentRole = effectiveConferenceRole(space.data?.viewerRole || role, me.data?.role, me.data?.admissionState);
  const canLock = meetingAllows(currentRole, "conference.lock", space.data?.rolePolicy);
  const canEnd = meetingAllows(currentRole, "conference.end", space.data?.rolePolicy);
  const canEnterBackstage = meetingAllows(currentRole, "backstage.enter", space.data?.rolePolicy);
  const canViewQuality = meetingAllows(currentRole, "quality.view", space.data?.rolePolicy);
  const isWebinar = space.data?.meetingType === "WEBINAR" || space.data?.meetingType === "TOWN_HALL";
  const inBackstage = mediaContext?.kind === "BACKSTAGE";
  const refresh = async () => {
    setCredentialError(undefined);
    try {
      if (!sessionId) throw new Error("This session cannot refresh its media credential. Leave and rejoin the conference.");
      const result = await refreshCredential.mutateAsync({ participantSessionId: sessionId });
      onCredentialRefreshStateChange(true);
      await room.disconnect(false);
      await room.connect(result.rtc.url, result.rtc.credential);
      setMediaContext(result.mediaContext);
    } catch (error) {
      setCredentialError(error);
    } finally {
      refreshCredential.reset();
      onCredentialRefreshStateChange(false);
    }
  };
  const changeBackstage = async () => {
    setBackstageError(undefined);
    setBackstagePending(true);
    try {
      const result = inBackstage
        ? await profilesService.leaveBackstage(id)
        : await profilesService.enterBackstage(id);
      setMediaContext(result.mediaContext);
      if (result.credentialRefreshRequired) {
        if (!sessionId) throw new Error("This session cannot refresh its media credential. Leave and rejoin the conference.");
        const credential = await refreshCredential.mutateAsync({ participantSessionId: sessionId });
        onCredentialRefreshStateChange(true);
        await room.disconnect(false);
        await room.connect(credential.rtc.url, credential.rtc.credential);
        setMediaContext(credential.mediaContext);
      }
    } catch (error) {
      setBackstageError(error);
    } finally {
      refreshCredential.reset();
      onCredentialRefreshStateChange(false);
      setBackstagePending(false);
    }
  };
  useEffect(() => {
    if ((c && ["ENDED", "CANCELLED", "FAILED"].includes(c?.status)) || (me.data && ["REMOVED", "BANNED", "DENIED"].includes(me.data.admissionState))) void room.disconnect();
  }, [c, me.data, room]);

  return (
    <Dialog><div className="room-control"><DialogTrigger asChild><button aria-label="More room tools" className="room-control-button"><MoreHorizontal /></button></DialogTrigger><span>More</span></div><DialogContent className="max-h-[85dvh] overflow-y-auto sm:max-w-2xl"><DialogHeader><DialogTitle>Room tools</DialogTitle><DialogDescription>Meeting controls and collaboration settings.</DialogDescription></DialogHeader>{tools}
      <div className="flex flex-wrap items-center justify-center gap-2 border-b border-border bg-card px-4 py-2 text-sm">
        <span>{c?.status}{c?.isLocked ? " · Locked" : ""}</span>
        {c?.status === "ENDING" && <span className="text-muted-foreground">Admission stopped. Waiting for provider termination.</span>}
        {c?.status === "LIVE" && <>
          {canLock && <ActionDialog label={c?.isLocked ? "Unlock" : "Lock"} description="Change admission for new non-exempt entrants. Existing sessions stay connected." submit={() => (c?.isLocked ? unlock : lock).mutateAsync(id)} />}
          {canEnd && <ActionDialog label="End for everyone" destructive description="Stop new admission and request provider termination for all participants." submit={() => end.mutateAsync(id)} />}
          <Button size="sm" variant="outline" disabled={refreshCredential.isPending || !sessionId} onClick={() => void refresh()}><RefreshCw className="mr-2 size-4" />{refreshCredential.isPending ? "Refreshing…" : "Refresh media access"}</Button>
          {isWebinar && canEnterBackstage && <Button size="sm" variant="outline" disabled={backstagePending || refreshCredential.isPending} onClick={() => void changeBackstage()}>{backstagePending ? "Changing room…" : inBackstage ? "Return to main stage" : "Enter backstage"}</Button>}
          <CollaborationPolicyControls conferenceId={id} role={currentRole} rolePolicy={space.data?.rolePolicy} />
        </>}
      </div>
      {credentialError !== undefined && <div className="bg-card px-3 py-2"><RequestError error={credentialError} /></div>}
      {backstageError !== undefined && <div className="bg-card px-3 py-2"><RequestError error={backstageError} /></div>}

      {c?.status === "LIVE" && <RoomQualityPanel conferenceId={id} sessionId={sessionId} canViewSummary={canViewQuality} />}
    </DialogContent></Dialog>
  );
}
