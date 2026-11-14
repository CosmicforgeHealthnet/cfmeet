"use client";

import { NiceSelect } from "@/components/ui/nice-select";
import { useState } from "react";
import type { ParticipantResponse, Role, RolePolicy, TrackKind, BanScope } from "@/types/v2";
import { useParticipantsV2, useCurrentParticipant, useChangeRole, useRemoveParticipant, useBanParticipant, useMuteParticipant, useReturnToWaiting } from "@/hooks/v2/useParticipants";
import { useConference } from "@/hooks/v2/useConferences";
import { useAddSpotlight, useCollaborationState, useLockAudio, useLockScreenShare, useLockVideo, useLowerOtherHand, useRemoveSpotlight, useRestrictChat, useStopScreenShare, useUnlockAudio, useUnlockScreenShare, useUnlockVideo, useUnrestrictChat } from "@/hooks/v2/useCollaboration";
import { effectiveConferenceRole, mayTarget, meetingAllows, participantRoles } from "@/lib/meeting-authority";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import { ActionDialog, Field, RequestError } from "@/components/meeting-spaces/ManagementUI";
import { ChevronDown, Hand, MonitorUp, Mic, Users, Video, X } from "lucide-react";
import { useEffect } from "react";
import type { RaisedHand } from "@/types/v2";

const nice = (v: string) => v.replaceAll("_", " ").toLowerCase().replace(/^./, (c) => c.toUpperCase());
const ACT = "h-9 rounded-xl border-border bg-card px-3.5 text-sm font-semibold text-foreground shadow-none hover:bg-muted";

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
    <div className="flex items-center justify-between gap-3"><h2 className="text-base font-semibold text-foreground">People{roster.data ? <span className="ml-1.5 text-muted-foreground">{roster.data.length}</span> : null}</h2>{me.data && <span className="rounded-md bg-muted px-2 py-0.5 text-xs text-muted-foreground">You&apos;re {nice(me.data.role).toLowerCase()}</span>}</div>
    {me.isError && (me.error as { response?: { status?: number } })?.response?.status !== 404 && <RequestError error={me.error} retry={() => void me.refetch()} />}
    {collaboration.isError && <RequestError error={collaboration.error} retry={() => void collaboration.refetch()} />}
    {feedback && <p role="status" className="rounded-xl bg-primary/5 px-4 py-3 text-sm text-foreground">{feedback}</p>}
    {!!collaboration.data?.raisedHands.length && <RaisedHands
      hands={collaboration.data.raisedHands}
      canLower={(participantId) => { const participant = people.find(person => person.id === participantId); return active && canLowerHand && !!participant && mayTarget(role, participant.role, me.data?.id === participant.id); }}
      lower={(participantId) => lowerHand.mutateAsync(participantId)}
      pending={lowerHand.isPending}
      error={lowerHand.isError ? lowerHand.error : undefined}
      myId={me.data?.id}
    />}
    {roster.isLoading && <div aria-busy="true" className="space-y-2">{[0, 1, 2].map(i => <div key={i} className="h-14 animate-pulse rounded-xl bg-muted" />)}</div>}
    {roster.isError && <RequestError error={roster.error} retry={() => void roster.refetch()} />}
    {roster.data?.length === 0 && <div className="flex flex-col items-center py-8 text-center"><span className="flex size-12 items-center justify-center rounded-2xl bg-muted text-muted-foreground"><Users className="size-5" /></span><p className="mt-3 text-sm font-medium text-foreground">No one has joined</p><p className="text-sm text-muted-foreground">People appear here once they&apos;re in the call.</p></div>}
    {people.map(participant => <ParticipantRow key={participant.id} participant={participant} actorRole={role} self={me.data?.id === participant.id} conferenceId={conferenceId} policy={policy} active={active} live={conference.data?.status === "LIVE"} audioLocked={me.data?.id === participant.id ? collaboration.data?.me?.audioLocked : undefined} videoLocked={me.data?.id === participant.id ? collaboration.data?.me?.videoLocked : undefined} screenShareLocked={me.data?.id === participant.id ? collaboration.data?.me?.screenShareLocked : undefined} isSharing={collaboration.data?.activeScreenShares.includes(participant.id) ?? false} isSpotlighted={collaboration.data?.spotlights.some(item => item.participantId === participant.id) ?? false} feedback={setFeedback} />)}
  </section>;
}

function ParticipantRow({ participant: p, actorRole, self, conferenceId, policy, active, live, audioLocked: initialAudioLock, videoLocked: initialVideoLock, screenShareLocked: initialShareLock, isSharing, isSpotlighted, feedback }: {
  participant: ParticipantResponse; actorRole?: Role; self: boolean; conferenceId: string; policy?: RolePolicy; active: boolean; live: boolean;
  audioLocked?: boolean; videoLocked?: boolean; screenShareLocked?: boolean; isSharing: boolean; isSpotlighted: boolean; feedback: (value: string) => void;
}) {
  const changeRole = useChangeRole(conferenceId), remove = useRemoveParticipant(conferenceId), ban = useBanParticipant(conferenceId), mute = useMuteParticipant(conferenceId), returnToWaiting = useReturnToWaiting(conferenceId);
  const lockAudio = useLockAudio(conferenceId), unlockAudio = useUnlockAudio(conferenceId);
  const lockVideo = useLockVideo(conferenceId), unlockVideo = useUnlockVideo(conferenceId);
  const lockShare = useLockScreenShare(conferenceId), unlockShare = useUnlockScreenShare(conferenceId);
  const stopShare = useStopScreenShare(conferenceId), addSpotlight = useAddSpotlight(conferenceId), removeSpotlight = useRemoveSpotlight(conferenceId);
  const restrictChat = useRestrictChat(conferenceId), unrestrictChat = useUnrestrictChat(conferenceId);
  const [open, setOpen] = useState(false);
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
  const controlError = lockAudio.error || unlockAudio.error || lockVideo.error || unlockVideo.error || lockShare.error || unlockShare.error || stopShare.error || addSpotlight.error || removeSpotlight.error || restrictChat.error || unrestrictChat.error || returnToWaiting.error;
  const canManage = target || canSpotlight || (active && self && isSharing);
  return <article className="rounded-xl border border-border bg-background">
    <div className="flex items-center gap-3 p-3">
      <Avatar className="size-10"><AvatarFallback className="bg-primary/10 text-sm font-semibold text-primary">{p.displayName.slice(0, 2).toUpperCase()}</AvatarFallback></Avatar>
      <div className="min-w-0 flex-1">
        <p className="truncate text-sm font-medium text-foreground">{p.displayName}{self ? <span className="text-muted-foreground"> (you)</span> : null}</p>
        <div className="mt-1 flex flex-wrap gap-1.5 text-xs">
          <span className="rounded-md bg-primary/10 px-2 py-0.5 font-medium text-primary">{nice(p.role)}</span>
          {p.admissionState !== "ADMITTED" && <span className="rounded-md bg-muted px-2 py-0.5 text-muted-foreground">{nice(p.admissionState)}</span>}
          {isSpotlighted && <span className="rounded-md bg-amber-500/10 px-2 py-0.5 font-medium text-amber-700 dark:text-amber-300">Spotlighted</span>}
          {isSharing && <span className="rounded-md bg-[var(--success)]/12 px-2 py-0.5 font-medium text-[var(--success)]">Sharing screen</span>}
        </div>
      </div>
      {canManage && <button type="button" aria-expanded={open} onClick={() => setOpen(o => !o)} className="inline-flex h-9 items-center gap-1 rounded-xl border border-border px-3 text-sm font-semibold text-foreground hover:bg-muted">Manage <ChevronDown className={`size-4 transition-transform ${open ? "rotate-180" : ""}`} /></button>}
    </div>
    {self && (initialAudioLock || initialVideoLock || initialShareLock) && <p className="px-3 pb-3 text-xs text-amber-600 dark:text-amber-400">A host locked your {[initialAudioLock && "mic", initialVideoLock && "camera", initialShareLock && "screen sharing"].filter(Boolean).join(", ")}.</p>}
    {controlError && <div className="px-3 pb-3"><RequestError error={controlError} /></div>}
    {canManage && open && <div className="flex flex-wrap gap-2 border-t border-border p-3">
      {target && meetingAllows(actorRole, "participant.audio_lock", policy) && <><Button size="sm" variant="outline" className={ACT} disabled={lockAudio.isPending || unlockAudio.isPending} onClick={() => void setLock("audio", true, lockAudio, unlockAudio)}>Lock mic</Button><Button size="sm" variant="outline" className={ACT} disabled={lockAudio.isPending || unlockAudio.isPending} onClick={() => void setLock("audio", false, lockAudio, unlockAudio)}>Allow mic</Button></>}
      {target && meetingAllows(actorRole, "participant.video_lock", policy) && <><Button size="sm" variant="outline" className={ACT} disabled={lockVideo.isPending || unlockVideo.isPending} onClick={() => void setLock("video", true, lockVideo, unlockVideo)}>Lock camera</Button><Button size="sm" variant="outline" className={ACT} disabled={lockVideo.isPending || unlockVideo.isPending} onClick={() => void setLock("video", false, lockVideo, unlockVideo)}>Allow camera</Button></>}
      {target && meetingAllows(actorRole, "participant.screen_share_stop", policy) && <><Button size="sm" variant="outline" className={ACT} disabled={lockShare.isPending || unlockShare.isPending} onClick={() => void setLock("screenShare", true, lockShare, unlockShare)}>Lock sharing</Button><Button size="sm" variant="outline" className={ACT} disabled={lockShare.isPending || unlockShare.isPending} onClick={() => void setLock("screenShare", false, lockShare, unlockShare)}>Allow sharing</Button></>}
      {canSpotlight && <Button size="sm" variant="outline" className={ACT} disabled={addSpotlight.isPending || removeSpotlight.isPending} onClick={async () => {
        try {
          if (isSpotlighted) await removeSpotlight.mutateAsync(p.id);
          else await addSpotlight.mutateAsync({ participantId: p.id });
          feedback(isSpotlighted ? "Spotlight removed." : `${p.displayName} is spotlighted.`);
        } catch {
          feedback("");
        }
      }}>{isSpotlighted ? "Remove spotlight" : "Spotlight"}</Button>}
      {active && isSharing && (self || (target && meetingAllows(actorRole, "participant.screen_share_stop", policy))) && <Button size="sm" variant="outline" className={ACT} disabled={stopShare.isPending} onClick={async () => {
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
      {target && meetingAllows(actorRole, "participant.role_change", policy) && roles.length > 0 && <ActionDialog label="Change role" onOpen={() => { setRole(p.role); setReviewedVersion(p.version); }} description="Change their role for this session only. Their membership of the meeting stays the same." submit={async () => {
        if (!roles.includes(role)) throw new Error("Choose a role within your grant authority.");
        await changeRole.mutateAsync({ participantId: p.id, data: { role, version: reviewedVersion } }); feedback("Conference role updated. Media permissions are synchronized by the provider.");
      }}><NiceSelect label="Role" placeholder="Choose a role" value={roles.includes(role) ? role : ""} onChange={setRole} options={roles.map(item => ({ value: item, label: nice(item) }))} /></ActionDialog>}
      {target && live && meetingAllows(actorRole, "participant.mute", policy) && <ActionDialog label="Mute" description="Mute this person right now. They can unmute themselves unless you also lock it." submit={async () => { const result = await mute.mutateAsync({ participantId: p.id, data: { trackKind: track } }); feedback(`Provider muted ${result.mutedTracks} track(s) across ${result.sessions} examined session(s).`); }}><NiceSelect<TrackKind> label="What to mute" value={track} onChange={setTrack} options={[{ value: "AUDIO", label: "Microphone", icon: Mic }, { value: "VIDEO", label: "Camera", icon: Video }, { value: "SCREEN_SHARE", label: "Screen share", icon: MonitorUp }]} /></ActionDialog>}
      {target && meetingAllows(actorRole, "participant.remove", policy) && <ActionDialog label="Remove" destructive description="Remove this person from this session. They could join again later unless you ban them." submit={async () => enforcement(await remove.mutateAsync({ participantId: p.id, data: { ...(reason.trim() ? { reason: reason.trim() } : {}) } }))}><Field label="Reason (optional)" value={reason} maxLength={500} onChange={e => setReason(e.target.value)} /></ActionDialog>}
      {target && meetingAllows(actorRole, "participant.return_to_waiting", policy) && <ActionDialog label="Send to waiting room" destructive description="Move this person back to the waiting room. They'll need to be let in again." submit={async () => {
        const result = await returnToWaiting.mutateAsync(p.id);
        feedback(result.providerEnforcement === "PENDING_PROVIDER_CONTROL" ? "Participant returned to waiting. Provider disconnection is still pending." : "Participant returned to waiting.");
      }} />}
      {target && meetingAllows(actorRole, "participant.ban", policy) && <ActionDialog label="Ban" destructive description="Remove this person and stop them from rejoining. Choose whether it applies to this session or every session of this meeting." submit={async () => {
        if (expires && (!Number.isFinite(Date.parse(expires)) || Date.parse(expires) <= Date.now())) throw new Error("Ban expiry must be in the future.");
        enforcement(await ban.mutateAsync({ participantId: p.id, data: { scope, ...(reason.trim() ? { reason: reason.trim() } : {}), ...(expires ? { expiresAt: new Date(expires).toISOString() } : {}) } }));
      }}><NiceSelect<BanScope> label="Applies to" value={scope} onChange={setScope} options={[{ value: "CONFERENCE", label: "This session only" }, { value: "SPACE", label: "Every session of this meeting", hint: "They can't join any future session" }]} /><Field label="Reason (optional)" maxLength={500} value={reason} onChange={e => setReason(e.target.value)} /><Field label="Expires (optional, browser timezone)" type="datetime-local" value={expires} onChange={e => setExpires(e.target.value)} /></ActionDialog>}
    </div>}
  </article>;
}

/** Raised hands as a queue: first raised is first in line, with how long each has waited (Meet shows only names). */
function RaisedHands({ hands, canLower, lower, pending, error, myId }: { hands: RaisedHand[]; canLower: (participantId: string) => boolean; lower: (participantId: string) => Promise<unknown>; pending: boolean; error?: unknown; myId?: string }) {
  const queue = [...hands].sort((a, b) => a.raisedAt.localeCompare(b.raisedAt));
  const lowerable = queue.filter(hand => canLower(hand.participantId));
  const [now, setNow] = useState(() => Date.now());
  const [clearing, setClearing] = useState(false);
  useEffect(() => { const t = setInterval(() => setNow(Date.now()), 15000); return () => clearInterval(t); }, []);
  const waited = (at: string) => { const m = Math.floor((now - Date.parse(at)) / 60000); return m < 1 ? "just now" : `${m} min`; };
  const lowerAll = async () => {
    setClearing(true);
    for (const hand of lowerable) { try { await lower(hand.participantId); } catch { break; } }
    setClearing(false);
  };
  return <section className="overflow-hidden rounded-2xl border border-[#a8c7fa]/40 bg-[#a8c7fa]/10">
    <div className="flex items-center gap-2 px-4 pt-3">
      <Hand className="size-4 text-[#8ab4ff]" />
      <h3 className="flex-1 text-sm font-semibold text-foreground">Hands raised <span className="text-muted-foreground">· {queue.length}</span></h3>
      {lowerable.length > 1 && <button type="button" disabled={pending || clearing} onClick={() => void lowerAll()} className="h-8 rounded-full px-3 text-sm font-semibold text-primary hover:bg-primary/10 disabled:opacity-50">{clearing ? "Lowering…" : "Lower all"}</button>}
    </div>
    <ol className="space-y-0.5 p-2">
      {queue.map((hand, index) => <li key={hand.participantId} className="flex items-center gap-3 rounded-xl px-2 py-2">
        <span className={`flex size-7 shrink-0 items-center justify-center rounded-full text-xs font-bold ${index === 0 ? "bg-[#a8c7fa] text-[#062e6f]" : "bg-muted text-muted-foreground"}`}>{index + 1}</span>
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-medium text-foreground">{hand.displayName}{hand.participantId === myId && <span className="ml-1.5 text-xs font-normal text-muted-foreground">You</span>}</p>
          <p className="text-xs text-muted-foreground">{index === 0 ? "Next to speak · " : ""}{waited(hand.raisedAt)}</p>
        </div>
        {canLower(hand.participantId) && <button type="button" aria-label={`Lower ${hand.displayName}'s hand`} title="Lower hand" disabled={pending || clearing} onClick={() => void lower(hand.participantId).catch(() => undefined)} className="flex size-8 shrink-0 items-center justify-center rounded-full text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50"><X className="size-4" /></button>}
      </li>)}
    </ol>
    {!!error && <div className="px-3 pb-3"><RequestError error={error} /></div>}
  </section>;
}
