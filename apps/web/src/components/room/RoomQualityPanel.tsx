"use client";

import { useState } from "react";
import { useRoomContext } from "@livekit/components-react";
import { Button } from "@/components/ui/button";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { useConferenceQuality, useSubmitQualitySamples } from "@/hooks/v2/useQuality";
import type { QualitySample } from "@/types/v2";

type StatsWithMetrics = RTCStats & Record<string, unknown>;

function metric(stat: StatsWithMetrics, key: string): number | undefined {
  const value = stat[key];
  return typeof value === "number" && Number.isFinite(value) && value >= 0 ? value : undefined;
}

async function collectSample(room: ReturnType<typeof useRoomContext>): Promise<QualitySample> {
  const publications = [
    ...room.localParticipant.audioTrackPublications.values(),
    ...room.localParticipant.videoTrackPublications.values(),
  ];
  const reports = await Promise.all(
    publications.map(publication => publication.track?.getRTCStatsReport()),
  );
  const stats = reports.flatMap(report => report ? Array.from(report.values()) : []) as StatsWithMetrics[];
  const candidatePair = stats.find(item => item.type === "candidate-pair" && item.state === "succeeded");
  const inbound = stats.find(item => item.type === "inbound-rtp");
  const video = stats.find(item => item.type === "outbound-rtp" && item.kind === "video");
  const received = inbound ? metric(inbound, "packetsReceived") : undefined;
  const lost = inbound ? metric(inbound, "packetsLost") : undefined;
  const lossRatio = received !== undefined && lost !== undefined && received + lost > 0
    ? lost / (received + lost)
    : undefined;
  const currentRoundTripTime = candidatePair ? metric(candidatePair, "currentRoundTripTime") : undefined;
  const jitter = inbound ? metric(inbound, "jitter") : undefined;
  const width = video ? metric(video, "frameWidth") : undefined;
  const height = video ? metric(video, "frameHeight") : undefined;
  const fps = video ? metric(video, "framesPerSecond") : undefined;
  const sample: QualitySample = {
    timestamp: new Date().toISOString(),
    ...(currentRoundTripTime !== undefined ? { rttMs: currentRoundTripTime * 1000 } : {}),
    ...(jitter !== undefined ? { jitterMs: jitter * 1000 } : {}),
    ...(lossRatio !== undefined ? { packetLossRatio: lossRatio } : {}),
    ...(width !== undefined ? { width } : {}),
    ...(height !== undefined ? { height } : {}),
    ...(fps !== undefined ? { fps } : {}),
  };
  if (Object.keys(sample).length === 1) {
    throw new Error("No WebRTC quality measurements are available yet. Start audio or video and try again.");
  }
  return sample;
}

export default function RoomQualityPanel({
  conferenceId,
  sessionId,
  canViewSummary,
}: {
  conferenceId: string;
  sessionId?: string;
  canViewSummary: boolean;
}) {
  const room = useRoomContext();
  const quality = useConferenceQuality(conferenceId, canViewSummary);
  const submit = useSubmitQualitySamples(conferenceId);
  const [sampleError, setSampleError] = useState<unknown>();
  const [sampleResult, setSampleResult] = useState<string>();

  const sendSample = async () => {
    setSampleError(undefined);
    setSampleResult(undefined);
    try {
      if (!sessionId) throw new Error("This participant session is unavailable for quality reporting.");
      const sample = await collectSample(room);
      const result = await submit.mutateAsync({
        participantSessionId: sessionId,
        batchId: crypto.randomUUID(),
        samples: [sample],
      });
      setSampleResult(`${result.accepted} sample accepted${result.accepted === 1 ? "" : "s"}; ${result.sinkDropped} dropped.`);
    } catch (error) {
      setSampleError(error);
    }
  };

  return <section className="space-y-3 border-b border-border bg-card px-4 py-3">
    <div className="flex flex-wrap items-center justify-between gap-3">
      <div>
        <h2 className="font-medium">Connection quality</h2>
        <p className="text-xs text-muted-foreground">{sessionId ? "Send a one-time sample of locally measured WebRTC statistics." : "A participant session is required to submit quality measurements."}</p>
      </div>
      <Button size="sm" variant="outline" disabled={submit.isPending || !sessionId} onClick={() => void sendSample()}>
        {submit.isPending ? "Sending…" : "Send quality sample"}
      </Button>
    </div>
    {sampleError !== undefined && <RequestError error={sampleError} />}
    {sampleResult && <p role="status" className="text-sm">{sampleResult}</p>}
    {canViewSummary && <>
      {quality.isLoading && <p role="status" className="text-sm">Loading conference quality…</p>}
      {quality.isError && <RequestError error={quality.error} retry={() => void quality.refetch()} />}
      {quality.data && <div className="flex flex-wrap gap-x-5 gap-y-1 text-sm">
        <span>Health: {quality.data.healthState}</span>
        <span>Active sessions: {quality.data.activeSessions}</span>
        <span>Healthy / degraded / unhealthy: {quality.data.healthySessions} / {quality.data.degradedSessions} / {quality.data.unhealthySessions}</span>
        <span>Average RTT: {quality.data.avgRttMs == null ? "—" : `${Math.round(quality.data.avgRttMs)} ms`}</span>
        <span>Average jitter: {quality.data.avgJitterMs == null ? "—" : `${Math.round(quality.data.avgJitterMs)} ms`}</span>
        <span>Average packet loss: {quality.data.avgPacketLossRatio == null ? "—" : `${(quality.data.avgPacketLossRatio * 100).toFixed(1)}%`}</span>
        <span>Reconnects: {quality.data.reconnects}</span>
      </div>}
    </>}
  </section>;
}
