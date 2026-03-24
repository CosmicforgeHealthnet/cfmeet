"use client";

import { useState, useRef, useEffect, useCallback } from "react";

interface UseMediaStreamOptions {
  video?: boolean;
  audio?: boolean;
}

export function useMediaStream(
  options: UseMediaStreamOptions = { video: true, audio: true },
) {
  const [isCameraOn, setIsCameraOn] = useState(!!options.video);
  const [isMicOn, setIsMicOn] = useState(!!options.audio);
  const [stream, setStream] = useState<MediaStream | null>(null);

  const videoRef = useRef<HTMLVideoElement>(null);
  const streamRef = useRef<MediaStream | null>(null);

  const startStream = useCallback(async () => {
    try {
      const mediaStream = await navigator.mediaDevices.getUserMedia({
        video: true,
        audio: true,
      });
      streamRef.current = mediaStream;
      setStream(mediaStream);
      if (videoRef.current) {
        videoRef.current.srcObject = mediaStream;
      }
    } catch (err) {
      console.error("Failed to access media devices:", err);
    }
  }, []);

  const stopStream = useCallback(() => {
    if (streamRef.current) {
      streamRef.current.getTracks().forEach((track) => track.stop());
      streamRef.current = null;
      setStream(null);
    }
    if (videoRef.current) {
      videoRef.current.srcObject = null;
    }
  }, []);

  // Start/stop camera - sync with external media devices API
  useEffect(() => {
    if (isCameraOn) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
      startStream();
    } else {
      stopStream();
    }
    return () => {
      stopStream();
    };
  }, [isCameraOn, startStream, stopStream]);

  // Mute/unmute mic track
  useEffect(() => {
    if (streamRef.current) {
      streamRef.current.getAudioTracks().forEach((track) => {
        track.enabled = isMicOn;
      });
    }
  }, [isMicOn]);

  const toggleCamera = useCallback(() => {
    setIsCameraOn((prev) => !prev);
  }, []);

  const toggleMic = useCallback(() => {
    setIsMicOn((prev) => !prev);
  }, []);

  return {
    videoRef,
    streamRef,
    stream,
    isCameraOn,
    isMicOn,
    toggleCamera,
    toggleMic,
  };
}
