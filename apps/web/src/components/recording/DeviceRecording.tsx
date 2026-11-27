"use client";

/**
 * Record a call or meeting to this device (never uploaded to CF Meet).
 *
 * Everyone present must tap Allow first (30 seconds to answer). Recording stops when anyone
 * withdraws, someone joins or leaves, or the connection drops; starting again asks everyone
 * again. Other people see a red "is recording" notice the whole time. The file downloads when
 * you stop, with "Save again" in case the download was blocked.
 *
 * Consent messages travel over the media room (topic `cfmeet.local-recording.v1`), as agreed
 * with the backend; the server only grants the right to send them (`canPublishData`).
 *
 * Mount <DeviceRecordingProvider> inside the LiveKit room for the whole session, then put
 * <DeviceRecordButton> (meeting bar) or <DeviceRecordMenuItem> (call menu) where it fits.
 */
import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";
import { useRoomContext } from "@livekit/components-react";
import { ConnectionState, RoomEvent, type RemoteParticipant } from "livekit-client";
import { Circle, Download, Loader2, Square, X } from "lucide-react";
import { offerRecordingAfterLeaving, receiveRecordingConsent, recordingFilename, startLocalCapture, type ConsentRequest, type LocalCapture } from "@/lib/local-recording";
import { cn } from "@/lib/utils";

const TOPIC = "cfmeet.local-recording.v1";
const ASK_MS = 30_000;
type Phase = "idle" | "waiting" | "starting" | "recording";
type Notice = { id: string; name: string; recording: boolean; allowed: boolean };
type Message = { id: string; action: "request" | "allow" | "decline" | "start" | "stop"; to?: string };
type Ctx = { available: boolean; phase: Phase; start: () => void; stop: () => void };

const RecordingContext = createContext<Ctx>({ available: false, phase: "idle", start: () => {}, stop: () => {} });
export const useDeviceRecording = () => useContext(RecordingContext);

const clock = (ms: number) => {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const mm = String(Math.floor((s % 3600) / 60)).padStart(h ? 2 : 1, "0");
  return `${h ? `${h}:` : ""}${mm}:${String(s % 60).padStart(2, "0")}`;
};

export function DeviceRecordingProvider({ label, canStart, children }: { label: "meeting" | "call"; canStart: boolean; children: React.ReactNode }) {
  const room = useRoomContext();
  const [phase, setPhase] = useState<Phase>("idle");
  const [notices, setNotices] = useState<Record<string, Notice>>({});
  const [error, setError] = useState("");
  const [file, setFile] = useState<{ url: string; name: string } | null>(null);
  const [since, setSince] = useState(0);
  const [now, setNow] = useState(0);
  const capture = useRef<LocalCapture | null>(null);
  const pending = useRef<ConsentRequest | null>(null);
  const noticeRef = useRef<Record<string, Notice>>({});
  const deadline = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const mounted = useRef(true);

  const send = useCallback((message: Message) => room.localParticipant.publishData(
    new TextEncoder().encode(JSON.stringify(message)), { reliable: true, topic: TOPIC, destinationIdentities: message.to ? [message.to] : undefined },
  ), [room]);
  const updateNotices = useCallback((next: Record<string, Notice>) => {
    noticeRef.current = next;
    if (mounted.current) setNotices(next);
  }, []);
  const stop = useCallback(() => {
    const id = pending.current?.id;
    pending.current = null;
    clearTimeout(deadline.current);
    capture.current?.stop();
    capture.current = null;
    if (mounted.current) setPhase("idle");
    if (id) void send({ id, action: "stop" }).catch(() => {});
  }, [send]);

  const begin = useCallback(async () => {
    const request = pending.current;
    if (!request || request.starting || request.waiting.size) return;
    request.starting = true;
    clearTimeout(deadline.current);
    setPhase("starting");
    try {
      const recorder = await startLocalCapture(() => [room.localParticipant, ...room.remoteParticipants.values()].flatMap((p) =>
        [...p.trackPublications.values()].flatMap((publication) => publication.track ? [{
          track: publication.track.mediaStreamTrack, label: p.name || (p.isLocal ? "You" : "Participant"), muted: publication.isMuted,
        }] : [])), () => { setError("The recording got too big for this device, so it stopped and saved. Start again to record the next part."); stop(); });
      // Someone can decline, leave or join while the recorder is getting ready.
      if (pending.current !== request || !mounted.current) { recorder.stop(); void recorder.finished.catch(() => {}); return; }
      capture.current = recorder;
      void recorder.finished.then((blob) => {
        if (!blob.size) throw new Error("Nothing was recorded.");
        const url = URL.createObjectURL(blob);
        const name = recordingFilename(blob.type, label);
        const link = document.createElement("a");
        link.href = url; link.download = name;
        document.body.appendChild(link); link.click(); link.remove();
        if (mounted.current) setFile((previous) => { if (previous) URL.revokeObjectURL(previous.url); return { url, name }; });
        else offerRecordingAfterLeaving(url, name);
      }).catch((e) => { if (mounted.current) setError(e instanceof Error ? e.message : "The recording couldn't be saved."); }).finally(() => {
        if (pending.current === request) stop();
      });
      await send({ id: request.id, action: "start" });
      if (pending.current === request && mounted.current) { setSince(Date.now()); setNow(Date.now()); setPhase("recording"); }
    } catch (e) {
      stop();
      if (mounted.current) setError(e instanceof Error ? e.message : "Recording couldn't start.");
    }
  }, [room, label, send, stop]);

  useEffect(() => {
    mounted.current = true;
    const receive = (bytes: Uint8Array, participant?: RemoteParticipant, _kind?: unknown, topic?: string) => {
      if (topic !== TOPIC || !participant || bytes.length > 2048) return;
      let message: Message;
      try { message = JSON.parse(new TextDecoder().decode(bytes)); } catch { return; }
      if (typeof message.id !== "string" || !/^[a-f0-9-]{36}$/.test(message.id)) return;
      if (message.to && message.to !== room.localParticipant.identity) return;
      const identity = participant.identity;
      if (message.action === "request") {
        updateNotices({ ...noticeRef.current, [identity]: { id: message.id, name: participant.name || "Someone", recording: false, allowed: false } });
      } else if (message.action === "allow" || message.action === "decline") {
        const decision = receiveRecordingConsent(pending.current, message.id, identity, message.action);
        if (decision === "ready") void begin();
        if (decision === "stop") { setError(`${participant.name || "Someone"} didn't allow recording.`); stop(); }
      } else if (message.action === "start") {
        const previous = noticeRef.current[identity];
        if (previous?.id === message.id && previous.allowed) updateNotices({ ...noticeRef.current, [identity]: { ...previous, recording: true } });
      } else if (message.action === "stop" && noticeRef.current[identity]?.id === message.id) {
        const next = { ...noticeRef.current };
        delete next[identity];
        updateNotices(next);
      }
    };
    const membershipChanged = () => {
      if (pending.current) { setError("Someone joined or left, so recording stopped and was saved. Start again to ask everyone."); stop(); }
      const next = { ...noticeRef.current };
      for (const id of Object.keys(next)) if (!room.remoteParticipants.has(id)) delete next[id];
      updateNotices(next);
    };
    const interrupted = () => {
      if (pending.current) setError("The connection dropped, so recording stopped and was saved.");
      stop();
      updateNotices({});
    };
    const unload = (event: BeforeUnloadEvent) => { if (pending.current) { event.preventDefault(); event.returnValue = ""; } };
    room.on(RoomEvent.DataReceived, receive).on(RoomEvent.ParticipantConnected, membershipChanged).on(RoomEvent.ParticipantDisconnected, membershipChanged)
      .on(RoomEvent.Reconnecting, interrupted).on(RoomEvent.Disconnected, interrupted);
    window.addEventListener("beforeunload", unload);
    return () => {
      mounted.current = false;
      stop();
      room.off(RoomEvent.DataReceived, receive).off(RoomEvent.ParticipantConnected, membershipChanged).off(RoomEvent.ParticipantDisconnected, membershipChanged)
        .off(RoomEvent.Reconnecting, interrupted).off(RoomEvent.Disconnected, interrupted);
      window.removeEventListener("beforeunload", unload);
    };
  }, [room, begin, stop, updateNotices]);
  useEffect(() => () => { if (file) URL.revokeObjectURL(file.url); }, [file]);
  // Lost permission (e.g. the host turned recording off) stops it at once.
  useEffect(() => { if (!canStart && pending.current) stop(); }, [canStart, stop]);
  useEffect(() => {
    if (phase === "idle") return;
    const t = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(t);
  }, [phase]);

  const start = useCallback(() => {
    if (pending.current || !canStart) return;
    setError("");
    if (typeof MediaRecorder === "undefined" || typeof HTMLCanvasElement === "undefined" || !HTMLCanvasElement.prototype.captureStream) {
      setError("This browser can't record. Try the latest Chrome, Edge or Firefox on a computer.");
      return;
    }
    if (room.state !== ConnectionState.Connected) { setError("Wait until you're connected, then try again."); return; }
    const others = new Set(room.remoteParticipants.keys());
    pending.current = { id: crypto.randomUUID(), waiting: new Set(others), participants: others, starting: false };
    const t = Date.now();
    setSince(t); setNow(t);
    setPhase("waiting");
    deadline.current = setTimeout(() => { setError("Not everyone allowed recording in time, so it didn't start."); stop(); }, ASK_MS);
    void send({ id: pending.current.id, action: "request" }).then(begin).catch(() => { setError("Couldn't ask everyone. Check your connection and try again."); stop(); });
  }, [canStart, room, send, begin, stop]);

  const decide = async (identity: string, allow: boolean) => {
    const notice = noticeRef.current[identity];
    if (!notice) return;
    if (allow) updateNotices({ ...noticeRef.current, [identity]: { ...notice, allowed: true } });
    try {
      await send({ id: notice.id, action: allow ? "allow" : "decline", to: identity });
      if (noticeRef.current[identity]?.id !== notice.id) return;
      const next = { ...noticeRef.current };
      if (!allow) delete next[identity];
      updateNotices(next);
    } catch {
      if (noticeRef.current[identity]?.id === notice.id) updateNotices({ ...noticeRef.current, [identity]: { ...notice, allowed: false } });
      setError("Your answer didn't go through. Try again.");
    }
  };

  const pill = "pointer-events-auto flex max-w-full items-center gap-2 rounded-full bg-[#2D2E31]/95 py-1.5 pl-3.5 pr-1.5 text-sm text-white shadow-2xl ring-1 ring-white/10 backdrop-blur";
  const small = "h-8 shrink-0 rounded-full px-3 text-sm font-semibold";
  const asking = Object.entries(notices).filter(([, n]) => !n.allowed);
  const others = Object.entries(notices).filter(([, n]) => n.allowed);
  const waitLeft = Math.ceil(Math.max(0, ASK_MS - (now - since)) / 1000);

  return (
    <RecordingContext.Provider value={{ available: canStart, phase, start, stop }}>
      {children}
      <div className="dark pointer-events-none fixed inset-x-0 top-3 z-[85] flex flex-col items-center gap-2 px-3" aria-live="polite" data-testid="device-recording">
        {phase === "waiting" || phase === "starting" ? (
          <div className={pill} role="status">
            <Loader2 className="size-4 shrink-0 animate-spin text-white/70" />
            <span className="truncate">{phase === "waiting" ? `Asking everyone to allow recording… ${waitLeft}s` : "Starting recording…"}</span>
            <button type="button" onClick={stop} className={cn(small, "text-white/80 hover:bg-white/10")}>Cancel</button>
          </div>
        ) : phase === "recording" ? (
          <div className={pill} role="status" data-testid="recording-on">
            <span className="size-2.5 shrink-0 animate-pulse rounded-full bg-red-500" />
            <span className="truncate">Recording to this device <span className="tabular-nums text-white/70">· {clock(now - since)}</span></span>
            <button type="button" onClick={stop} className={cn(small, "flex items-center gap-1.5 bg-red-500 text-white hover:bg-red-600")}><Square className="size-3 fill-current" />Stop &amp; save</button>
          </div>
        ) : file ? (
          <div className={pill} role="status">
            <Download className="size-4 shrink-0 text-emerald-400" />
            <span className="truncate">Recording saved to your downloads</span>
            <a href={file.url} download={file.name} className={cn(small, "flex items-center text-[#8AB4FF] hover:bg-white/10")}>Save again</a>
            <button type="button" aria-label="Dismiss" onClick={() => setFile(null)} className="flex size-8 shrink-0 items-center justify-center rounded-full text-white/70 hover:bg-white/10"><X className="size-4" /></button>
          </div>
        ) : null}

        {asking.map(([identity, n]) => (
          <div key={identity} role="alertdialog" aria-label="Recording request" className="pointer-events-auto w-[min(26rem,100%)] rounded-2xl bg-[#2D2E31] p-4 text-white shadow-2xl ring-1 ring-white/10">
            <p className="flex items-center gap-2 font-semibold"><Circle className="size-4 shrink-0 fill-red-500 text-red-500" />{n.name} wants to record this {label}</p>
            <p className="mt-1 text-sm text-white/70">The recording is saved on their device only. Recording starts only if everyone allows it.</p>
            <div className="mt-3 flex justify-end gap-2">
              <button type="button" onClick={() => void decide(identity, false)} className="h-9 rounded-full px-4 text-sm font-semibold text-white/85 hover:bg-white/10">Don&apos;t allow</button>
              <button type="button" onClick={() => void decide(identity, true)} className="h-9 rounded-full bg-[#8AB4FF] px-4 text-sm font-semibold text-[#0A2A5E] hover:bg-[#A8C7FA]">Allow</button>
            </div>
          </div>
        ))}
        {others.map(([identity, n]) => (
          <div key={identity} className={pill} role="status">
            <span className={cn("size-2.5 shrink-0 rounded-full", n.recording ? "animate-pulse bg-red-500" : "bg-white/50")} />
            <span className="truncate">{n.recording ? `${n.name} is recording this ${label}` : `Waiting for ${n.name} to start recording`}</span>
            <button type="button" onClick={() => void decide(identity, false)} className={cn(small, "text-white/80 hover:bg-white/10")}>Stop allowing</button>
          </div>
        ))}

        {error ? (
          <div className={cn(pill, "bg-red-950/95 ring-red-400/30")} role="alert">
            <span className="min-w-0 py-1">{error}</span>
            <button type="button" aria-label="Dismiss" onClick={() => setError("")} className="flex size-8 shrink-0 items-center justify-center rounded-full text-white/70 hover:bg-white/10"><X className="size-4" /></button>
          </div>
        ) : null}
      </div>
    </RecordingContext.Provider>
  );
}

/** Meeting toolbar button (same look as the camera / mic buttons). */
export function DeviceRecordButton() {
  const { available, phase, start, stop } = useDeviceRecording();
  if (!available) return null;
  const on = phase !== "idle";
  const text = on ? (phase === "recording" ? "Stop recording" : "Cancel recording") : "Record to this device";
  return (
    <button type="button" className="room-control-button" data-active={on || undefined} aria-pressed={on} aria-label={text} title={text}
      onClick={on ? stop : start} data-testid="record-device">
      {on ? <Square className="fill-current text-red-400" /> : <Circle className="text-red-400" />}
    </button>
  );
}

/** Call "More" menu item. */
export function DeviceRecordMenuItem({ className }: { className: string }) {
  const { available, phase, start, stop } = useDeviceRecording();
  if (!available) return null;
  const on = phase !== "idle";
  return (
    <button type="button" className={className} onClick={on ? stop : start} data-testid="record-device">
      {on ? <Square className="size-4 fill-red-400 text-red-400" /> : <Circle className="size-4 text-red-400" />}
      {on ? (phase === "recording" ? "Stop & save recording" : "Cancel recording") : "Record to this device"}
    </button>
  );
}
