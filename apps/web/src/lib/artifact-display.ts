/**
 * Display rules for meeting artefacts (FE-007). Reads only the fields the backend's
 * `ArtifactView` (OpenAPI) sends: there is no filename, MIME type, ready time or deleted
 * time on the wire. A display name is derived from the type and creation time; "ready" is
 * `finalizedAt`; deletion is `deletionPending` / status DELETED.
 *
 * Pure: type-only imports, so node tests run it directly (tests/meetings-calendar.test.mjs).
 */
import type { ArtifactType, ArtifactView } from "../types/generated/meeting-service";

const TYPE_LABELS: Record<ArtifactType, string> = {
  RECORDING: "Recording",
  TRANSCRIPT: "Transcript",
  CAPTION_TRACK: "Caption track",
  ATTENDANCE_REPORT: "Attendance report",
  CHAT_EXPORT: "Chat export",
  POLL_REPORT: "Poll report",
  MEETING_SUMMARY: "Meeting summary",
  WHITEBOARD_SNAPSHOT: "Whiteboard snapshot",
};

export function artifactTypeLabel(type: ArtifactType | string): string {
  return TYPE_LABELS[type as ArtifactType] ?? String(type).replaceAll("_", " ").toLowerCase();
}

/** "Recording · 9 Nov 2026, 14:05" (the viewer's zone). */
export function artifactDisplayName(artifact: Pick<ArtifactView, "artifactType" | "createdAt">, timeZone?: string): string {
  const created = Date.parse(artifact.createdAt);
  if (!Number.isFinite(created)) return artifactTypeLabel(artifact.artifactType);
  const when = new Intl.DateTimeFormat("en-GB", { timeZone, day: "numeric", month: "short", year: "numeric", hour: "2-digit", minute: "2-digit", hourCycle: "h23" }).format(created);
  return `${artifactTypeLabel(artifact.artifactType)} · ${when}`;
}

export function formatBytes(bytes: number | null | undefined): string | null {
  if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return null;
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; }
  return `${value >= 10 ? Math.round(value) : Math.round(value * 10) / 10} ${units[unit]}`;
}

/** `durationMs` → "1:02:03" / "4:05". */
export function formatDurationMs(ms: number | null | undefined): string | null {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return null;
  const total = Math.round(ms / 1000);
  const h = Math.floor(total / 3600), m = Math.floor((total % 3600) / 60), s = total % 60;
  const two = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${two(m)}:${two(s)}` : `${m}:${two(s)}`;
}

/** Short state line: status, deletion and failure category as the backend reports them. */
export function artifactStateLabel(artifact: Pick<ArtifactView, "status" | "deletionPending" | "failureCode">): string {
  if (artifact.status === "DELETED") return "Deleted";
  if (artifact.deletionPending) return "Deletion pending";
  if (artifact.status === "FAILED") return artifact.failureCode ? `Failed (${artifact.failureCode})` : "Failed";
  return artifact.status.charAt(0) + artifact.status.slice(1).toLowerCase();
}

/** Secure access is issued only for READY artefacts without a pending deletion. */
export function artifactAccessible(artifact: Pick<ArtifactView, "status" | "deletionPending">): boolean {
  return artifact.status === "READY" && !artifact.deletionPending;
}

const fmt = (iso: string | null | undefined, timeZone?: string) => {
  if (!iso) return null;
  const at = Date.parse(iso);
  return Number.isFinite(at) ? new Intl.DateTimeFormat("en-GB", { timeZone, dateStyle: "medium", timeStyle: "short" }).format(at) : null;
};

/** Label/value rows for the detail view; absent fields are left out rather than shown as "Not available". */
export function artifactDetailRows(artifact: ArtifactView, timeZone?: string): Array<[string, string]> {
  const rows: Array<[string, string | null]> = [
    ["ID", artifact.id],
    ["Type", artifactTypeLabel(artifact.artifactType)],
    ["State", artifactStateLabel(artifact)],
    ["Content type", artifact.contentType ?? null],
    ["Size", formatBytes(artifact.sizeBytes)],
    ["Duration", formatDurationMs(artifact.durationMs)],
    ["Layout", artifact.layout ?? null],
    ["Started", fmt(artifact.startedAt, timeZone)],
    ["Ended", fmt(artifact.endedAt, timeZone)],
    ["Finalized", fmt(artifact.finalizedAt, timeZone)],
    ["Created", fmt(artifact.createdAt, timeZone)],
    ["Updated", fmt(artifact.updatedAt, timeZone)],
    ["Retained until", fmt(artifact.retentionAt, timeZone)],
    ["SHA-256", artifact.checksumSha256 ?? null],
    ["Derived from", artifact.sourceArtifactId ?? null],
  ];
  return rows.filter((row): row is [string, string] => row[1] !== null && row[1] !== "");
}
