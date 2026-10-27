"use client";

import { useState, useMemo } from "react";
import { useRouter } from "next/navigation";
import { useSpaces } from "@/hooks/v2/useSpaces";
import type { MeetingSpaceResponse, SpaceStatus, AccessPolicy } from "@/types/v2";
import { Button } from "@/components/ui/button";
import {
  Calendar,
  Video,
  Lock,
  Globe,
  ChevronLeft,
  ChevronRight,
  Copy,
  Check,
  Users,
  Shield,
} from "lucide-react";
import { Spinner } from "@/components/ui/spinner";
import { formatShortDateForDisplay } from "@/utils/timezone";
import MeetingSpaceActions from "./MeetingSpaceActions";

const STATUS_FILTERS: { label: string; value: SpaceStatus | "all" }[] = [
  { label: "All", value: "all" },
  { label: "Active", value: "ACTIVE" },
  { label: "Archived", value: "ARCHIVED" },
];

const MEETING_TYPE_LABELS: Record<string, string> = {
  STANDARD: "Standard",
  WEBINAR: "Webinar",
  TOWN_HALL: "Town Hall",
  HEALTHCARE: "Healthcare",
  TRAINING: "Training",
};

const ITEMS_PER_PAGE = 10;

export default function MeetingsContent() {
  const router = useRouter();
  const [statusFilter, setStatusFilter] = useState<SpaceStatus | "all">("all");
  const [page, setPage] = useState(1);

  const { data, isLoading, isError, refetch } = useSpaces();

  // Filter and paginate client-side
  const filteredSpaces = useMemo(() => {
    const spaces = data?.items ?? [];
    if (statusFilter === "all") return spaces;
    return spaces.filter((s) => s.status === statusFilter);
  }, [data?.items, statusFilter]);

  const totalPages = Math.ceil(filteredSpaces.length / ITEMS_PER_PAGE);
  const paginatedSpaces = useMemo(() => {
    const start = (Math.min(page, Math.max(1, totalPages)) - 1) * ITEMS_PER_PAGE;
    return filteredSpaces.slice(start, start + ITEMS_PER_PAGE);
  }, [filteredSpaces, page, totalPages]);

  return (
    <div className="px-4 py-6 sm:px-6 sm:py-8 md:p-10 font-inter">
      {/* Header */}
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-4 mb-6 sm:mb-8">
        <div>
          <h2 className="text-xl sm:text-2xl font-bold text-foreground">
            Meeting Spaces
          </h2>
          <p className="text-sm text-muted-foreground mt-1">
            Reusable meeting settings for spaces you own or belong to. A space is separate from its live conferences.
          </p>
        </div>
        <div className="flex items-center gap-3"><Button onClick={() => router.push("/dashboard/meetings/new")}>Create meeting space</Button><span className="text-sm text-muted-foreground">
          {filteredSpaces.length} space{filteredSpaces.length !== 1 ? "s" : ""}{" "}
          {statusFilter !== "all" ? `(${statusFilter.toLowerCase()})` : "total"}
        </span></div>
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
          <Button variant="outline" onClick={() => void refetch()}>Retry</Button>
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
            onClick={() => router.push("/dashboard/meetings/new")}
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

    </div>
  );
}

// ─── Space Card ────────────────────────────────────────────────────────────

function getAccessPolicyIcon(policy: AccessPolicy) {
  switch (policy) {
    case "PUBLIC":
    case "PUBLIC_PASSCODE":
      return <Globe className="w-3.5 h-3.5" />;
    case "INVITE_ONLY":
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
    AUTHENTICATED: "Signed-in Users",
    ORGANIZATION_ONLY: "Organization Only",
    DOMAIN_RESTRICTED: "Domain Restricted",
    INVITE_ONLY: "Members and Invitations",
    CUSTOM: "Custom",
    HEALTHCARE_APPOINTMENT: "Healthcare",
  };
  return labels[policy] || policy;
}


function SpaceCard({
  space,
}: {
  space: MeetingSpaceResponse;
}) {
  const router = useRouter();
  const [copied, setCopied] = useState(false);


  const joinUrl =
    typeof window !== "undefined"
      ? `${window.location.origin}/${space.meetingCode}`
      : "";

  const handleCopy = () => {
    navigator.clipboard.writeText(joinUrl);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };



  return (
    <div className="bg-card border border-border rounded-xl p-4 sm:p-5 shadow-sm hover:shadow-md transition-shadow">
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-3">
        {/* Left */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 mb-1.5">
            <h4 className="font-semibold text-foreground text-sm sm:text-base truncate">
              {space.title || "Untitled Meeting"}
            </h4>
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
        <div className="flex flex-wrap items-center gap-1.5 sm:gap-2 shrink-0">
          <Button size="sm" variant="outline" onClick={() => router.push(`/dashboard/meetings/${space.id}`)}>Manage</Button>
          <MeetingSpaceActions space={space} />
          <Button size="sm" variant="outline" onClick={() => router.push(`/dashboard/conferences?space=${space.id}`)}>Conferences</Button>
        </div>
      </div>
    </div>
  );
}
