"use client";

import { useState } from "react";
import type { ParticipantResponse, Role, RolePolicy, TrackKind, BanScope } from "@/types/v2";
import { useParticipantsV2, useCurrentParticipant, useChangeRole, useRemoveParticipant, useBanParticipant, useMuteParticipant } from "@/hooks/v2/useParticipants";
import { useConference } from "@/hooks/v2/useConferences";
import { useAddSpotlight, useCollaborationState, useLockAudio, useLockScreenShare, useLockVideo, useLowerOtherHand, useRemoveSpotlight, useRestrictChat, useStopScreenShare, useUnlockAudio, useUnlockScreenShare, useUnlockVideo, useUnrestrictChat } from "@/hooks/v2/useCollaboration";
import { effectiveConferenceRole, mayTarget, meetingAllows, participantRoles } from "@/lib/meeting-authority";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import { ActionDialog, Field, RequestError } from "@/components/meeting-spaces/ManagementUI";

export default function ParticipantsPanel({ conferenceId, spaceRole, policy }: { conferenceId: string; spaceRole?: Role; policy?: RolePolicy }) {
  const roster = useParticipantsV2(conferenceId);
  const me = useCurrentParticipant(conferenceId);
  const collaboration = useCollaborationState(conferenceId);
  const conference = useConference(conferenceId);
  const [feedback, setFeedback] = useState("");
  const role = effectiveConferenceRole(spaceRole, me.data?.role, me.data?.admissionState);
  const active = !!conference.data && !conference.data.isLegacy && ["CREATED", "PREPARING", "WAITING", "LIVE"].includes(conference.data.status);
  const people = roster.data || [];
  const lowerHand = useLowerOtherHand(conferenceId);
  const canLowerHand = meetingAllows(role, "hand.lower_other", policy);
  return <section className="space-y-3">
    <div><h2 className="font-semibold">Participants{roster.data ? ` (${roster.data.length})` : ""}</h2><p className="text-xs text-muted-foreground">Admitted people in this occurrence. Admission does not imply an active media connection.</p></div>
    {me.data && <p className="text-xs text-muted-foreground">You: {me.data.displayName} · {me.data.role.replaceAll("_", " ")} · {me.data.admissionState}</p>}
    {me.isError && (me.error as { response?: { status?: number } })?.response?.status !== 404 && <RequestError error={me.error} retry={() => void me.refetch()} />}
    {collaboration.isError && <RequestError error={collaboration.error} retry={() => void collaboration.refetch()} />}
    {feedback && <p role="status" className="rounded-md bg-muted p-3 text-sm">{feedback}</p>}
    {!!collaboration.data?.raisedHands.length && <section className="space-y-2 rounded-lg border border-border p-3"><h3 className="text-sm font-semibold">Raised hands ({collaboration.data.raisedHands.length})</h3>{collaboration.data.raisedHands.map(hand => {
      const participant = people.find(person => person.id === hand.participantId);
      const canLower = active && canLowerHand && !!participant && mayTarget(role, participant.role, me.data?.id === participant.id);
      return <div key={hand.participantId} className="flex items-center justify-between gap-2 text-sm"><span>{hand.displayName}</span>{canLower && <Button size="sm" variant="outline" disabled={lowerHand.isPending} onClick={() => void lowerHand.mutateAsync(hand.participantId).catch(() => undefined)}>Lower hand</Button>}</div>;
    })}{lowerHand.isError && <RequestError error={lowerHand.error} />}</section>}
    {roster.isLoading && <p role="status" className="text-sm">Loading participants…</p>}
    {roster.isError && <RequestError error={roster.error} retry={() => void roster.refetch()} />}
    {roster.data?.length === 0 && <p className="text-sm text-muted-foreground">No admitted participants.</p>}
    {people.map(participant => <ParticipantRow key={participant.id} participant={participant} actorRole={role} self={me.data?.id === participant.id} conferenceId={conferenceId} policy={policy} active={active} live={conference.data?.status === "LIVE"} audioLocked={me.data?.id === participant.id ? collaboration.data?.me?.audioLocked : undefined} videoLocked={me.data?.id === participant.id ? collaboration.data?.me?.videoLocked : undefined} screenShareLocked={me.data?.id === participant.id ? collaboration.data?.me?.screenShareLocked : undefined} isSharing={collaboration.data?.activeScreenShares.includes(participant.id) ?? false} isSpotlighted={collaboration.data?.spotlights.some(item => item.participantId === participant.id) ?? false} feedback={setFeedback} />)}
  </section>;
}

function ParticipantRow({ participant: p, actorRole, self, conferenceId, policy, active, live, audioLocked: initialAudioLock, videoLocked: initialVideoLock, screenShareLocked: initialShareLock, isSharing, isSpotlighted, feedback }: {
  participant: ParticipantResponse; actorRole?: Role; self: boolean; conferenceId: string; policy?: RolePolicy; active: boolean; live: boolean;
  audioLocked?: boolean; videoLocked?: boolean; screenShareLocked?: boolean; isSharing: boolean; isSpotlighted: boolean; feedback: (value: string) => void;
}) {
  const changeRole = useChangeRole(conferenceId), remove = useRemoveParticipant(conferenceId), ban = useBanParticipant(conferenceId), mute = useMuteParticipant(conferenceId);
  const lockAudio = useLockAudio(conferenceId), unlockAudio = useUnlockAudio(conferenceId);
  const lockVideo = useLockVideo(conferenceId), unlockVideo = useUnlockVideo(conferenceId);
  const lockShare = useLockScreenShare(conferenceId), unlockShare = useUnlockScreenShare(conferenceId);
  const stopShare = useStopScreenShare(conferenceId), addSpotlight = useAddSpotlight(conferenceId), removeSpotlight = useRemoveSpotlight(conferenceId);
  const restrictChat = useRestrictChat(conferenceId), unrestrictChat = useUnrestrictChat(conferenceId);
  const [role, setRole] = useState<Role>(p.role);
  const [reviewedVersion, setReviewedVersion] = useState(p.version);
  const [track, setTrack] = useState<TrackKind>("AUDIO");
  const [reason, setReason] = useState("");
  const [scope, setScope] = useState<BanScope>("CONFERENCE");
  const [expires, setExpires] = useState("");
  const roles = participantRoles(actorRole, policy);
  const target = active && p.conferenceId === conferenceId && mayTarget(actorRole, p.role, self);
  const canSpotlight = live && meetingAllows(actorRole, "participant.spotlight", policy);
  const canModerateChat = target && meetingAllows(actorRole, "chat.moderate", policy);
  const enforcement = (result: { providerEnforcement: string }) => feedback(result.providerEnforcement === "PENDING_PROVIDER_CONTROL" ? "The change is saved. Media enforcement is queued; the provider has not confirmed disconnection yet." : "The change is saved and any required provider action is confirmed.");
  const setLock = async (kind: "audio" | "video" | "screenShare", locked: boolean, lock: typeof lockAudio, unlock: typeof unlockAudio) => {
    try {
      const response = await (locked ? lock : unlock).mutateAsync(p.id);
      const actual = kind === "audio" ? response.audioLocked : kind === "video" ? response.videoLocked : response.screenShareLocked;
      feedback(`${kind === "screenShare" ? "Screen sharing" : kind === "audio" ? "Microphone" : "Camera"} ${actual ? "locked" : "unlocked"}. Provider enforcement: ${response.providerEnforcement}.`);
    } catch {
      feedback("");
    }
  };
  const controlError = lockAudio.error || unlockAudio.error || lockVideo.error || unlockVideo.error || lockShare.error || unlockShare.error || stopShare.error || addSpotlight.error || removeSpotlight.error || restrictChat.error || unrestrictChat.error;
  return <article className="space-y-2 rounded-lg border border-border bg-card p-3"><div className="flex items-center gap-2"><Avatar className="h-8 w-8"><AvatarFallback>{p.displayName.slice(0, 2).toUpperCase()}</AvatarFallback></Avatar><div className="min-w-0"><p className="break-words text-sm font-medium">{p.displayName}{self ? " (you)" : ""}{isSpotlighted && <span className="ml-2 rounded-full bg-primary/10 px-2 py-0.5 text-xs text-primary">Spotlighted</span>}</p><p className="text-xs text-muted-foreground">{p.role.replaceAll("_", " ")} · {p.admissionState}</p></div></div>
    {self && <p className="text-xs text-muted-foreground">{initialAudioLock ? "Microphone locked" : "Microphone unlocked"} · {initialVideoLock ? "Camera locked" : "Camera unlocked"} · {initialShareLock ? "Screen sharing locked" : "Screen sharing unlocked"}</p>}
    {controlError && <RequestError error={controlError} />}
    {(target || canSpotlight || (active && self && isSharing)) && <div className="flex flex-wrap gap-2">
      {target && meetingAllows(actorRole, "participant.audio_lock", policy) && <><Button size="sm" variant="outline" disabled={lockAudio.isPending || unlockAudio.isPending} onClick={() => void setLock("audio", true, lockAudio, unlockAudio)}>Lock microphone</Button><Button size="sm" variant="outline" disabled={lockAudio.isPending || unlockAudio.isPending} onClick={() => void setLock("audio", false, lockAudio, unlockAudio)}>Unlock microphone</Button></>}
      {target && meetingAllows(actorRole, "participant.video_lock", policy) && <><Button size="sm" variant="outline" disabled={lockVideo.isPending || unlockVideo.isPending} onClick={() => void setLock("video", true, lockVideo, unlockVideo)}>Lock camera</Button><Button size="sm" variant="outline" disabled={lockVideo.isPending || unlockVideo.isPending} onClick={() => void setLock("video", false, lockVideo, unlockVideo)}>Unlock camera</Button></>}
      {target && meetingAllows(actorRole, "participant.screen_share_stop", policy) && <><Button size="sm" variant="outline" disabled={lockShare.isPending || unlockShare.isPending} onClick={() => void setLock("screenShare", true, lockShare, unlockShare)}>Lock screen share</Button><Button size="sm" variant="outline" disabled={lockShare.isPending || unlockShare.isPending} onClick={() => void setLock("screenShare", false, lockShare, unlockShare)}>Unlock screen share</Button></>}
      {canSpotlight && <Button size="sm" variant="outline" disabled={addSpotlight.isPending || removeSpotlight.isPending} onClick={async () => {
        try {
          if (isSpotlighted) await removeSpotlight.mutateAsync(p.id);
          else await addSpotlight.mutateAsync({ participantId: p.id });
          feedback(isSpotlighted ? "Spotlight removed." : `${p.displayName} is spotlighted.`);
        } catch {
          feedback("");
        }
      }}>{isSpotlighted ? "Remove spotlight" : "Spotlight"}</Button>}
      {active && isSharing && (self || (target && meetingAllows(actorRole, "participant.screen_share_stop", policy))) && <Button size="sm" variant="outline" disabled={stopShare.isPending} onClick={async () => {
        try {
          const result = await stopShare.mutateAsync(p.id);
          feedback(`Screen share stopped for ${result.stoppedSessions} session(s).`);
        } catch {
          feedback("");
        }
      }}>Stop screen share</Button>}
      {canModerateChat && <><ActionDialog label="Restrict chat" description="Prevent this participant from sending conference chat messages. This does not change meeting-space membership." submit={async () => {
        const result = await restrictChat.mutateAsync(p.id);
        feedback(result.changed ? "Chat restricted for this participant." : "Chat was already restricted.");
      }} /><ActionDialog label="Allow chat" description="Restore this participant’s conference chat access." submit={async () => {
        const result = await unrestrictChat.mutateAsync(p.id);
        feedback(result.changed ? "Chat access restored." : "Chat was already allowed.");
      }} /></>}
      {target && meetingAllows(actorRole, "participant.role_change", policy) && roles.length > 0 && <ActionDialog label="Change role" onOpen={() => { setRole(p.role); setReviewedVersion(p.version); }} description="Change the role in this conference only. Persistent membership is unchanged." submit={async () => {
        if (!roles.includes(role)) throw new Error("Choose a role within your grant authority.");
        await changeRole.mutateAsync({ participantId: p.id, data: { role, version: reviewedVersion } }); feedback("Conference role updated. Media permissions are synchronized by the provider.");
      }}><label className="grid gap-2 text-sm font-medium">Role<select className="h-10 rounded-md border border-input bg-background px-3" value={roles.includes(role) ? role : ""} onChange={e => setRole(e.target.value as Role)} required><option value="" disabled>Select role</option>{roles.map(item => <option key={item} value={item}>{item.replaceAll("_", " ")}</option>)}</select></label></ActionDialog>}
      {target && live && meetingAllows(actorRole, "participant.mute", policy) && <ActionDialog label="Mute tracks" description="Ask the provider to mute published tracks across this participant's connected sessions. This does not unmute or permanently lock media." submit={async () => { const result = await mute.mutateAsync({ participantId: p.id, data: { trackKind: track } }); feedback(`Provider muted ${result.mutedTracks} track(s) across ${result.sessions} examined session(s).`); }}><label className="grid gap-2 text-sm font-medium">Track<select className="h-10 rounded-md border border-input bg-background px-3" value={track} onChange={e => setTrack(e.target.value as TrackKind)}><option value="AUDIO">Microphone audio</option><option value="VIDEO">Camera video</option><option value="SCREEN_SHARE">Screen share</option></select></label></ActionDialog>}
      {target && meetingAllows(actorRole, "participant.remove", policy) && <ActionDialog label="Remove" destructive description="Remove this participant from this occurrence and request media disconnection. This does not ban future admission." submit={async () => enforcement(await remove.mutateAsync({ participantId: p.id, data: { ...(reason.trim() ? { reason: reason.trim() } : {}) } }))}><Field label="Reason (optional)" value={reason} maxLength={500} onChange={e => setReason(e.target.value)} /></ActionDialog>}
      {target && meetingAllows(actorRole, "participant.ban", policy) && <ActionDialog label="Ban" destructive description="Ban this identity and request media disconnection. A space ban also blocks admission to other occurrences in this space." submit={async () => {
        if (expires && (!Number.isFinite(Date.parse(expires)) || Date.parse(expires) <= Date.now())) throw new Error("Ban expiry must be in the future.");
        enforcement(await ban.mutateAsync({ participantId: p.id, data: { scope, ...(reason.trim() ? { reason: reason.trim() } : {}), ...(expires ? { expiresAt: new Date(expires).toISOString() } : {}) } }));
      }}><label className="grid gap-2 text-sm font-medium">Scope<select className="h-10 rounded-md border border-input bg-background px-3" value={scope} onChange={e => setScope(e.target.value as BanScope)}><option value="CONFERENCE">This occurrence</option><option value="SPACE">All occurrences in this meeting space</option></select></label><Field label="Reason (optional)" maxLength={500} value={reason} onChange={e => setReason(e.target.value)} /><Field label="Expires (optional, browser timezone)" type="datetime-local" value={expires} onChange={e => setExpires(e.target.value)} /></ActionDialog>}
    </div>}
  </article>;
}
