"use client";

import { useState, useMemo } from "react";
import { useRouter } from "next/navigation";
import { useSpaces, useArchiveSpace } from "@/hooks/v2";
import type { MeetingSpaceResponse, SpaceStatus, AccessPolicy, ConferenceStatus } from "@/types/v2";
import { Button } from "@/components/ui/button";
import {
  Calendar,
  Video,
  Lock,
  Globe,
  ExternalLink,
  ChevronLeft,
  ChevronRight,
  Pencil,
  Archive,
  Copy,
  Check,
  Users,
  Shield,
} from "lucide-react";
import { Spinner } from "@/components/ui/spinner";
import { formatShortDateForDisplay } from "@/utils/timezone";

const STATUS_FILTERS: { label: string; value: SpaceStatus | "all" }[] = [
  { label: "All", value: "all" },
  { label: "Active", value: "ACTIVE" },
  { label: "Archived", value: "ARCHIVED" },
];

const MEETING_TYPE_LABELS: Record<string, string> = {
  STANDARD: "Standard",
  WEBINAR: "Webinar",
  TOWN_HALL: "Town Hall",
  LARGE_MEETING: "Large Meeting",
};

const ITEMS_PER_PAGE = 10;

export default function MeetingsContent() {
  const router = useRouter();
  const [statusFilter, setStatusFilter] = useState<SpaceStatus | "all">("all");
  const [page, setPage] = useState(1);
  const [archivingId, setArchivingId] = useState<string | null>(null);

  const { data, isLoading, isError } = useSpaces();

  // Filter and paginate client-side
  const filteredSpaces = useMemo(() => {
    const spaces = data?.items ?? [];
    if (statusFilter === "all") return spaces;
    return spaces.filter((s) => s.status === statusFilter);
  }, [data?.items, statusFilter]);

  const totalPages = Math.ceil(filteredSpaces.length / ITEMS_PER_PAGE);
  const paginatedSpaces = useMemo(() => {
    const start = (page - 1) * ITEMS_PER_PAGE;
    return filteredSpaces.slice(start, start + ITEMS_PER_PAGE);
  }, [filteredSpaces, page]);

  return (
    <div className="px-4 py-6 sm:px-6 sm:py-8 md:p-10 font-inter">
      {/* Header */}
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-4 mb-6 sm:mb-8">
        <div>
          <h2 className="text-xl sm:text-2xl font-bold text-foreground">
            Meeting Spaces
          </h2>
          <p className="text-sm text-muted-foreground mt-1">
            View and manage your meeting spaces
          </p>
        </div>
        <span className="text-sm text-muted-foreground">
          {filteredSpaces.length} space{filteredSpaces.length !== 1 ? "s" : ""}{" "}
          {statusFilter !== "all" ? `(${statusFilter.toLowerCase()})` : "total"}
        </span>
      </div>

      {/* Status Filters */}
      <div className="flex gap-2 mb-6 overflow-x-auto pb-1">
        {STATUS_FILTERS.map((filter) => (
          <button
            key={filter.value}
            onClick={() => {
              setStatusFilter(filter.value);
              setPage(1);
            }}
            className={`px-4 py-1.5 rounded-full text-sm font-medium whitespace-nowrap transition-all ${
              statusFilter === filter.value
                ? "bg-primary text-white shadow-sm"
                : "bg-sidebar text-muted-foreground hover:bg-muted"
            }`}
          >
            {filter.label}
          </button>
        ))}
      </div>

      {/* Content */}
      {isLoading ? (
        <div className="flex items-center justify-center py-20">
          <Spinner />
        </div>
      ) : isError ? (
        <div className="flex flex-col items-center justify-center py-20 text-center">
          <p className="text-red-500 font-medium">Failed to load meeting spaces</p>
          <p className="text-sm text-muted-foreground mt-1">
            Please check your connection and try again.
          </p>
        </div>
      ) : paginatedSpaces.length === 0 ? (
        <div className="flex flex-col items-center justify-center py-20 text-center">
          <Calendar className="w-12 h-12 text-muted-foreground mb-3" />
          <p className="text-foreground font-medium">No meeting spaces found</p>
          <p className="text-sm text-muted-foreground mt-1">
            {statusFilter !== "all"
              ? `No ${statusFilter.toLowerCase()} meeting spaces to show.`
              : "You haven't created any meeting spaces yet."}
          </p>
          <Button
            className="mt-4"
            onClick={() => router.push("/dashboard/schedule")}
          >
            Create Meeting Space
          </Button>
        </div>
      ) : (
        <>
          {/* Space List */}
          <div className="space-y-3">
            {paginatedSpaces.map((space) => (
              <SpaceCard
                key={space.id}
                space={space}
                onEdit={() => router.push(`/dashboard/meetings/${space.id}/edit`)}
                onArchive={() => setArchivingId(space.id)}
              />
            ))}
          </div>

          {/* Pagination */}
          {totalPages > 1 && (
            <div className="flex items-center justify-center gap-3 mt-8">
              <Button
                variant="outline"
                size="sm"
                disabled={page <= 1}
                onClick={() => setPage((p) => p - 1)}
              >
                <ChevronLeft className="w-4 h-4" />
              </Button>
              <span className="text-sm text-muted-foreground">
                Page {page} of {totalPages}
              </span>
              <Button
                variant="outline"
                size="sm"
                disabled={page >= totalPages}
                onClick={() => setPage((p) => p + 1)}
              >
                <ChevronRight className="w-4 h-4" />
              </Button>
            </div>
          )}
        </>
      )}

      {/* Archive Confirm Modal */}
      {archivingId && (
        <ArchiveSpaceModal
          spaceId={archivingId}
          onClose={() => setArchivingId(null)}
        />
      )}
    </div>
  );
}

// ─── Space Card ────────────────────────────────────────────────────────────

function getAccessPolicyIcon(policy: AccessPolicy) {
  switch (policy) {
    case "PUBLIC":
    case "PUBLIC_PASSCODE":
      return <Globe className="w-3.5 h-3.5" />;
    case "MEMBERS_ONLY":
    case "INVITATION_ONLY":
      return <Lock className="w-3.5 h-3.5" />;
    case "ORGANIZATION_ONLY":
    case "DOMAIN_RESTRICTED":
      return <Shield className="w-3.5 h-3.5" />;
    default:
      return <Users className="w-3.5 h-3.5" />;
  }
}

function getAccessPolicyLabel(policy: AccessPolicy): string {
  const labels: Record<string, string> = {
    PUBLIC: "Public",
    PUBLIC_PASSCODE: "Passcode Protected",
    REGISTERED_USERS: "Registered Users",
    ORGANIZATION_ONLY: "Organization Only",
    DOMAIN_RESTRICTED: "Domain Restricted",
    EXTERNAL_ONLY: "External Only",
    MEMBERS_ONLY: "Members Only",
    INVITATION_ONLY: "Invitation Only",
    CUSTOM: "Custom",
    HEALTHCARE_APPOINTMENT: "Healthcare",
  };
  return labels[policy] || policy;
}

function getConferenceStatusInfo(status: ConferenceStatus): { label: string; className: string } {
  switch (status) {
    case "LIVE":
      return { label: "Live", className: "bg-green-500 text-white" };
    case "WAITING":
    case "PREPARING":
      return { label: "Starting", className: "bg-yellow-500 text-white" };
    case "CREATED":
      return { label: "Scheduled", className: "bg-blue-500 text-white" };
    case "ENDING":
      return { label: "Ending", className: "bg-orange-500 text-white" };
    case "ENDED":
      return { label: "Ended", className: "bg-gray-500 text-white" };
    case "CANCELLED":
      return { label: "Cancelled", className: "bg-red-500 text-white" };
    case "FAILED":
      return { label: "Failed", className: "bg-red-600 text-white" };
    default:
      return { label: status, className: "bg-gray-400 text-white" };
  }
}

function SpaceCard({
  space,
  onEdit,
  onArchive,
}: {
  space: MeetingSpaceResponse;
  onEdit: () => void;
  onArchive: () => void;
}) {
  const router = useRouter();
  const [copied, setCopied] = useState(false);
  const isActive = space.status === "ACTIVE";

  const joinUrl =
    typeof window !== "undefined"
      ? `${window.location.origin}/${space.meetingCode}`
      : "";

  const handleCopy = () => {
    navigator.clipboard.writeText(joinUrl);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  const handleJoin = () => {
    router.push(`/${space.meetingCode}`);
  };

  const conferenceStatus = space.latestConference?.status;
  const statusInfo = conferenceStatus ? getConferenceStatusInfo(conferenceStatus) : null;

  return (
    <div className="bg-card border border-border rounded-xl p-4 sm:p-5 shadow-sm hover:shadow-md transition-shadow">
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-3">
        {/* Left */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 mb-1.5">
            <h4 className="font-semibold text-foreground text-sm sm:text-base truncate">
              {space.title || "Untitled Meeting"}
            </h4>
            {statusInfo && (
              <span className={`px-2 py-0.5 rounded text-xs font-medium shrink-0 ${statusInfo.className}`}>
                {statusInfo.label}
              </span>
            )}
          </div>

          <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-muted-foreground text-xs sm:text-sm">
            <span className="flex items-center gap-1">
              <Video className="w-3.5 h-3.5" />
              {MEETING_TYPE_LABELS[space.meetingType] || space.meetingType}
            </span>
            <span className="flex items-center gap-1">
              {getAccessPolicyIcon(space.accessPolicy)}
              {getAccessPolicyLabel(space.accessPolicy)}
            </span>
            {space.interactiveCapacity && (
              <span className="flex items-center gap-1">
                <Users className="w-3.5 h-3.5" />
                Up to {space.interactiveCapacity} participants
              </span>
            )}
            {space.createdAt && (
              <span className="flex items-center gap-1">
                <Calendar className="w-3.5 h-3.5" />
                {formatShortDateForDisplay(space.createdAt)}
              </span>
            )}
          </div>

          <div className="flex items-center gap-2 mt-2 text-xs text-muted-foreground">
            <span className="font-mono bg-muted px-2 py-0.5 rounded">
              {space.meetingCode}
            </span>
            <button
              onClick={handleCopy}
              className="p-1 hover:bg-muted rounded transition-colors"
              title="Copy meeting link"
            >
              {copied ? (
                <Check className="w-3.5 h-3.5 text-green-500" />
              ) : (
                <Copy className="w-3.5 h-3.5 text-muted-foreground hover:text-primary" />
              )}
            </button>
          </div>
        </div>

        {/* Right — Actions */}
        <div className="flex items-center gap-1.5 sm:gap-2 shrink-0">
          {isActive && (
            <>
              <Button
                variant="ghost"
                size="sm"
                className="gap-1.5 text-primary px-2 sm:px-3"
                onClick={onEdit}
                title="Edit meeting space"
              >
                <Pencil className="w-3.5 h-3.5 sm:w-4 sm:h-4" />
                <span className="hidden sm:inline">Edit</span>
              </Button>
              <Button
                variant="ghost"
                size="sm"
                className="gap-1.5 text-orange-500 hover:text-orange-600 hover:bg-orange-50 dark:hover:bg-orange-900/20 px-2 sm:px-3"
                onClick={onArchive}
                title="Archive meeting space"
              >
                <Archive className="w-3.5 h-3.5 sm:w-4 sm:h-4" />
                <span className="hidden sm:inline">Archive</span>
              </Button>
            </>
          )}
          {isActive && (
            <Button
              size="sm"
              className="gap-1.5 px-2 sm:px-3"
              onClick={handleJoin}
              title="Join meeting"
            >
              <ExternalLink className="w-3.5 h-3.5 sm:w-4 sm:h-4" />
              <span className="hidden sm:inline">Join</span>
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}

// ─── Archive Confirm Modal ────────────────────────────────────────────────────

function ArchiveSpaceModal({
  spaceId,
  onClose,
}: {
  spaceId: string;
  onClose: () => void;
}) {
  const archiveSpace = useArchiveSpace();
  const [error, setError] = useState("");

  const handleArchive = () => {
    setError("");
    archiveSpace.mutate(
      { id: spaceId },
      {
        onSuccess: () => onClose(),
        onError: (err) => {
          const message =
            err instanceof Error ? err.message : "Failed to archive meeting space";
          setError(message);
        },
      }
    );
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/40" onClick={onClose} />
      <div className="relative bg-card rounded-2xl shadow-xl w-full max-w-sm mx-4 p-6 text-center">
        <div className="w-12 h-12 rounded-full bg-orange-50 dark:bg-orange-900/20 flex items-center justify-center mx-auto mb-4">
          <Archive className="w-5 h-5 text-orange-500" />
        </div>
        <h3 className="text-lg font-bold text-foreground mb-1">
          Archive Meeting Space
        </h3>
        <p className="text-sm text-muted-foreground mb-6">
          Archived spaces can be restored later. Any active conferences will be ended.
        </p>

        {error && (
          <p className="text-sm text-red-500 font-medium mb-4">{error}</p>
        )}

        <div className="flex gap-3">
          <Button variant="outline" className="flex-1" onClick={onClose}>
            Cancel
          </Button>
          <Button
            className="flex-1 bg-orange-500 hover:bg-orange-600 text-white"
            loading={archiveSpace.isPending}
            onClick={handleArchive}
          >
            Archive
          </Button>
        </div>
      </div>
    </div>
  );
}
