"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { useMeetings, useDeleteMeeting } from "@/hooks";
import type { Meeting, MeetingStatus } from "@/types/meeting";
import type { ApiErrorResponse } from "@/types/auth";
import type { AxiosError } from "axios";
import { Button } from "@/components/ui/button";
import {
  Calendar,
  Clock,
  Video,
  Lock,
  Globe,
  ExternalLink,
  ChevronLeft,
  ChevronRight,
  Pencil,
  Trash2,
  Copy,
  Check,
} from "lucide-react";
import { Spinner } from "@/components/ui/spinner";
import {
  formatShortDateForDisplay,
  formatTimeForDisplay,
} from "@/utils/timezone";

const STATUS_FILTERS: { label: string; value: MeetingStatus | "all" }[] = [
  { label: "All", value: "all" },
  { label: "Scheduled", value: "scheduled" },
  { label: "Live", value: "live" },
  { label: "Ended", value: "ended" },
  { label: "Cancelled", value: "cancelled" },
];

const STATUS_STYLES: Record<MeetingStatus, string> = {
  scheduled: "bg-blue-50 text-blue-600",
  live: "bg-green-50 text-green-600",
  ended: "bg-gray-100 text-gray-500",
  cancelled: "bg-red-50 text-red-500",
};

function getDuration(start: string, end: string) {
  const ms = new Date(end).getTime() - new Date(start).getTime();
  const mins = Math.round(ms / 60000);
  if (mins < 60) return `${mins}m`;
  const hrs = Math.floor(mins / 60);
  const rem = mins % 60;
  return rem > 0 ? `${hrs}h ${rem}m` : `${hrs}h`;
}

export default function MeetingsContent() {
  const router = useRouter();
  const [statusFilter, setStatusFilter] = useState<MeetingStatus | "all">(
    "all",
  );
  const [page, setPage] = useState(1);
  const [deletingId, setDeletingId] = useState<string | null>(null);

  const { data, isLoading, isError } = useMeetings({
    page,
    limit: 10,
    ...(statusFilter !== "all" && { status: statusFilter }),
  });

  const meetings = data?.data ?? [];
  const meta = data?.meta;

  return (
    <div className="px-4 py-6 sm:px-6 sm:py-8 md:p-10 font-inter">
      {/* Header */}
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-4 mb-6 sm:mb-8">
        <div>
          <h2 className="text-xl sm:text-2xl font-bold text-[#343434]">
            Meetings
          </h2>
          <p className="text-sm text-[#00000080] mt-1">
            View and manage your meetings
          </p>
        </div>
        {meta && (
          <span className="text-sm text-[#00000080]">
            {meta.total} meeting{meta.total !== 1 ? "s" : ""} total
          </span>
        )}
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
                ? "bg-[#029CD4] text-white shadow-sm"
                : "bg-[#FAFAFB] text-[#00000080] hover:bg-gray-200"
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
          <p className="text-red-500 font-medium">Failed to load meetings</p>
          <p className="text-sm text-[#00000080] mt-1">
            Please check your connection and try again.
          </p>
        </div>
      ) : meetings.length === 0 ? (
        <div className="flex flex-col items-center justify-center py-20 text-center">
          <Calendar className="w-12 h-12 text-gray-300 mb-3" />
          <p className="text-[#343434] font-medium">No meetings found</p>
          <p className="text-sm text-[#00000080] mt-1">
            {statusFilter !== "all"
              ? `No ${statusFilter} meetings to show.`
              : "You haven't scheduled any meetings yet."}
          </p>
        </div>
      ) : (
        <>
          {/* Meeting List */}
          <div className="space-y-3">
            {meetings.map((meeting) => (
              <MeetingCard
                key={meeting.id}
                meeting={meeting}
                onEdit={() => router.push(`/dashboard/meetings/${meeting.id}/edit`)}
                onDelete={() => setDeletingId(meeting.id)}
              />
            ))}
          </div>

          {/* Pagination */}
          {meta && meta.total_pages > 1 && (
            <div className="flex items-center justify-center gap-3 mt-8">
              <Button
                variant="outline"
                size="sm"
                disabled={page <= 1}
                onClick={() => setPage((p) => p - 1)}
              >
                <ChevronLeft className="w-4 h-4" />
              </Button>
              <span className="text-sm text-[#00000080]">
                Page {meta.page} of {meta.total_pages}
              </span>
              <Button
                variant="outline"
                size="sm"
                disabled={page >= meta.total_pages}
                onClick={() => setPage((p) => p + 1)}
              >
                <ChevronRight className="w-4 h-4" />
              </Button>
            </div>
          )}
        </>
      )}

      {/* Delete Confirm Modal */}
      {deletingId && (
        <DeleteMeetingModal
          meetingId={deletingId}
          onClose={() => setDeletingId(null)}
        />
      )}
    </div>
  );
}

// ─── Meeting Card ────────────────────────────────────────────────────────────

function MeetingCard({
  meeting,
  onEdit,
  onDelete,
}: {
  meeting: Meeting;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const isScheduled = meeting.status === "scheduled";

  const handleCopy = () => {
    navigator.clipboard.writeText(meeting.join_url);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div className="bg-white border border-gray-100 rounded-xl p-4 sm:p-5 shadow-sm hover:shadow-md transition-shadow">
      <div className="flex flex-col sm:flex-row sm:items-center sm:justify-between gap-3">
        {/* Left */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 mb-1.5">
            <h4 className="font-semibold text-[#343434] text-sm sm:text-base truncate">
              {meeting.title}
            </h4>
            <span
              className={`px-2 py-0.5 rounded-full text-[11px] font-medium capitalize shrink-0 ${STATUS_STYLES[meeting.status]}`}
            >
              {meeting.status}
            </span>
          </div>

          <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-[#00000080] text-xs sm:text-sm">
            <span className="flex items-center gap-1">
              <Calendar className="w-3.5 h-3.5" />
              {formatShortDateForDisplay(meeting.start_time)}
            </span>
            <span className="flex items-center gap-1">
              <Clock className="w-3.5 h-3.5" />
              {formatTimeForDisplay(meeting.start_time)} –{" "}
              {formatTimeForDisplay(meeting.end_time)}
            </span>
            <span className="flex items-center gap-1">
              <Video className="w-3.5 h-3.5" />
              {getDuration(meeting.start_time, meeting.end_time)}
            </span>
            <span className="flex items-center gap-1">
              {meeting.is_private ? (
                <Lock className="w-3.5 h-3.5" />
              ) : (
                <Globe className="w-3.5 h-3.5" />
              )}
              {meeting.is_private ? "Private" : "Public"}
            </span>
          </div>

          <div className="flex items-center gap-2 mt-2 text-xs text-[#00000060]">
            <span className="font-mono bg-gray-50 px-2 py-0.5 rounded">
              {meeting.meeting_identifier}
            </span>
            <button
              onClick={handleCopy}
              className="p-1 hover:bg-gray-100 rounded transition-colors"
              title="Copy meeting link"
            >
              {copied ? (
                <Check className="w-3.5 h-3.5 text-green-500" />
              ) : (
                <Copy className="w-3.5 h-3.5 text-[#00000060] hover:text-[#029CD4]" />
              )}
            </button>
          </div>
        </div>

        {/* Right — Actions */}
        <div className="flex items-center gap-1.5 sm:gap-2 shrink-0">
          {isScheduled && (
            <>
              <Button
                variant="ghost"
                size="sm"
                className="gap-1.5 text-[#029CD4] px-2 sm:px-3"
                onClick={onEdit}
                title="Edit meeting"
              >
                <Pencil className="w-3.5 h-3.5 sm:w-4 sm:h-4" />
                <span className="hidden sm:inline">Edit</span>
              </Button>
              <Button
                variant="ghost"
                size="sm"
                className="gap-1.5 text-red-500 hover:text-red-600 hover:bg-red-50 px-2 sm:px-3"
                onClick={onDelete}
                title="Delete meeting"
              >
                <Trash2 className="w-3.5 h-3.5 sm:w-4 sm:h-4" />
                <span className="hidden sm:inline">Delete</span>
              </Button>
            </>
          )}
          {(meeting.status === "scheduled" || meeting.status === "live") && (
            <Button
              size="sm"
              className="gap-1.5 px-2 sm:px-3"
              onClick={() => window.open(meeting.join_url, "_blank")}
              title={
                meeting.status === "live" ? "Join meeting now" : "Join meeting"
              }
            >
              <ExternalLink className="w-3.5 h-3.5 sm:w-4 sm:h-4" />
              <span className="hidden sm:inline">
                {meeting.status === "live" ? "Join Now" : "Join"}
              </span>
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}

// ─── Delete Confirm Modal ────────────────────────────────────────────────────

function DeleteMeetingModal({
  meetingId,
  onClose,
}: {
  meetingId: string;
  onClose: () => void;
}) {
  const deleteMeeting = useDeleteMeeting();
  const [error, setError] = useState("");

  const handleDelete = () => {
    setError("");
    deleteMeeting.mutate(meetingId, {
      onSuccess: () => onClose(),
      onError: (err: AxiosError<ApiErrorResponse>) => {
        setError(
          err.response?.data?.error?.message || "Failed to delete meeting",
        );
      },
    });
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/40" onClick={onClose} />
      <div className="relative bg-white rounded-2xl shadow-xl w-full max-w-sm mx-4 p-6 text-center">
        <div className="w-12 h-12 rounded-full bg-red-50 flex items-center justify-center mx-auto mb-4">
          <Trash2 className="w-5 h-5 text-red-500" />
        </div>
        <h3 className="text-lg font-bold text-[#343434] mb-1">
          Delete Meeting
        </h3>
        <p className="text-sm text-[#00000080] mb-6">
          Are you sure? This action cannot be undone.
        </p>

        {error && (
          <p className="text-sm text-red-500 font-medium mb-4">{error}</p>
        )}

        <div className="flex gap-3">
          <Button variant="outline" className="flex-1" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="destructive"
            className="flex-1"
            loading={deleteMeeting.isPending}
            onClick={handleDelete}
          >
            Delete
          </Button>
        </div>
      </div>
    </div>
  );
}
