"use client";

import { useState, useRef, useEffect, useCallback } from "react";

interface UseMediaStreamOptions {
  video?: boolean;
  audio?: boolean;
}

/** Why the preview has no camera/mic: blocked by the browser or no device found. */
export type MediaError = "denied" | "unavailable" | null;

function classify(err: unknown): MediaError {
  const name = (err as { name?: string })?.name;
  if (name === "NotAllowedError" || name === "SecurityError") return "denied";
  return "unavailable";
}

/**
 * Pre-join camera/mic preview. Camera and microphone are separate tracks: turning the
 * camera off releases only the camera (its light goes off) and the mic keeps working, so
 * the level meter stays live. Access failures are reported, not swallowed.
 */
export function useMediaStream(options: UseMediaStreamOptions = { video: true, audio: true }) {
  const [isCameraOn, setIsCameraOn] = useState(!!options.video);
  const [isMicOn, setIsMicOn] = useState(!!options.audio);
  const [stream, setStream] = useState<MediaStream | null>(null);
  const [error, setError] = useState<MediaError>(null);
  const [attempt, setAttempt] = useState(0);

  const videoRef = useRef<HTMLVideoElement>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const micOn = useRef(isMicOn);
  micOn.current = isMicOn;

  const publish = useCallback((tracks: MediaStreamTrack[]) => {
    const next = tracks.length ? new MediaStream(tracks) : null;
    streamRef.current = next;
    setStream(next);
  }, []);

  // First access: camera + microphone in ONE request, so the browser asks once.
  const [ready, setReady] = useState(false);
  const cameraWanted = useRef(isCameraOn);
  cameraWanted.current = isCameraOn;
  useEffect(() => {
    let cancelled = false;
    setReady(false);
    navigator.mediaDevices
      .getUserMedia({ audio: !!options.audio, video: cameraWanted.current })
      .then((s) => {
        if (cancelled) return s.getTracks().forEach((t) => t.stop());
        s.getAudioTracks().forEach((t) => (t.enabled = micOn.current));
        publish(s.getTracks());
        setError(null);
      })
      .catch((err) => {
        if (cancelled) return;
        setError(classify(err));
        setIsCameraOn(false);
      })
      .finally(() => !cancelled && setReady(true));
    return () => {
      cancelled = true;
      streamRef.current?.getTracks().forEach((t) => t.stop());
    };
  }, [options.audio, attempt, publish]);

  // Camera toggles after the first access: release fully when off (light goes out),
  // re-acquire only the camera when on (permission already granted, no new prompt).
  useEffect(() => {
    if (!ready) return;
    if (!isCameraOn) {
      streamRef.current?.getVideoTracks().forEach((t) => t.stop());
      publish(streamRef.current?.getAudioTracks() ?? []);
      return;
    }
    if (streamRef.current?.getVideoTracks().some((t) => t.readyState === "live")) return;
    let cancelled = false;
    navigator.mediaDevices
      .getUserMedia({ video: true })
      .then((s) => {
        if (cancelled) return s.getTracks().forEach((t) => t.stop());
        publish([...s.getVideoTracks(), ...(streamRef.current?.getAudioTracks() ?? [])]);
        setError(null);
      })
      .catch((err) => {
        if (cancelled) return;
        setError(classify(err));
        setIsCameraOn(false);
      });
    return () => {
      cancelled = true;
    };
  }, [isCameraOn, ready, publish]);

  // Attach the stream to the <video> whenever either changes (the element mounts with the camera).
  useEffect(() => {
    if (videoRef.current) videoRef.current.srcObject = stream;
  }, [stream, isCameraOn]);

  // Mute/unmute the mic track without releasing it.
  useEffect(() => {
    streamRef.current?.getAudioTracks().forEach((t) => (t.enabled = isMicOn));
  }, [isMicOn, stream]);

  // Release everything on unmount.
  useEffect(() => () => streamRef.current?.getTracks().forEach((t) => t.stop()), []);

  const toggleCamera = useCallback(() => setIsCameraOn((prev) => !prev), []);
  const toggleMic = useCallback(() => setIsMicOn((prev) => !prev), []);
  const retry = useCallback(() => {
    setError(null);
    setIsCameraOn(true);
    setAttempt((a) => a + 1);
  }, []);

  return { videoRef, streamRef, stream, isCameraOn, isMicOn, toggleCamera, toggleMic, error, retry };
}
