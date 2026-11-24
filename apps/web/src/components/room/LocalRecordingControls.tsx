"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useRoomContext } from "@livekit/components-react";
import { ConnectionState, RoomEvent, type RemoteParticipant } from "livekit-client";
import { receiveRecordingConsent, recordingFilename, startLocalCapture, type ConsentRequest, type LocalCapture } from "@/lib/local-recording";

const TOPIC = "cfmeet.local-recording.v1";
type Notice = { id: string; name: string; recording: boolean; allowed: boolean };
type Message = { id: string; action: "request" | "allow" | "decline" | "start" | "stop"; to?: string };

/** Mounted for the whole media session, including when menus close or a call is minimised. */
export default function LocalRecordingControls({ label, canStart = true }: { label: "meeting" | "call"; canStart?: boolean }) {
  const room = useRoomContext();
  const [phase, setPhase] = useState<"idle" | "waiting" | "starting" | "recording">("idle");
  const [notices, setNotices] = useState<Record<string, Notice>>({});
  const [error, setError] = useState("");
  const [file, setFile] = useState<{ url: string; name: string } | null>(null);
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
      const recorder = await startLocalCapture(() => [room.localParticipant, ...room.remoteParticipants.values()].flatMap(p =>
        [...p.trackPublications.values()].flatMap(publication => publication.track ? [{
          track: publication.track.mediaStreamTrack, label: p.name || (p.isLocal ? "You" : "Participant"), muted: publication.isMuted,
        }] : [])), () => { setError("Recording reached the device memory limit. Save this file before recording another part."); stop(); });
      // Someone can decline, leave, or join while the audio device is preparing.
      if (pending.current !== request || !mounted.current) { recorder.stop(); void recorder.finished.catch(() => {}); return; }
      capture.current = recorder;
      void recorder.finished.then(blob => {
        if (!blob.size) throw new Error("No recording data was produced.");
        const url = URL.createObjectURL(blob), name = recordingFilename(blob.type, label);
        const link = document.createElement("a"); link.href = url; link.download = name;
        document.body.appendChild(link); link.click(); link.remove();
        if (mounted.current) setFile(previous => { if (previous) URL.revokeObjectURL(previous.url); return { url, name }; });
        // Also allow an automatic end-of-call download to finish after unmount.
        else setTimeout(() => URL.revokeObjectURL(url), 120_000);
      }).catch(e => { if (mounted.current) setError(e instanceof Error ? e.message : "Recording could not be saved."); }).finally(() => {
        if (pending.current === request) stop();
      });
      await send({ id: request.id, action: "start" });
      if (pending.current === request && mounted.current) setPhase("recording");
    } catch (e) {
      stop();
      if (mounted.current) setError(e instanceof Error ? e.message : "Local recording could not start.");
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
      const identity = participant.identity, request = pending.current;
      if (message.action === "request") {
        updateNotices({ ...noticeRef.current, [identity]: { id: message.id, name: participant.name || "A participant", recording: false, allowed: false } });
      } else if (message.action === "allow" || message.action === "decline") {
        const decision = receiveRecordingConsent(request, message.id, identity, message.action);
        if (decision === "ready") void begin();
        if (decision === "stop") { setError("A participant did not allow recording. Recording stopped."); stop(); }
      } else if (message.action === "start") {
        const previous = noticeRef.current[identity];
        if (previous?.id === message.id && previous.allowed) updateNotices({ ...noticeRef.current, [identity]: { ...previous, recording: true } });
      } else if (message.action === "stop" && noticeRef.current[identity]?.id === message.id) {
        const next = { ...noticeRef.current }; delete next[identity]; updateNotices(next);
      }
    };
    const membershipChanged = () => {
      if (pending.current) { setError("The participant list changed. Recording stopped; ask everyone again to restart."); stop(); }
      const next = { ...noticeRef.current };
      for (const id of Object.keys(next)) if (!room.remoteParticipants.has(id)) delete next[id];
      updateNotices(next);
    };
    const interrupted = () => { stop(); updateNotices({}); };
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

  const request = () => {
    if (pending.current || !canStart) return;
    setError("");
    if (room.state !== ConnectionState.Connected) { setError("Connect to the media session before recording."); return; }
    pending.current = { id: crypto.randomUUID(), waiting: new Set(room.remoteParticipants.keys()), participants: new Set(room.remoteParticipants.keys()), starting: false };
    setPhase("waiting");
    deadline.current = setTimeout(() => { setError("Recording did not start: everyone must allow it within 30 seconds."); stop(); }, 30_000);
    void send({ id: pending.current.id, action: "request" }).then(begin).catch(() => { setError("The recording request could not reach everyone."); stop(); });
  };
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
      setError("Your recording decision could not be sent. Please try again.");
    }
  };
  return <section aria-label="Device recording" className="relative z-[70] shrink-0 space-y-2 rounded-lg bg-zinc-900 p-2 text-xs text-white">
    <div className="flex flex-wrap items-center gap-2">
      {canStart && phase === "idle" ? <button type="button" className="rounded border border-white/40 px-3 py-2" onClick={request}>Record {label} to this device</button> : null}
      {phase !== "idle" ? <><span role="status">{phase === "waiting" ? "Waiting for everyone’s consent…" : phase === "starting" ? "Preparing local recording…" : "● Recording on this device"}</span><button type="button" className="rounded border px-3 py-2" onClick={stop}>{phase === "recording" ? "Stop & save" : "Cancel"}</button></> : null}
      <span>Saved to your device only. Keep this page open until the download finishes.</span>
      {file ? <a className="underline" href={file.url} download={file.name}>Save recording again</a> : null}
    </div>
    {Object.entries(notices).map(([identity, notice]) => <div key={identity} className="flex flex-wrap items-center gap-2" role="status">
      <span>{notice.name} {notice.recording ? "is recording on their device." : "wants to record on their device."}</span>
      {!notice.allowed ? <button type="button" className="rounded border px-3 py-2" onClick={() => void decide(identity, true)}>Allow</button> : null}
      <button type="button" className="rounded border px-3 py-2" onClick={() => void decide(identity, false)}>{notice.allowed ? "Withdraw consent" : "Don’t allow"}</button>
    </div>)}
    {error ? <p role="alert">{error}</p> : null}
  </section>;
}
