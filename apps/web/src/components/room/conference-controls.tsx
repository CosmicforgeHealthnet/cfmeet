"use client";

import { useRoomContext } from "@livekit/components-react";
import { useEffect, useRef, useState } from "react";
import { MoreHorizontal, RefreshCw, Lock, LockOpen, SlidersHorizontal, Activity, Theater } from "lucide-react";
import { Popover } from "radix-ui";
import { MEDIA_MOVE_EVENT } from "./RoomLiveEvents";
import { useConference, useLockConference, useUnlockConference } from "@/hooks/v2/useConferences";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import type { Role } from "@/types/v2";
import { useCurrentParticipant } from "@/hooks/v2/useParticipants";
import { useSpace } from "@/hooks/v2/useSpaces";
import { useMediaCredential } from "@/hooks/v2/useCollaboration";
import { effectiveConferenceRole, meetingAllows } from "@/lib/meeting-authority";
import CollaborationPolicyControls from "@/components/conferences/CollaborationPolicyControls";
import type { ReactNode } from "react";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "@/components/ui/dialog";
import RoomQualityPanel from "./RoomQualityPanel";
import { profilesService } from "@/services/v2/profiles.service";
import type { MediaContextView } from "@/types/v2";

export default function ConferenceControls({ id, role, sessionId, initialMediaContext, onCredentialRefreshStateChange, tools, audioSettings }: { tools?: (close: () => void, openAudio: () => void) => ReactNode; audioSettings?: ReactNode; id: string; role?: Role; sessionId?: string; initialMediaContext?: MediaContextView; onCredentialRefreshStateChange: (refreshing: boolean, media?: { audio: boolean; video: boolean }) => void }) {
  const query = useConference(id);
  const lock = useLockConference(), unlock = useUnlockConference();
  const refreshCredential = useMediaCredential(id);
  const room = useRoomContext();
  const [credentialError, setCredentialError] = useState<unknown>();
  const [backstageError, setBackstageError] = useState<unknown>();
  const [backstagePending, setBackstagePending] = useState(false);
  const [mediaContext, setMediaContext] = useState(initialMediaContext);
  const [menuOpen, setMenuOpen] = useState(false);
  const [dialog, setDialog] = useState<"controls" | "quality" | "audio" | null>(null);
  const [lockError, setLockError] = useState<unknown>();
  const c = query.data;
  const me = useCurrentParticipant(id);
  const space = useSpace(c?.meetingSpaceId || "");
  const currentRole = effectiveConferenceRole(space.data?.viewerRole || role, me.data?.role, me.data?.admissionState);
  const canLock = meetingAllows(currentRole, "conference.lock", space.data?.rolePolicy);
  const canEnterBackstage = meetingAllows(currentRole, "backstage.enter", space.data?.rolePolicy);
  const canViewQuality = meetingAllows(currentRole, "quality.view", space.data?.rolePolicy);
  const isWebinar = space.data?.meetingType === "WEBINAR" || space.data?.meetingType === "TOWN_HALL";
  const inBackstage = mediaContext?.kind === "BACKSTAGE";
  // What a reconnect must restore: the mic and camera as they are now (not as at join).
  const currentMedia = () => ({ audio: room.localParticipant.isMicrophoneEnabled, video: room.localParticipant.isCameraEnabled });
  const refresh = async () => {
    setCredentialError(undefined);
    try {
      if (!sessionId) throw new Error("This session cannot refresh its media credential. Leave and rejoin the conference.");
      // Before the request: the server may drop us from the old room first, which must not
      // read as leaving the meeting. Mic and camera stay as the person left them.
      onCredentialRefreshStateChange(true, currentMedia());
      const result = await refreshCredential.mutateAsync({ participantSessionId: sessionId });
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
  // Breakout enter/leave/move: the server switched our media room; fetch a credential for it.
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  const switching = useRef(false);
  useEffect(() => {
    const onMove = (event: Event) => {
      const { participantId, self } = (event as CustomEvent<{ participantId?: string; self?: boolean }>).detail ?? {};
      if (!self && participantId !== me.data?.id) return;
      if (switching.current) return;
      switching.current = true;
      void refreshRef.current().finally(() => { switching.current = false; });
    };
    window.addEventListener(MEDIA_MOVE_EVENT, onMove);
    return () => window.removeEventListener(MEDIA_MOVE_EVENT, onMove);
  }, [me.data?.id]);
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
        onCredentialRefreshStateChange(true, currentMedia());
        const credential = await refreshCredential.mutateAsync({ participantSessionId: sessionId });
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

  const close = () => setMenuOpen(false);
  const live = c?.status === "LIVE";
  const row = "flex w-full items-center gap-3 rounded-lg px-3 py-2.5 text-left text-sm text-foreground transition-colors hover:bg-white/10 disabled:opacity-50";
  const toggleLock = async () => {
    setLockError(undefined);
    try { await (c?.isLocked ? unlock : lock).mutateAsync(id); } catch (error) { setLockError(error); }
  };
  // Host section: only for people who can manage the call.
  const hostRows = live && (canLock || canEnterBackstage || canViewQuality || ["OWNER", "HOST", "CO_HOST", "MODERATOR"].includes(currentRole ?? ""));

  return (
    <>
      <Popover.Root open={menuOpen} onOpenChange={setMenuOpen}>
        <div className="room-control">
          <Popover.Trigger asChild>
            <button type="button" aria-label="More options" title="More options" aria-pressed={menuOpen} className="room-control-button"><MoreHorizontal /></button>
          </Popover.Trigger>
          <span>More</span>
        </div>
        <Popover.Portal>
          <Popover.Content side="top" align="center" sideOffset={14} collisionPadding={12} className="dark z-50 max-h-[70dvh] w-[min(92vw,19rem)] overflow-y-auto rounded-2xl bg-[#2D2E31] p-2 text-foreground shadow-2xl ring-1 ring-white/10">
            {tools?.(close, () => { close(); setDialog("audio"); })}
            {hostRows && (
              <div className="mt-1 border-t border-white/10 pt-2">
                <p className="px-3 pb-1 pt-1 text-[11px] font-semibold uppercase tracking-wider text-white/40">Host controls</p>
                {live && canLock && (
                  <button type="button" className={row} disabled={lock.isPending || unlock.isPending} onClick={() => void toggleLock()}>
                    {c?.isLocked ? <LockOpen className="size-4 text-white/70" /> : <Lock className="size-4 text-white/70" />}
                    <span className="flex-1">{c?.isLocked ? "Unlock meeting" : "Lock meeting"}</span>
                    <span className="text-xs text-white/40">{c?.isLocked ? "Locked" : "Open"}</span>
                  </button>
                )}
                {live && <button type="button" className={row} onClick={() => { close(); setDialog("controls"); }}><SlidersHorizontal className="size-4 text-white/70" /> Meeting controls</button>}
                {live && <button type="button" className={row} onClick={() => { close(); setDialog("quality"); }}><Activity className="size-4 text-white/70" /> Connection quality</button>}
                {live && isWebinar && canEnterBackstage && (
                  <button type="button" className={row} disabled={backstagePending || refreshCredential.isPending} onClick={() => void changeBackstage()}>
                    <Theater className="size-4 text-white/70" /> {backstagePending ? "Changing room…" : inBackstage ? "Return to main stage" : "Go backstage"}
                  </button>
                )}
                {live && (
                  <button type="button" className={row} disabled={refreshCredential.isPending || !sessionId} onClick={() => void refresh()}>
                    <RefreshCw className={`size-4 text-white/70 ${refreshCredential.isPending ? "animate-spin" : ""}`} /> {refreshCredential.isPending ? "Reconnecting…" : "Fix audio/video connection"}
                  </button>
                )}
              </div>
            )}
            {c?.status === "ENDING" && <p className="px-3 py-2 text-xs text-white/60">The meeting is ending. No one new can join.</p>}
            {lockError !== undefined && <div className="p-2"><RequestError error={lockError} /></div>}
            {credentialError !== undefined && <div className="p-2"><RequestError error={credentialError} /></div>}
            {backstageError !== undefined && <div className="p-2"><RequestError error={backstageError} /></div>}
          </Popover.Content>
        </Popover.Portal>
      </Popover.Root>

      <Dialog open={dialog !== null} onOpenChange={(v) => !v && setDialog(null)}>
        <DialogContent className="dark max-h-[85dvh] overflow-y-auto border-white/10 bg-[#2D2E31] text-foreground sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>{dialog === "controls" ? "Meeting controls" : dialog === "audio" ? "Audio settings" : "Connection quality"}</DialogTitle>
            <DialogDescription>{dialog === "controls" ? "Choose what people in this call can do." : dialog === "audio" ? "Choose which microphone and speaker this call uses." : "How your audio and video are coming through."}</DialogDescription>
          </DialogHeader>
          {dialog === "controls" && <CollaborationPolicyControls conferenceId={id} role={currentRole} rolePolicy={space.data?.rolePolicy} />}
          {dialog === "audio" && audioSettings}
          {dialog === "quality" && live && <RoomQualityPanel conferenceId={id} sessionId={sessionId} canViewSummary={canViewQuality} />}
        </DialogContent>
      </Dialog>
    </>
  );
}
