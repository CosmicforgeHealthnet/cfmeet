/** Device-only capture. Source tracks belong to the call and must never be stopped here. */
export type RecordingTrack = { track: MediaStreamTrack; label: string; muted: boolean };
export type LocalCapture = { stop: () => void; finished: Promise<Blob> };
export type ConsentRequest = { id: string; waiting: Set<string>; participants: Set<string>; starting: boolean };

export function receiveRecordingConsent(request: ConsentRequest | null, id: string, participant: string, action: string) {
  if (!request || request.id !== id || !request.participants.has(participant)) return "ignore";
  if (action === "decline") return "stop";
  if (action !== "allow" || !request.waiting.delete(participant)) return "ignore";
  return request.waiting.size ? "waiting" : "ready";
}

export async function startLocalCapture(getTracks: () => RecordingTrack[], onLimit: () => void): Promise<LocalCapture> {
  if (typeof MediaRecorder === "undefined" || !HTMLCanvasElement.prototype.captureStream) {
    throw new Error("Local recording is unavailable in this browser. Try an up-to-date desktop browser.");
  }
  const canvas = document.createElement("canvas");
  canvas.width = 1280; canvas.height = 720;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("This device cannot prepare the recording.");
  const audio = new AudioContext();
  const destination = audio.createMediaStreamDestination();
  const videos = new Map<MediaStreamTrack, HTMLVideoElement>();
  const sources = new Map<MediaStreamTrack, MediaStreamAudioSourceNode>();
  let timer: ReturnType<typeof setInterval> | undefined;
  let output: MediaStream | undefined;
  const clean = () => {
    clearInterval(timer);
    for (const v of videos.values()) { v.pause(); v.srcObject = null; }
    for (const s of sources.values()) s.disconnect();
    output?.getVideoTracks().forEach(t => t.stop());
    destination.stream.getTracks().forEach(t => t.stop());
    void audio.close();
  };
  try {
    await audio.resume();
    if (audio.state !== "running") throw new Error("Enable browser audio, then try recording again.");
    const draw = () => {
      const tracks = getTracks().filter(t => !t.muted && t.track.readyState === "live");
      const active = new Set(tracks.map(t => t.track));
      for (const [t, s] of sources) if (!active.has(t)) { s.disconnect(); sources.delete(t); }
      for (const [t, v] of videos) if (!active.has(t)) { v.pause(); v.srcObject = null; videos.delete(t); }
      for (const { track } of tracks) {
        if (track.kind === "audio" && !sources.has(track)) {
          const source = audio.createMediaStreamSource(new MediaStream([track]));
          source.connect(destination); sources.set(track, source);
        }
        if (track.kind === "video" && !videos.has(track)) {
          const v = document.createElement("video");
          v.muted = true; v.playsInline = true; v.srcObject = new MediaStream([track]);
          void v.play().catch(() => {}); videos.set(track, v);
        }
      }
      context.fillStyle = "#18181b"; context.fillRect(0, 0, 1280, 720);
      const tiles = tracks.filter(t => t.track.kind === "video");
      const columns = Math.ceil(Math.sqrt(tiles.length || 1));
      const rows = Math.ceil((tiles.length || 1) / columns);
      const w = 1280 / columns, h = 720 / rows;
      tiles.forEach(({ track, label }, i) => {
        const v = videos.get(track)!;
        const x = (i % columns) * w, y = Math.floor(i / columns) * h;
        if (v.readyState >= 2 && v.videoWidth) {
          const scale = Math.min(w / v.videoWidth, h / v.videoHeight);
          context.drawImage(v, x + (w - v.videoWidth * scale) / 2, y + (h - v.videoHeight * scale) / 2, v.videoWidth * scale, v.videoHeight * scale);
        }
        context.fillStyle = "#ffffff"; context.font = "18px sans-serif";
        context.fillText(label, x + 12, y + h - 15, w - 24);
      });
      if (!tiles.length) {
        context.fillStyle = "#ffffff"; context.font = "30px sans-serif";
        context.fillText("CF Meet · Audio recording", 50, 350);
      }
    };
    draw(); timer = setInterval(draw, 1000 / 15);
    output = new MediaStream([...canvas.captureStream(15).getVideoTracks(), ...destination.stream.getAudioTracks()]);
    const mimeType = ["video/webm;codecs=vp8,opus", "video/webm", "video/mp4"].find(t => MediaRecorder.isTypeSupported(t));
    if (!mimeType) throw new Error("This browser has no supported local recording format.");
    const recorder = new MediaRecorder(output, { mimeType, videoBitsPerSecond: 1_500_000, audioBitsPerSecond: 128_000 });
    const parts: Blob[] = [];
    let bytes = 0;
    const finished = new Promise<Blob>((resolve, reject) => {
      recorder.ondataavailable = event => {
        if (event.data.size) { parts.push(event.data); bytes += event.data.size; }
        // Bound memory on phones and laptops; save this file before starting another.
        if (bytes >= 256 * 1024 * 1024 && recorder.state !== "inactive") { recorder.stop(); onLimit(); }
      };
      recorder.onerror = () => {
        // Preserve any completed chunks even when capture is interrupted.
        if (parts.length) resolve(new Blob(parts, { type: recorder.mimeType }));
        else reject(new Error("Recording failed before any media could be saved."));
        clean();
      };
      recorder.onstop = () => { clean(); resolve(new Blob(parts, { type: recorder.mimeType })); };
    });
    recorder.start(1000);
    return { finished, stop: () => { if (recorder.state !== "inactive") recorder.stop(); } };
  } catch (error) { clean(); throw error; }
}

export function recordingFilename(type: string, label: string) {
  return `cfmeet-${label}-${new Date().toISOString().replace(/[:.]/g, "-")}.${type.includes("mp4") ? "mp4" : "webm"}`;
}
