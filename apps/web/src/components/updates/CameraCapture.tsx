"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { Camera, Images, Loader2, SwitchCamera, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { formatClock } from "@/lib/media-editor/audio";

/** Longest camera recording (longer clips are split into 60 s statuses by the video editor). */
const MAX_RECORDING_MS = 3 * 60_000;
const VIDEO_TYPES = ["video/mp4;codecs=avc1,mp4a", "video/mp4", "video/webm;codecs=vp9,opus", "video/webm;codecs=vp8,opus", "video/webm"];

/**
 * In-app camera for statuses: live preview (front / back), tap the shutter for a photo, switch
 * to Video and tap to start / stop recording (at most 3 minutes). Where the browser has no
 * camera access (or permission is refused) it offers the phone's own camera app through a
 * `capture` file input, and the gallery.
 */
export default function CameraCapture({ onCaptured, onClose, onGallery }: {
  onCaptured: (file: File) => void;
  onClose: () => void;
  onGallery: () => void;
}) {
  const video = useRef<HTMLVideoElement>(null);
  const native = useRef<HTMLInputElement>(null);
  const [stream, setStream] = useState<MediaStream | null>(null);
  const [facing, setFacing] = useState<"user" | "environment">("environment");
  const [mode, setMode] = useState<"photo" | "video">("photo");
  const supported = typeof navigator !== "undefined" && !!navigator.mediaDevices?.getUserMedia;
  const [failedState, setFailed] = useState<string | null>(null);
  const failed = supported ? failedState : "This browser can't open the camera here.";
  const [starting, setStarting] = useState(supported);
  const [recording, setRecording] = useState<{ started: number } | null>(null);
  const [elapsed, setElapsed] = useState(0);
  const [flash, setFlash] = useState(false);
  const recorder = useRef<MediaRecorder | null>(null);

  // (Re)open the camera for the chosen side; audio is requested too so recording starts at once.
  useEffect(() => {
    if (!supported) return;
    let live = true;
    let opened: MediaStream | null = null;
    const constraints = { facingMode: { ideal: facing }, width: { ideal: 1920 }, height: { ideal: 1080 } };
    navigator.mediaDevices.getUserMedia({ video: constraints, audio: true })
      .catch(() => navigator.mediaDevices.getUserMedia({ video: constraints, audio: false }))
      .then((s) => {
        if (!live) { s.getTracks().forEach((t) => t.stop()); return; }
        opened = s;
        setStream(s);
        setFailed(null);
      }, (e: unknown) => {
        if (!live) return;
        const name = e instanceof DOMException ? e.name : "";
        setFailed(name === "NotAllowedError" ? "Camera access was refused. Allow it in your browser's site settings, or use your camera app." : "The camera couldn't be opened.");
      })
      .finally(() => { if (live) setStarting(false); });
    return () => { live = false; opened?.getTracks().forEach((t) => t.stop()); };
  }, [facing, supported]);

  useEffect(() => {
    const el = video.current;
    if (!el || !stream) return;
    el.srcObject = stream;
    void el.play().catch(() => {});
  }, [stream]);

  const stopRecording = useCallback(() => {
    const r = recorder.current;
    if (r && r.state !== "inactive") r.stop();
  }, []);

  useEffect(() => {
    if (!recording) return;
    const t = setInterval(() => {
      const e = Date.now() - recording.started;
      setElapsed(e);
      if (e >= MAX_RECORDING_MS) stopRecording();
    }, 200);
    return () => clearInterval(t);
  }, [recording, stopRecording]);

  const takePhoto = async () => {
    const el = video.current;
    if (!el || !el.videoWidth) return;
    const c = document.createElement("canvas");
    c.width = el.videoWidth;
    c.height = el.videoHeight;
    const ctx = c.getContext("2d");
    if (!ctx) return;
    // The preview of the front camera is mirrored; the photo is saved as the camera sees.
    ctx.drawImage(el, 0, 0);
    setFlash(true);
    setTimeout(() => setFlash(false), 150);
    const blob = await new Promise<Blob | null>((r) => c.toBlob(r, "image/jpeg", 0.92));
    if (blob) onCaptured(new File([blob], `photo-${Date.now()}.jpg`, { type: "image/jpeg", lastModified: Date.now() }));
  };

  const startRecording = () => {
    if (!stream || typeof MediaRecorder === "undefined") { setFailed("This browser can't record video here."); return; }
    const type = VIDEO_TYPES.find((t) => MediaRecorder.isTypeSupported(t));
    const r = new MediaRecorder(stream, { ...(type ? { mimeType: type } : {}), videoBitsPerSecond: 4_000_000 });
    const chunks: Blob[] = [];
    r.ondataavailable = (e) => { if (e.data.size) chunks.push(e.data); };
    r.onstop = () => {
      setRecording(null);
      recorder.current = null;
      const mime = (r.mimeType || type || "video/webm").split(";")[0];
      const blob = new Blob(chunks, { type: mime });
      if (blob.size) onCaptured(new File([blob], `video-${Date.now()}.${mime.includes("mp4") ? "mp4" : "webm"}`, { type: mime, lastModified: Date.now() }));
    };
    recorder.current = r;
    r.start(1000);
    setElapsed(0);
    setRecording({ started: Date.now() });
  };

  const shutter = () => {
    if (mode === "photo") void takePhoto();
    else if (recording) stopRecording();
    else startRecording();
  };

  return (
    <div className="fixed inset-0 z-[80] flex flex-col bg-black text-white" role="dialog" aria-modal="true" aria-label="Camera" data-testid="status-camera">
      <input ref={native} type="file" accept={mode === "video" ? "video/*" : "image/*"} capture="environment" className="hidden"
        onChange={(e) => { const f = e.target.files?.[0]; e.target.value = ""; if (f) onCaptured(f); }} />
      <div className="relative min-h-0 flex-1 overflow-hidden">
        {stream ? (
          <video ref={video} muted playsInline autoPlay aria-label="Camera preview" className={cn("absolute inset-0 size-full object-cover", facing === "user" && "-scale-x-100")} />
        ) : null}
        {flash ? <div className="absolute inset-0 bg-white/80" /> : null}
        {starting ? <Loader2 className="absolute left-1/2 top-1/2 size-10 -translate-x-1/2 -translate-y-1/2 animate-spin text-white/70" aria-label="Opening camera" /> : null}
        {failed && !stream ? (
          <div className="absolute inset-0 flex flex-col items-center justify-center gap-4 p-8 text-center">
            <Camera className="size-12 text-white/60" />
            <p className="max-w-xs text-sm text-white/80" role="alert">{failed}</p>
            <button type="button" onClick={() => native.current?.click()} className="h-12 rounded-full bg-primary px-6 font-semibold text-primary-foreground" data-testid="camera-native">Use camera app</button>
          </div>
        ) : null}
        <div className="absolute inset-x-0 top-0 flex items-center justify-between bg-gradient-to-b from-black/50 to-transparent px-3 pb-6 pt-[max(env(safe-area-inset-top),0.75rem)]">
          <button type="button" aria-label="Close camera" onClick={() => { stopRecording(); onClose(); }} className="flex size-11 items-center justify-center rounded-full bg-black/40 backdrop-blur"><X className="size-6" /></button>
          {recording ? <span className="flex items-center gap-2 rounded-full bg-red-600 px-3 py-1 text-sm font-semibold tabular-nums" role="timer" aria-live="off"><span className="size-2 animate-pulse rounded-full bg-white" />{formatClock(elapsed)}</span> : null}
          <button type="button" onClick={() => native.current?.click()} className="h-9 rounded-full bg-black/40 px-3 text-xs font-semibold backdrop-blur">Camera app</button>
        </div>
      </div>

      <div className="space-y-3 bg-black px-4 pt-3 pb-[max(env(safe-area-inset-bottom),1rem)]">
        <div className="flex justify-center gap-1" role="radiogroup" aria-label="Camera mode">
          {(["photo", "video"] as const).map((m) => (
            <button key={m} type="button" role="radio" aria-checked={mode === m} disabled={!!recording} onClick={() => setMode(m)}
              className={cn("h-9 rounded-full px-4 text-sm font-semibold capitalize", mode === m ? "bg-white text-black" : "text-white/80 hover:bg-white/10")}>{m}</button>
          ))}
        </div>
        <div className="flex items-center justify-between">
          <button type="button" aria-label="Gallery" onClick={onGallery} disabled={!!recording} className="flex size-12 items-center justify-center rounded-full bg-white/10 hover:bg-white/20 disabled:opacity-40"><Images className="size-6" /></button>
          <button type="button" onClick={shutter} disabled={!stream} data-testid="camera-shutter"
            aria-label={mode === "photo" ? "Take photo" : recording ? "Stop recording" : "Start recording"}
            className="flex size-20 items-center justify-center rounded-full border-4 border-white disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary focus-visible:ring-offset-2 focus-visible:ring-offset-black">
            <span className={cn("block transition-all", mode === "photo" ? "size-16 rounded-full bg-white" : recording ? "size-8 rounded-md bg-red-600" : "size-16 rounded-full bg-red-600")} />
          </button>
          <button type="button" aria-label="Switch camera" onClick={() => { setStarting(true); setFacing(facing === "user" ? "environment" : "user"); }} disabled={!!recording || !stream}
            className="flex size-12 items-center justify-center rounded-full bg-white/10 hover:bg-white/20 disabled:opacity-40"><SwitchCamera className="size-6" /></button>
        </div>
      </div>
    </div>
  );
}
