"use client";

/**
 * The active call, on every signed-in page (CAL-002, CAL-015): media connection, its surfaced
 * status (connecting / network interrupted / retrying in n s / could not restore) with an
 * automatic rejoin, and the controls the signed-in party may use — Leave for anyone, End for
 * all / Hold / Resume for any active participant (ADR 0009 party rules), Cancel for the caller
 * before the answer. Any active party may also turn a voice call into a video call (C5: the
 * camera button appears; no camera is turned on), share its screen when Calling offers it (C6:
 * one presenter per call, shown on everyone's stage) and add people (C7).
 *
 * Design (Google Meet / Duo call screen): a full-screen stage — big avatar with ripples while it
 * rings, a live timer once connected, their video full-bleed with yours in a corner window —
 * and one floating control pill. Minimise shrinks it to a strip so the app stays usable; the
 * media connection stays mounted either way so audio never drops.
 */
import { useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import Link from "next/link";
import { Flag, Info, Maximize2, Minimize2, MoreHorizontal, Pause, Mic, Volume2, PhoneOff, Play, RefreshCw, UserPlus, Users, Video } from "lucide-react";
import { useCalling } from "@/context/CallingContext";
import { callingKeys, useCall, useCallActions, useCallingMutation } from "@/hooks/calling/useCalls";
import { callsService } from "@/services/calling/calls.service";
import { toast } from "@/components/ui/toast";
import { usePrincipalNames } from "@/hooks/calling/usePrincipalNames";
import { callingErrorMessage } from "@/lib/calling/errors";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import { callerLabel } from "@/lib/calling/incoming-call";
import { isPresenter, ownSession, visibleControls } from "@/lib/calling/call-permissions";
import { mediaStatusLabel } from "@/lib/calling/media-reconnect";
import { cn } from "@/lib/utils";
import { rememberedCallName } from "@/lib/calling/call-names";
import { Popover } from "radix-ui";
import { DeviceRecordMenuItem } from "@/components/recording/DeviceRecording";
import AbuseReportDialog from "./AbuseReportDialog";
import AddPeopleDialog from "./AddPeopleDialog";
import CallMedia, { closePreviewCamera, openPreviewCamera, CallButton, CallUpgradeButton, CallCameraButton, CallFlipButton, CallMicButton, CallMicToggle, CallScreenShareButton, CallSpeakerButton, CallVideoStage, MutedTag, useAnyVideo } from "./CallMedia";

const COLORS = ["#2563eb", "#059669", "#d97706", "#7c3aed", "#db2777", "#0891b2", "#4f46e5", "#ea580c"];
const colorFor = (raw: string) => COLORS[([...raw.trim().toLowerCase()].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % COLORS.length];
const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";
const clock = (ms: number) => {
  const t = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(t / 3600), m = Math.floor((t % 3600) / 60), s = t % 60;
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}` : `${m}:${String(s).padStart(2, "0")}`;
};
const moreItem = "flex w-full items-center gap-3 rounded-xl px-3 py-3 text-left text-sm hover:bg-white/10";
const RINGING = ["REQUESTED", "AUTHORIZED", "RESOLVING", "ROUTING", "DIALING", "RINGING"];

/**
 * Your own camera, full screen, from the moment a video call starts (WhatsApp / FaceTime),
 * until the call's real video takes over. Local only: nothing is sent from here.
 */
function CameraPreview() {
  const ref = useRef<HTMLVideoElement>(null);
  useEffect(() => {
    let live = true;
    void openPreviewCamera().then((s) => { if (live && s && ref.current) ref.current.srcObject = s; });
    return () => { live = false; };
  }, []);
  return <video ref={ref} autoPlay playsInline muted aria-hidden className="absolute inset-0 h-full w-full -scale-x-100 object-cover opacity-90" />;
}

type StageProps = {
  inRoom: boolean;
  video: boolean;
  /** Show the local camera preview before the call's video arrives (calls placed as video only). */
  preview?: boolean;
  group: boolean;
  seed: string;
  counterpart: string;
  status: string;
  ringing: boolean;
  connected: boolean;
  held: boolean;
  failed: boolean;
  error: string | null;
  onRetry: () => void;
  onMinimise: () => void;
  controls: React.ReactNode;
  hangup: React.ReactNode;
  footerLinks: React.ReactNode;
};

/** Full-screen stage. Inside <CallMedia> it can switch to video and show the mic state. */
function Stage(p: StageProps) {
  return p.inRoom ? <StageInRoom {...p} /> : <StageView {...p} anyVideo={false} />;
}
function StageInRoom(p: StageProps) {
  const anyVideo = useAnyVideo();
  return <StageView {...p} anyVideo={p.video && anyVideo} />;
}
export function StageView({ inRoom, video, preview = video, group, seed, counterpart, status, ringing, connected, held, failed, error, onRetry, onMinimise, controls, hangup, footerLinks, anyVideo }: StageProps & { anyVideo: boolean }) {
  return (
    <div className="relative flex h-full flex-col overflow-hidden">
      {inRoom && video ? <CallVideoStage /> : null}
      {preview && !anyVideo ? <CameraPreview /> : null}
      {!anyVideo && !video && <div aria-hidden className="pointer-events-none absolute left-1/2 top-[42%] size-[30rem] -translate-x-1/2 -translate-y-1/2 rounded-full opacity-20 blur-[110px]" style={{ backgroundColor: colorFor(seed) }} />}

      <header className={cn("relative z-10 flex h-16 items-center px-3 sm:h-20 sm:px-5", anyVideo && "bg-gradient-to-b from-black/70 to-transparent")}>
        <button type="button" onClick={onMinimise} aria-label="Minimise call" title="Minimise" className="flex size-11 items-center justify-center rounded-full bg-white/10 text-white hover:bg-white/20"><Minimize2 className="size-5" /></button>
        {anyVideo ? <p className="ml-3 min-w-0 truncate text-sm font-semibold text-white">{counterpart} <span className="font-normal tabular-nums text-white/70">· {status}</span></p> : null}
      </header>

      {!anyVideo ? (
        <div className="relative z-10 flex min-h-0 flex-1 flex-col items-center justify-center gap-5 overflow-hidden px-6 pb-4 text-center sm:gap-7 [@media(max-height:700px)]:gap-3">
          <div className="max-w-md">
            <h2 className="truncate text-[1.75rem] font-semibold leading-tight tracking-tight text-white sm:text-4xl">{counterpart}</h2>
            <p role="status" aria-live="polite" className={cn("mt-1.5 text-base tabular-nums sm:text-lg", connected && !held ? "text-white/85" : "text-white/60")}>{status}</p>
          </div>
          <div className="relative flex items-center justify-center">
            {ringing && [0, 1, 2].map((i) => (
              <span key={i} aria-hidden className="absolute size-28 animate-ping rounded-full sm:size-36 [@media(max-height:700px)]:size-24" style={{ backgroundColor: colorFor(seed), opacity: 0.16, animationDuration: "2.4s", animationDelay: `${i * 0.8}s` }} />
            ))}
            <span className="relative flex size-28 items-center justify-center rounded-full text-4xl font-semibold text-white shadow-2xl sm:size-36 sm:text-5xl [@media(max-height:700px)]:size-24 [@media(max-height:700px)]:text-3xl" style={{ backgroundColor: colorFor(seed) }}>
              {group ? <Users className="size-12" /> : initials(counterpart)}
            </span>
          </div>
          <div className="h-7">{inRoom && connected ? <MutedTag /> : null}</div>
          {failed ? (
            <button type="button" onClick={onRetry} className="flex h-10 items-center gap-2 rounded-xl bg-white/10 px-5 text-sm font-semibold text-white hover:bg-white/20"><RefreshCw className="size-4" />Reconnect</button>
          ) : null}
        </div>
      ) : <div className="flex-1" />}

      {/* Bottom panel of labelled round buttons, end call on the right. */}
      <footer className="relative z-10 px-3 pb-[max(1rem,env(safe-area-inset-bottom))] sm:px-6 sm:pb-8">
        {error ? <p role="alert" className="mx-auto mb-3 max-w-md rounded-xl bg-red-500/20 px-4 py-2 text-center text-sm text-red-100">{error}</p> : null}
        <div className="mx-auto w-full max-w-md rounded-[32px] bg-[#1B1C1E]/95 px-4 py-6 shadow-2xl ring-1 ring-white/10 backdrop-blur-xl sm:w-fit sm:max-w-none sm:px-6 sm:py-4">
          <div className="grid grid-cols-3 justify-items-center gap-y-5 sm:flex sm:items-start sm:gap-2">
            {controls}
            <div className="hidden sm:block">{hangup}</div>
          </div>
          <div className="mt-5 flex justify-center sm:hidden">{hangup}</div>
        </div>
        {footerLinks ? <div className="mt-3 flex items-center justify-center gap-5 text-xs">{footerLinks}</div> : null}
      </footer>
    </div>
  );
}

export default function ActiveCallDock() {
  const calling = useCalling();
  const media = calling?.media ?? null;
  const call = useCall(media?.callId ?? "");
  const actions = useCallActions(media?.callId ?? "");
  const mutation = useCallingMutation();
  const queryClient = useQueryClient();
  const others = call.data?.participants.filter((p) => p.principalId !== calling?.principalId && !p.displayName) ?? [];
  const names = usePrincipalNames(others.map((p) => p.principalId));
  const [full, setFull] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [reporting, setReporting] = useState(false);
  const [adding, setAdding] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const connectedNow = call.data?.state === "CONNECTED";
  useEffect(() => {
    if (!connectedNow) return;
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [connectedNow]);
  // No call on screen any more: release the preview camera (unless the call is sending it).
  const onScreen = !!calling?.media || !!calling?.placing;
  useEffect(() => { if (!onScreen) closePreviewCamera(); }, [onScreen]);
  // Every new call opens full screen.
  useEffect(() => { if (media?.callId) setFull(true); }, [media?.callId]);
  if (!calling) return null;
  if (!media) {
    if (!calling.placing) return null;
    const p = calling.placing;
    const label = p.label ?? calling.callLabel;
    // The same buttons the call will have, shown now (they work once the call is connected).
    const placeholders = (
      <>
        <CallButton icon={<Volume2 />} label="Speaker" disabled />
        <CallButton icon={<Mic />} label="Mute" disabled />
        <CallButton icon={<MoreHorizontal />} label="More" disabled />
      </>
    );
    return (
      <section aria-label="Active call" className="dark fixed inset-0 z-[60] bg-[#202124] text-white">
        <StageView inRoom={false} video={p.kind === "VIDEO"} group={false} seed={label ?? "call"} counterpart={label ?? "Calling…"} status={p.ended ?? (label ? "Calling…" : "")} ringing={!p.ended}
          connected={false} held={false} failed={false} error={null} onRetry={() => undefined} onMinimise={() => undefined} anyVideo={false} footerLinks={null} controls={placeholders}
          hangup={<CallButton icon={<PhoneOff />} label="Cancel" tone="danger" onClick={calling.cancelPlacing} />} />
      </section>
    );
  }

  const { mediaState, mediaEvents } = calling;
  const party = call.data?.participants.filter((p) => p.principalId !== calling.principalId) ?? [];
  const remembered = rememberedCallName(media.callId);
  // The name never jumps: the one shown when the call was placed, then the server's names.
  const placed = calling.callLabel ?? remembered;
  const counterpart = call.data
    ? party.map((p) => p.displayName || names.data?.[p.principalId] || (party.length === 1 ? placed : null) || "CF Meet user").join(", ") || callerLabel(call.data.participants, calling.principalId)
    : placed ?? "Calling…";
  // Colour from the first name shown, so the circle keeps its colour for the whole call.
  const seed = placed ?? counterpart;
  const controls = call.data
    ? visibleControls(call.data, calling.principalId, (name) => !!actions.data?.actions.some((a) => a.action === name && a.available))
    : { cancel: false, end: false, hold: false, resume: false, transfer: false, upgrade: false, screenShare: false, invite: false };
  const session = call.data ? ownSession(call.data, calling.principalId, calling.device.endpoint?.endpointId) : undefined;
  const group = party.length > 1;
  const asOf = call.data ? Date.parse(call.data.durationsAsOf) : NaN;
  const elapsed = connectedNow && Number.isFinite(asOf) ? (call.data?.connectedDurationMs ?? 0) + Math.max(0, now - asOf) : null;
  const run = async (action: () => Promise<unknown>) => {
    setError(null);
    try { await action(); } catch (e) { setError(callingErrorMessage(e)); }
  };
  const command = (operation: "cancel" | "end" | "hold" | "resume") => run(() =>
    mutation.mutateAsync({ operation, callId: media.callId, body: { expectedVersion: call.data!.version }, key: newIdempotencyKey() }));
  // C5: the call becomes a video call; every party's camera button appears (cameras stay off).
  // C6: the adapter names the signed-in user's own participant as the presenter.
  const screenShare = (operation: "startScreenShare" | "stopScreenShare") =>
    mutation.mutateAsync({ operation, callId: media.callId, body: {}, key: newIdempotencyKey() });
  const presenting = call.data ? isPresenter(call.data, calling.principalId) : false;
  const presenter = call.data?.screenShare ? call.data.participants.find((p) => p.participantId === call.data!.screenShare!.participantId) : undefined;
  const presenterLabel = presenting ? "You're presenting"
    : presenter ? `${presenter.displayName || names.data?.[presenter.principalId] || "Someone"} is presenting` : null;

  const state = call.data?.state;
  const held = state === "HELD";
  const ringing = !!state && (RINGING.includes(state) || state === "ANSWERED");
  const status = !state ? "Calling…"
    : held ? "On hold"
    : state === "ANSWERED" ? "Connecting…"
    : state === "RINGING" ? "Ringing…"
    : ringing ? "Calling…"
    : elapsed !== null && mediaState.status === "connected" ? (presenterLabel ? `${clock(elapsed)} · ${presenterLabel}` : clock(elapsed))
    : mediaState.status === "idle" || mediaState.status === "connecting" && !mediaState.attempt ? "Connecting…"
    : mediaStatusLabel(mediaState);
  const dot = mediaState.status === "connected" && !held && !ringing ? "bg-emerald-400" : mediaState.status === "failed" ? "bg-red-500" : "animate-pulse bg-amber-400";
  // Placed as video: the local preview and the automatic camera at join. Upgraded later (C5):
  // the video stage and the camera button only — the user turns the camera on.
  const placedVideo = media.kind === "VIDEO";
  const video = placedVideo || call.data?.kind === "VIDEO";
  const canPublishVideo = !!media.credential?.capabilities.canPublishVideo || call.data?.kind === "VIDEO";
  const hangUpNow = (operation: "cancel" | "end" | "leave") => {
    const data = call.data;
    calling.leaveMedia();
    if (data && operation !== "leave") {
      queryClient.setQueryData(callingKeys.detail(media.callId), { ...data, state: operation === "cancel" ? "CANCELLED" : "ENDED" });
    }
    const callId = media.callId;
    if (operation === "leave") {
      if (data && session) void mutation.mutateAsync({ operation: "removeSession", callId, sessionId: session.sessionId, body: { expectedVersion: data.version }, key: newIdempotencyKey() }).catch(() => undefined);
      return;
    }
    // Cancel / end must reach the server, or the other side keeps ringing: with the call's
    // details (fetched if they hadn't loaded), retried once with a fresh version, and said
    // out loud if it still fails.
    const attempt = async (version?: number) => {
      const current = version ?? (await callsService.get(callId)).version;
      await callsService.hangUp(callId, operation, current);
    };
    void attempt(data?.version)
      .catch(() => attempt())
      .catch(() => toast(operation === "cancel" ? "The call couldn't be cancelled — it may still be ringing. Try again from Calls." : "The call couldn't be ended for everyone. Try again from Calls.", "error"))
      .finally(() => void queryClient.invalidateQueries({ queryKey: callingKeys.detail(callId) }));
  };
  const hangUp = controls.cancel || !call.data
    ? { label: "Cancel", run: () => hangUpNow("cancel") }
    : controls.end && !group ? { label: "End call", run: () => hangUpNow("end") } : { label: "Leave", run: () => hangUpNow("leave") };
  const inRoom = !!media.credential;

  const me = call.data?.participants.find((p) => p.principalId === calling.principalId && !p.endedAt);
  const stageControls = (
    <>
      {inRoom ? <CallSpeakerButton /> : <CallButton icon={<Volume2 />} label="Speaker" disabled />}
      {inRoom && video && <CallCameraButton allowed={canPublishVideo} />}
      {inRoom ? <CallMicButton allowed={!!media.credential?.capabilities.canPublishAudio} /> : <CallButton icon={<Mic />} label="Mute" disabled />}
      {inRoom && video && <CallFlipButton onError={setError} />}
      <Popover.Root>
        <Popover.Trigger asChild><span><CallButton icon={<MoreHorizontal />} label="More" /></span></Popover.Trigger>
        <Popover.Portal>
          <Popover.Content side="top" sideOffset={10} collisionPadding={12} className="dark z-[80] w-60 rounded-2xl bg-[#2D2E31] p-1.5 text-white shadow-2xl ring-1 ring-white/10">
            {controls.invite ? <Popover.Close asChild><button type="button" className={moreItem} onClick={() => setAdding(true)}><UserPlus className="size-4 text-white/60" />Add people</button></Popover.Close> : null}
            {connectedNow ? <Popover.Close asChild><div><DeviceRecordMenuItem className={moreItem} /></div></Popover.Close> : null}
            <Popover.Close asChild><Link href={`/calling?call=${encodeURIComponent(media.callId)}`} onClick={() => setFull(false)} className={moreItem}><Info className="size-4 text-white/60" />Call details</Link></Popover.Close>
            <Popover.Close asChild><button type="button" className={moreItem} onClick={() => setReporting(true)}><Flag className="size-4 text-white/60" />Report a problem</button></Popover.Close>
          </Popover.Content>
        </Popover.Portal>
      </Popover.Root>
      {controls.upgrade && !video && inRoom && <CallUpgradeButton upgrade={() => mutation.mutateAsync({ operation: "upgrade", callId: media.callId, body: { kind: "VIDEO", expectedVersion: call.data!.version }, key: newIdempotencyKey() })} onError={setError} />}
      {/* CF2: still ringing (no room yet) — switch the call to video; the camera button appears once connected. */}
      {controls.upgrade && !video && !inRoom && <CallButton icon={<Video />} label="Video" disabled={mutation.isPending} onClick={() => void run(() => mutation.mutateAsync({ operation: "upgrade", callId: media.callId, body: { kind: "VIDEO", expectedVersion: call.data!.version }, key: newIdempotencyKey() }))} />}
      {inRoom && (controls.screenShare || presenting) && (
        <CallScreenShareButton presenting={presenting} onError={(e) => setError(callingErrorMessage(e))}
          onStart={() => screenShare("startScreenShare")} onStop={() => screenShare("stopScreenShare")} />
      )}
      {controls.hold && <CallButton icon={<Pause />} label="Hold" disabled={mutation.isPending} onClick={() => void command("hold")} />}
      {controls.resume && <CallButton icon={<Play />} label="Resume" tone="warn" disabled={mutation.isPending} onClick={() => void command("resume")} />}
    </>
  );
  const hangupButton = <CallButton icon={<PhoneOff />} label={hangUp.label} tone="danger" onClick={hangUp.run} />;
  const footerLinks = (
    <>
      {group && controls.end && !controls.cancel ? <button type="button" disabled={mutation.isPending} onClick={() => void command("end")} className="font-semibold text-red-300 hover:underline">End for everyone</button> : null}
    </>
  );

  const stage = (
    <Stage inRoom={inRoom} video={video} preview={placedVideo} group={group} seed={seed} counterpart={counterpart} status={status} ringing={ringing}
      connected={!!connectedNow} held={held} failed={mediaState.status === "failed"} error={error} onRetry={calling.retryMedia}
      onMinimise={() => setFull(false)} controls={stageControls} hangup={hangupButton} footerLinks={footerLinks} />
  );

  /* Minimised: a slim strip (tap to bring the call back). */
  const strip = (
    <div className="flex items-center gap-2">
      <button type="button" onClick={() => setFull(true)} className="flex min-w-0 flex-1 items-center gap-3 text-left" aria-label="Open call">
        <span className="relative flex size-10 shrink-0 items-center justify-center rounded-full text-sm font-semibold text-white" style={{ backgroundColor: colorFor(seed) }}>
          {group ? <Users className="size-5" /> : initials(counterpart)}
          <span className={cn("absolute -bottom-0.5 -right-0.5 size-3 rounded-full ring-2 ring-[#2D2E31]", dot)} />
        </span>
        <span className="min-w-0">
          <span className="block truncate text-sm font-semibold">{counterpart}</span>
          <span role="status" aria-live="polite" className="block truncate text-xs tabular-nums text-white/60">{status}</span>
        </span>
      </button>
      {inRoom && media.credential?.capabilities.canPublishAudio && <CallMicToggle />}
      <button type="button" onClick={hangUp.run} aria-label={hangUp.label} title={hangUp.label} disabled={mutation.isPending} className="flex size-10 shrink-0 items-center justify-center rounded-full bg-[#EA4335] text-white hover:bg-[#D93025]"><PhoneOff className="size-5" /></button>
      <button type="button" onClick={() => setFull(true)} aria-label="Full screen" title="Full screen" className="flex size-10 shrink-0 items-center justify-center rounded-full text-white/70 hover:bg-white/10 hover:text-white"><Maximize2 className="size-4" /></button>
    </div>
  );

  return (
    <section aria-label="Active call"
      className={cn("dark z-[60] text-white", full
        ? "fixed inset-0 bg-[#202124]"
        : "fixed bottom-20 right-3 w-[min(24rem,calc(100vw-1.5rem))] rounded-full bg-[#2D2E31] p-2 pl-2.5 shadow-2xl ring-1 ring-white/10 md:bottom-4")}>
      {/* One media connection for both layouts, so audio never drops when you minimise. */}
      {media.credential ? (
        <CallMedia key={media.credential.token} credential={media.credential} video={placedVideo} className={full ? "h-full" : undefined}
          onConnected={mediaEvents.connected} onDisconnected={mediaEvents.disconnected}
          onProviderReconnecting={mediaEvents.providerReconnecting} onProviderReconnected={mediaEvents.providerReconnected}
          onError={(e) => setError(callingErrorMessage(e))}>
          {full ? stage : strip}
        </CallMedia>
      ) : full ? stage : strip}
      <AbuseReportDialog callId={media.callId} open={reporting} onOpenChange={setReporting} />
      {call.data && controls.invite ? (
        <AddPeopleDialog callId={media.callId} version={call.data.version} principalId={calling.principalId} inviterParticipantId={me?.participantId ?? null}
          participantPrincipalIds={call.data.participants.map((p) => p.principalId)} open={adding} onOpenChange={setAdding} />
      ) : null}
    </section>
  );
}
