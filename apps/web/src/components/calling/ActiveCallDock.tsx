"use client";

/**
 * The active call, on every signed-in page (CAL-002, CAL-015): media connection, its surfaced
 * status (connecting / network interrupted / retrying in n s / could not restore) with an
 * automatic rejoin, and the controls the signed-in party may use — Leave for anyone, End for
 * all / Hold / Resume for any active participant (ADR 0009 party rules), Cancel for the caller
 * before the answer.
 *
 * Design (Google Meet / Duo call screen): a full-screen stage — big avatar with ripples while it
 * rings, a live timer once connected, their video full-bleed with yours in a corner window —
 * and one floating control pill. Minimise shrinks it to a strip so the app stays usable; the
 * media connection stays mounted either way so audio never drops.
 */
import { useEffect, useState } from "react";
import Link from "next/link";
import { ChevronDown, Maximize2, Pause, PhoneOff, Play, RefreshCw, Users } from "lucide-react";
import { useCalling } from "@/context/CallingContext";
import { useCall, useCallActions, useCallingMutation } from "@/hooks/calling/useCalls";
import { usePrincipalNames } from "@/hooks/calling/usePrincipalNames";
import { callingErrorMessage } from "@/lib/calling/errors";
import { newIdempotencyKey } from "@/lib/calling/idempotency";
import { callerLabel } from "@/lib/calling/incoming-call";
import { ownSession, visibleControls } from "@/lib/calling/call-permissions";
import { mediaStatusLabel } from "@/lib/calling/media-reconnect";
import { cn } from "@/lib/utils";
import { rememberedCallName } from "@/lib/calling/call-names";
import CallMedia, { CallCameraButton, CallMicButton, CallVideoStage, MutedTag, useAnyVideo } from "./CallMedia";

const COLORS = ["#2563eb", "#059669", "#d97706", "#7c3aed", "#db2777", "#0891b2", "#4f46e5", "#ea580c"];
const colorFor = (key: string) => COLORS[([...key].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % COLORS.length];
const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((w) => w[0]?.toUpperCase()).join("") || "?";
const clock = (ms: number) => {
  const t = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(t / 3600), m = Math.floor((t % 3600) / 60), s = t % 60;
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}` : `${m}:${String(s).padStart(2, "0")}`;
};
const RINGING = ["REQUESTED", "AUTHORIZED", "RESOLVING", "ROUTING", "DIALING", "RINGING"];

type StageProps = {
  inRoom: boolean;
  video: boolean;
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
export function StageView({ inRoom, video, group, seed, counterpart, status, ringing, connected, held, failed, error, onRetry, onMinimise, controls, footerLinks, anyVideo }: StageProps & { anyVideo: boolean }) {
  return (
    <div className="relative flex h-full flex-col overflow-hidden">
      {inRoom && video ? <CallVideoStage /> : null}
      {/* A soft glow in the caller's colour behind the avatar (only without video). */}
      {!anyVideo && <div aria-hidden className="pointer-events-none absolute left-1/2 top-[38%] size-[34rem] -translate-x-1/2 -translate-y-1/2 rounded-full opacity-25 blur-[120px]" style={{ backgroundColor: colorFor(seed) }} />}

      <header className={cn("relative z-10 flex items-center gap-2 p-4 sm:p-6", anyVideo && "bg-gradient-to-b from-black/60 to-transparent")}>
        <button type="button" onClick={onMinimise} aria-label="Minimise call" title="Minimise" className="flex size-11 items-center justify-center rounded-full bg-white/10 text-white backdrop-blur hover:bg-white/20"><ChevronDown className="size-5" /></button>
        <span className="ml-1 text-sm font-medium text-white/70">{group ? "Group call" : video ? "Video call" : "Voice call"}</span>
        {anyVideo && <span className="ml-auto rounded-full bg-black/40 px-3 py-1 text-sm font-semibold tabular-nums text-white backdrop-blur">{counterpart} · {status}</span>}
      </header>

      {!anyVideo ? (
        <div className="relative z-10 flex flex-1 flex-col items-center justify-center px-6 pb-16 text-center">
          <div className="relative flex items-center justify-center">
            {ringing && [0, 1, 2].map((i) => (
              <span key={i} aria-hidden className="absolute size-36 animate-ping rounded-full sm:size-40" style={{ backgroundColor: colorFor(seed), opacity: 0.2, animationDuration: "2.4s", animationDelay: `${i * 0.8}s` }} />
            ))}
            <span className="relative flex size-36 items-center justify-center rounded-full text-5xl font-semibold text-white shadow-2xl ring-4 ring-white/10 sm:size-40" style={{ backgroundColor: colorFor(seed) }}>
              {group ? <Users className="size-16" /> : initials(counterpart)}
            </span>
          </div>
          <h2 className="mt-8 max-w-xl truncate text-3xl font-semibold tracking-tight text-white sm:text-4xl">{counterpart}</h2>
          <p role="status" aria-live="polite" className={cn("mt-3 text-lg tabular-nums", connected && !held ? "text-white/90" : "text-white/60")}>{status}</p>
          <div className="mt-4 h-7">{inRoom && connected ? <MutedTag /> : null}</div>
          {failed ? (
            <button type="button" onClick={onRetry} className="mt-2 flex h-10 items-center gap-2 rounded-full bg-white/10 px-5 text-sm font-semibold text-white hover:bg-white/20"><RefreshCw className="size-4" />Reconnect</button>
          ) : null}
        </div>
      ) : <div className="flex-1" />}

      <footer className="relative z-10 flex flex-col items-center gap-3 p-6 pb-8">
        {error ? <p role="alert" className="rounded-full bg-red-500/20 px-4 py-2 text-sm text-red-100">{error}</p> : null}
        <div className="flex items-center gap-3 rounded-full bg-[#2D2E31]/90 p-2.5 shadow-2xl ring-1 ring-white/10 backdrop-blur-xl">{controls}</div>
        <div className="flex items-center gap-4 text-xs">{footerLinks}</div>
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
  const others = call.data?.participants.filter((p) => p.principalId !== calling?.principalId && !p.displayName) ?? [];
  const names = usePrincipalNames(others.map((p) => p.principalId));
  const [full, setFull] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const connectedNow = call.data?.state === "CONNECTED";
  useEffect(() => {
    if (!connectedNow) return;
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [connectedNow]);
  // Every new call opens full screen.
  useEffect(() => { if (media?.callId) setFull(true); }, [media?.callId]);
  if (!calling || !media) return null;

  const { mediaState, mediaEvents } = calling;
  const party = call.data?.participants.filter((p) => p.principalId !== calling.principalId) ?? [];
  const remembered = rememberedCallName(media.callId);
  const counterpart = call.data
    ? party.map((p) => p.displayName || names.data?.[p.principalId] || (party.length === 1 ? remembered : null) || "CF Meet user").join(", ") || callerLabel(call.data.participants, calling.principalId)
    : remembered ?? "Call";
  const seed = party[0]?.principalId ?? counterpart;
  const controls = call.data
    ? visibleControls(call.data, calling.principalId, (name) => !!actions.data?.actions.some((a) => a.action === name && a.available))
    : { cancel: false, end: false, hold: false, resume: false, transfer: false };
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
  const leave = () => run(async () => {
    if (session && call.data) {
      await mutation.mutateAsync({ operation: "removeSession", callId: media.callId, sessionId: session.sessionId, body: { expectedVersion: call.data.version }, key: newIdempotencyKey() });
    }
    calling.leaveMedia();
  });

  const state = call.data?.state;
  const held = state === "HELD";
  const ringing = !!state && (RINGING.includes(state) || state === "ANSWERED");
  const status = held ? "On hold"
    : state === "RINGING" ? "Ringing…"
    : state === "ANSWERED" ? "Connecting…"
    : ringing ? "Calling…"
    : elapsed !== null && mediaState.status === "connected" ? clock(elapsed)
    : mediaStatusLabel(mediaState);
  const dot = mediaState.status === "connected" && !held && !ringing ? "bg-emerald-400" : mediaState.status === "failed" ? "bg-red-500" : "animate-pulse bg-amber-400";
  const video = media.kind === "VIDEO";
  const hangUp = controls.cancel
    ? { label: "Cancel", run: () => void command("cancel") }
    : controls.end && !group ? { label: "End call", run: () => void command("end") } : { label: "Leave", run: () => void leave() };
  const pill = "flex size-14 items-center justify-center rounded-full bg-white/12 text-white transition-colors hover:bg-white/20 disabled:opacity-40 [&_svg]:size-6";
  const inRoom = !!media.credential;

  const stageControls = (
    <>
      {inRoom && <CallMicButton allowed={!!media.credential?.capabilities.canPublishAudio} />}
      {inRoom && video && <CallCameraButton allowed={!!media.credential?.capabilities.canPublishVideo} />}
      {controls.hold && <button type="button" aria-label="Hold" title="Put on hold" disabled={mutation.isPending} onClick={() => void command("hold")} className={pill}><Pause /></button>}
      {controls.resume && <button type="button" aria-label="Resume" title="Resume" disabled={mutation.isPending} onClick={() => void command("resume")} className={cn(pill, "bg-[#FDD663] text-[#3A2A00] hover:bg-[#FDE293]")}><Play /></button>}
      <button type="button" disabled={mutation.isPending} onClick={hangUp.run} aria-label={hangUp.label} title={hangUp.label}
        className="flex h-14 items-center gap-2 rounded-full bg-[#EA4335] px-6 text-sm font-semibold text-white transition-colors hover:bg-[#D93025] disabled:opacity-50 [&_svg]:size-6">
        <PhoneOff />{hangUp.label}
      </button>
    </>
  );
  const footerLinks = (
    <>
      {group && controls.end && !controls.cancel ? <button type="button" disabled={mutation.isPending} onClick={() => void command("end")} className="font-semibold text-red-300 hover:underline">End for everyone</button> : null}
      <Link href={`/calling?call=${encodeURIComponent(media.callId)}`} onClick={() => setFull(false)} className="font-medium text-white/50 hover:text-white/80">Call details</Link>
    </>
  );

  const stage = (
    <Stage inRoom={inRoom} video={video} group={group} seed={seed} counterpart={counterpart} status={status} ringing={ringing}
      connected={!!connectedNow} held={held} failed={mediaState.status === "failed"} error={error} onRetry={calling.retryMedia}
      onMinimise={() => setFull(false)} controls={stageControls} footerLinks={footerLinks} />
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
      {inRoom && <CallMicButton small allowed={!!media.credential?.capabilities.canPublishAudio} />}
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
        <CallMedia key={media.credential.token} credential={media.credential} video={video} className={full ? "h-full" : undefined}
          onConnected={mediaEvents.connected} onDisconnected={mediaEvents.disconnected}
          onProviderReconnecting={mediaEvents.providerReconnecting} onProviderReconnected={mediaEvents.providerReconnected}
          onError={(e) => setError(callingErrorMessage(e))}>
          {full ? stage : strip}
        </CallMedia>
      ) : full ? stage : strip}
    </section>
  );
}
