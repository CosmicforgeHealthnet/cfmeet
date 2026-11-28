"use client";

import { useEffect, useRef, useState } from "react";
import { AlertCircle, Mic, Music2, Plus, RotateCw, X } from "lucide-react";
import { cn } from "@/lib/utils";
import type { JobPreview, JobView } from "@/lib/status-publisher/types";
import type { PersonView } from "@/services/v2/social.service";
import { useStatusQueue } from "./StatusQueueRoot";
import TextStatusView from "@/components/media-editor/TextStatusView";
import { PersonAvatar } from "./shared";

/**
 * The own status card (WhatsApp): while statuses are being processed / uploaded / published it
 * shows the newest one's preview, "Adding…" and a progress ring round the avatar; a failure reads
 * "Failed to upload · Tap to retry" (Retry / Cancel sheet); once published it is "My status".
 */
export default function MyStatusCard({ me, postCount, latest, onView, onAdd }: {
  me: PersonView;
  /** Live statuses of mine in the feed. */
  postCount: number;
  /** The feed's latest status as the card's background. */
  latest: React.ReactNode;
  onView: () => void;
  onAdd: () => void;
}) {
  const { snapshot, retry, cancel } = useStatusQueue();
  const [sheet, setSheet] = useState(false);
  const pending = snapshot.jobs.filter((j) => j.state !== "done");
  const failed = pending.filter((j) => j.state === "failed");
  const active = pending.filter((j) => j.state !== "failed");
  const published = snapshot.jobs.filter((j) => j.state === "done");
  const newest = pending[pending.length - 1] ?? published[published.length - 1] ?? null;
  const progress = active.length ? active.reduce((s, j) => s + j.progress, 0) / active.length : 0;
  const hasStatus = postCount > 0 || published.length > 0;
  // The sheet closes itself when nothing is pending any more (and doesn't reopen with the next job).
  if (sheet && !pending.length) setSheet(false);

  const label = failed.length ? "Failed to upload · Tap to retry" : active.length ? "Adding…" : hasStatus ? "My status" : "Add status";
  const onImage = !!newest || hasStatus;

  const tap = () => {
    if (pending.length) setSheet(true);
    else if (postCount) onView();
    else onAdd();
  };

  return (
    <>
      <button type="button" data-testid="my-status" data-state={failed.length ? "failed" : active.length ? "adding" : hasStatus ? "posted" : "empty"} onClick={tap}
        aria-label={failed.length ? "Failed to upload your status. Tap to retry or cancel." : undefined}
        className="relative flex h-44 w-28 shrink-0 flex-col justify-between overflow-hidden rounded-2xl border border-border bg-card p-2.5 text-left">
        {newest ? <PendingThumb preview={newest.preview} /> : postCount ? latest : null}
        <span className="relative self-start">
          {active.length ? (
            <ProgressRing progress={progress}>
              <PersonAvatar person={me} size="size-11" />
            </ProgressRing>
          ) : failed.length ? (
            <span className="inline-flex rounded-full bg-destructive p-[3px]"><span className="flex rounded-full bg-background p-[2px]"><PersonAvatar person={me} size="size-11" /></span></span>
          ) : (
            <PersonAvatar person={me} size="size-11" ring={hasStatus ? "seen" : null} />
          )}
          {failed.length ? (
            <span className="absolute -bottom-0.5 -right-0.5 flex size-5 items-center justify-center rounded-full bg-destructive text-white ring-2 ring-card"><AlertCircle className="size-3.5" strokeWidth={3} /></span>
          ) : (
            <span className="absolute -bottom-0.5 -right-0.5 flex size-5 items-center justify-center rounded-full bg-primary text-primary-foreground ring-2 ring-card"
              onClick={(e) => { e.stopPropagation(); onAdd(); }} role="presentation"><Plus className="size-3.5" strokeWidth={3} /></span>
          )}
        </span>
        <span className={cn("relative font-semibold leading-tight", failed.length ? "text-xs" : "text-sm", onImage && "text-white drop-shadow")} data-testid="my-status-label">{label}</span>
      </button>
      {sheet ? (
        <QueueSheet jobs={pending} canView={postCount > 0} onView={() => { setSheet(false); onView(); }} onClose={() => setSheet(false)}
          onRetry={(id) => retry(id)} onCancel={(id) => cancel(id)} online={snapshot.online} />
      ) : null}
    </>
  );
}

/** SVG ring round the avatar; `progress` 0–1. */
function ProgressRing({ progress, children }: { progress: number; children: React.ReactNode }) {
  const pct = Math.round(progress * 100);
  const r = 26;
  const c = 2 * Math.PI * r;
  return (
    <span className="relative inline-flex size-[56px] items-center justify-center" role="progressbar" aria-label="Adding status" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} data-testid="status-progress">
      <svg viewBox="0 0 56 56" className="absolute inset-0 size-full -rotate-90" aria-hidden>
        <circle cx="28" cy="28" r={r} fill="none" strokeWidth="3" className="stroke-white/40" />
        <circle cx="28" cy="28" r={r} fill="none" strokeWidth="3" strokeLinecap="round" className="stroke-emerald-500 transition-[stroke-dashoffset] duration-300"
          strokeDasharray={c} strokeDashoffset={c * (1 - Math.max(0.03, progress))} />
      </svg>
      <span className="flex rounded-full bg-background p-[2px]">{children}</span>
    </span>
  );
}

/** A pending status's preview: the photo, the video's thumbnail / first frame, or the text on its colour. */
export function PendingThumb({ preview, small }: { preview: JobPreview; small?: boolean }) {
  return (
    <span aria-hidden className={cn(small ? "relative block size-12 shrink-0 overflow-hidden rounded-xl" : "absolute inset-0")}>
      {preview.kind === "TEXT" ? (
        <TextStatusView status={{ text: preview.text, background: preview.background, font: preview.font }} />
      ) : preview.blob ? (
        <BlobMedia blob={preview.blob} kind="image" />
      ) : preview.kind === "VIDEO" && preview.video ? (
        <BlobMedia blob={preview.video} kind="video" />
      ) : preview.kind === "AUDIO" ? (
        <span className="flex size-full items-center justify-center text-white" style={{ backgroundColor: preview.background ?? "#128C7E" }}>{preview.music ? <Music2 className={small ? "size-5" : "size-8"} /> : <Mic className={small ? "size-5" : "size-8"} />}</span>
      ) : (
        <span className="block size-full bg-gradient-to-b from-slate-600 to-slate-900" />
      )}
      {small ? null : <span className="absolute inset-0 bg-gradient-to-b from-black/30 via-transparent to-black/60" />}
    </span>
  );
}

/** An object URL for a Blob, owned by the element (created and revoked in the effect). */
function BlobMedia({ blob, kind }: { blob: Blob; kind: "image" | "video" }) {
  const img = useRef<HTMLImageElement>(null);
  const video = useRef<HTMLVideoElement>(null);
  useEffect(() => {
    const url = URL.createObjectURL(blob);
    // "#t=0.1" makes Safari paint the first frame without playing.
    if (kind === "video" && video.current) video.current.src = `${url}#t=0.1`;
    if (kind === "image" && img.current) img.current.src = url;
    return () => URL.revokeObjectURL(url);
  }, [blob, kind]);
  // eslint-disable-next-line @next/next/no-img-element
  if (kind === "image") return <img ref={img} alt="" className="size-full object-cover" />;
  return <video ref={video} muted playsInline preload="metadata" className="size-full object-cover" />;
}

const pct = (f: number) => `${Math.round(Math.max(0, Math.min(1, f)) * 100)}%`;
const mb = (b: number) => (b >= 1_000_000 ? `${(b / 1_000_000).toFixed(1)} MB` : `${Math.max(1, Math.round(b / 1000))} kB`);

/** One line naming the stage a pending status is in: processing, uploading, publishing, waiting or failed. */
export function stateText(job: JobView, online: boolean): string {
  switch (job.state) {
    case "failed": return job.error && job.error !== "Failed to upload" ? `Failed to upload · ${job.error}` : "Failed to upload";
    case "processing": return `Processing video… ${pct(job.processing ?? 0)}`;
    case "uploading": return job.upload ? `Uploading… ${mb(job.upload.sentBytes)} of ${mb(job.upload.totalBytes)}` : "Uploading…";
    case "publishing": return job.postCount > 1 ? `Publishing ${Math.min(job.postCount, job.publishedCount + 1)} of ${job.postCount}…` : "Publishing…";
    case "waiting": return online ? "Waiting to retry…" : "Waiting for connection…";
    case "done": return "Posted";
    default: return job.progress > 0 ? `Waiting to continue · ${pct(job.progress)} done` : "Waiting…";
  }
}

/** Separate bars for processing (video render) and upload, so the stage is never ambiguous. */
export function StageBars({ job }: { job: JobView }) {
  if (job.state === "failed" || job.state === "done") return null;
  const upload = job.upload && job.upload.totalBytes > 0 ? job.upload.sentBytes / job.upload.totalBytes : job.processing !== null && job.processing < 1 ? 0 : null;
  const rows: { label: string; value: number; active: boolean }[] = [];
  if (job.processing !== null) rows.push({ label: "Processing", value: job.processing, active: job.state === "processing" });
  if (upload !== null) rows.push({ label: "Upload", value: upload, active: job.state === "uploading" });
  rows.push({ label: "Published", value: job.postCount ? job.publishedCount / job.postCount : 0, active: job.state === "publishing" });
  return (
    <span className="mt-1 grid gap-1" data-testid="status-stage-bars">
      {rows.map((r) => (
        <span key={r.label} className="flex items-center gap-2 text-[10px] text-muted-foreground">
          <span className={cn("w-16 shrink-0", r.active && "font-semibold text-foreground")}>{r.label}</span>
          <span className="h-1 min-w-0 flex-1 overflow-hidden rounded-full bg-muted" role="progressbar" aria-label={r.label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(r.value * 100)}>
            <span className={cn("block h-full rounded-full transition-[width] duration-300", r.active ? "bg-primary" : "bg-primary/50")} style={{ width: pct(r.value) }} />
          </span>
          <span className="w-8 shrink-0 text-right tabular-nums">{pct(r.value)}</span>
        </span>
      ))}
    </span>
  );
}

export function jobTitle(job: JobView): string {
  if (job.postCount > 1) return `${job.postCount} status updates`;
  const p = job.preview;
  if (p.kind === "TEXT") return "Text status";
  if (p.kind === "IMAGE") return p.music ? "Photo with music" : "Photo";
  if (p.kind === "VIDEO") return "Video";
  return p.music ? "Music status" : "Voice status";
}

function QueueSheet({ jobs, canView, online, onView, onClose, onRetry, onCancel }: {
  jobs: JobView[]; canView: boolean; online: boolean;
  onView: () => void; onClose: () => void; onRetry: (id: string) => void; onCancel: (id: string) => void;
}) {
  return (
    <div className="fixed inset-0 z-[60] flex items-end justify-center bg-black/40 sm:items-center" onClick={onClose}>
      <div role="dialog" aria-label="Status updates in progress" className="w-full max-w-lg rounded-t-3xl bg-background p-4 shadow-2xl sm:rounded-3xl" onClick={(e) => e.stopPropagation()} data-testid="status-queue-sheet">
        <div className="mb-2 flex items-center justify-between">
          <h2 className="text-lg font-bold">My status</h2>
          <button type="button" aria-label="Close" onClick={onClose} className="flex size-9 items-center justify-center rounded-full hover:bg-muted"><X className="size-5" /></button>
        </div>
        <ul className="max-h-[50vh] space-y-1 overflow-y-auto">
          {[...jobs].reverse().map((job) => (
            <li key={job.id} className="flex items-center gap-3 rounded-2xl px-1 py-2" data-testid="status-queue-item">
              <PendingThumb preview={job.preview} small />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-sm font-semibold">{jobTitle(job)}</span>
                <span className={cn("block truncate text-xs", job.state === "failed" ? "text-destructive" : "text-muted-foreground")} aria-live="polite">{stateText(job, online)}</span>
                <StageBars job={job} />
              </span>
              {job.state === "failed" && !job.fatal ? (
                <button type="button" onClick={() => onRetry(job.id)} className="flex h-9 items-center gap-1.5 rounded-full bg-primary px-3 text-sm font-semibold text-primary-foreground" data-testid="status-retry">
                  <RotateCw className="size-4" />Retry
                </button>
              ) : null}
              <button type="button" onClick={() => onCancel(job.id)} className="h-9 rounded-full bg-muted px-3 text-sm font-semibold" data-testid="status-cancel">Cancel</button>
            </li>
          ))}
        </ul>
        {canView ? <button type="button" onClick={onView} className="mt-3 h-11 w-full rounded-full bg-primary/10 text-sm font-semibold text-primary">View my status</button> : null}
      </div>
    </div>
  );
}
