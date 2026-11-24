import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, statSync } from "node:fs";
import { stripTypeScriptTypes } from "node:module";
import { chromium } from "playwright";
import { receiveRecordingConsent } from "../src/lib/local-recording.ts";

test("local recording waits for every current participant and ignores foreign or stale consent", () => {
  const request = { id: "current", participants: new Set(["alice", "bob"]), waiting: new Set(["alice", "bob"]), starting: false };
  assert.equal(receiveRecordingConsent(request, "old", "alice", "allow"), "ignore");
  assert.equal(receiveRecordingConsent(request, "current", "stranger", "allow"), "ignore");
  assert.equal(receiveRecordingConsent(request, "current", "alice", "allow"), "waiting");
  assert.equal(receiveRecordingConsent(request, "current", "alice", "allow"), "ignore");
  assert.equal(receiveRecordingConsent(request, "current", "bob", "allow"), "ready");
  assert.equal(receiveRecordingConsent(request, "current", "alice", "decline"), "stop", "consent can be withdrawn after starting");
  assert.equal(receiveRecordingConsent(null, "current", "alice", "allow"), "ignore", "late consent cannot revive a cancelled recording");
});

test("device recording produces playable audio/video and preserves the session tracks", async () => {
  const browser = await chromium.launch({ args: ["--autoplay-policy=no-user-gesture-required"] });
  try {
    const page = await browser.newPage();
    const uploads = [];
    page.on("request", request => { if (/^https?:/.test(request.url())) uploads.push(request.url()); });
    const source = stripTypeScriptTypes(readFileSync(new URL("../src/lib/local-recording.ts", import.meta.url), "utf8"));
    await page.addScriptTag({ type: "module", content: source.replace("export async function startLocalCapture", "window.startLocalCapture = async function startLocalCapture").replace("export function recordingFilename", "window.recordingFilename = function recordingFilename").replace("export function offerRecordingAfterLeaving", "window.offerRecordingAfterLeaving = function offerRecordingAfterLeaving") });
    const result = await page.evaluate(async () => {
      const audio = new AudioContext(); await audio.resume();
      const destination = audio.createMediaStreamDestination();
      const oscillator = audio.createOscillator(); oscillator.frequency.value = 440; oscillator.connect(destination); oscillator.start();
      const canvas = document.createElement("canvas"); canvas.width = 320; canvas.height = 180;
      const ctx = canvas.getContext("2d");
      const timer = setInterval(() => { ctx.fillStyle = "#ff0000"; ctx.fillRect(0, 0, 320, 180); }, 50);
      const stream = canvas.captureStream(15);
      const videoTrack = stream.getVideoTracks()[0], audioTrack = destination.stream.getAudioTracks()[0];
      let tracks = [{ track: videoTrack, label: "Synthetic camera", muted: false }, { track: audioTrack, label: "Synthetic voice", muted: false }];
      const recorder = await window.startLocalCapture(() => tracks, () => { throw new Error("Unexpected memory limit"); });
      await new Promise(r => setTimeout(r, 1600));
      // Mid-recording mute/unmute and track replacement must not change the output stream.
      tracks = [];
      await new Promise(r => setTimeout(r, 250));
      tracks = [{ track: videoTrack, label: "Camera resumed", muted: false }, { track: audioTrack, label: "Voice resumed", muted: false }];
      await new Promise(r => setTimeout(r, 1600));
      recorder.stop(); recorder.stop();
      const blob = await recorder.finished;
      const url = URL.createObjectURL(blob), video = document.createElement("video"); video.src = url;
      await new Promise((resolve, reject) => { video.onloadeddata = resolve; video.onerror = reject; });
      const playback = audio.createMediaElementSource(video), analyser = audio.createAnalyser();
      playback.connect(analyser); analyser.connect(audio.destination);
      await video.play(); await new Promise(r => setTimeout(r, 1200));
      const samples = new Uint8Array(analyser.fftSize); analyser.getByteTimeDomainData(samples);
      const energy = samples.some(v => Math.abs(v - 128) > 3);
      const result = { bytes: blob.size, type: blob.type, width: video.videoWidth, height: video.videoHeight, energy,
        sourceVideo: videoTrack.readyState, sourceAudio: audioTrack.readyState, url, name: window.recordingFilename(blob.type, "meeting") };
      video.pause(); video.src = "";
      clearInterval(timer); stream.getTracks().forEach(t => t.stop()); oscillator.stop(); await audio.close();
      return result;
    });
    assert.ok(result.bytes > 1000);
    assert.match(result.type, /^video\/(webm|mp4)/);
    assert.deepEqual([result.width, result.height], [1280, 720]);
    assert.equal(result.energy, true, "recording must include audible media");
    assert.equal(result.sourceVideo, "live", "saving must not end the camera");
    assert.equal(result.sourceAudio, "live", "saving must not end the microphone");
    assert.match(result.name, /^cfmeet-meeting-.*\.(webm|mp4)$/);
    await page.evaluate(({ url, name }) => window.offerRecordingAfterLeaving(url, name), result);
    const [download] = await Promise.all([page.waitForEvent("download"), page.getByRole("link", { name: "Save recording", exact: true }).click()]);
    assert.equal(download.suggestedFilename(), result.name);
    assert.equal(statSync(await download.path()).size, result.bytes);
    assert.deepEqual(uploads, [], "recording and saving must send no HTTP requests");
    await page.getByRole("button", { name: "Dismiss", exact: true }).click();
    await page.close();
  } finally { await browser.close(); }
});
