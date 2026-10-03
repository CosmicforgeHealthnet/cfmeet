"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { useMeeting, useUpdateMeeting } from "@/hooks";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { Spinner } from "@/components/ui/spinner";
import type { ApiErrorResponse } from "@/types/auth";
import type { Meeting } from "@/types/meeting";
import type { AxiosError } from "axios";
import { Calendar, Clock, Lock, Globe, ArrowLeft } from "lucide-react";
import { getUserTimezone, toLocalDateValue, toLocalTimeValue } from "@/utils/timezone";

const DURATION_OPTIONS = [
  { value: "15", label: "15 minutes" },
  { value: "30", label: "30 minutes" },
  { value: "45", label: "45 minutes" },
  { value: "60", label: "1 hour" },
  { value: "90", label: "1.5 hours" },
  { value: "120", label: "2 hours" },
  { value: "180", label: "3 hours" },
];

function calculateDuration(startTime: string, endTime: string): string {
  const start = new Date(startTime);
  const end = new Date(endTime);
  const diffMinutes = Math.round((end.getTime() - start.getTime()) / 60000);

  // Find the closest duration option
  const option = DURATION_OPTIONS.find((opt) => parseInt(opt.value) === diffMinutes);
  if (option) return option.value;

  // Default to 30 minutes if no match
  return "30";
}

interface EditMeetingFormProps {
  meeting: Meeting;
  meetingId: string;
}

function EditMeetingForm({ meeting, meetingId }: EditMeetingFormProps) {
  const router = useRouter();
  const updateMeeting = useUpdateMeeting();

  // Initialize state directly from props - no useEffect needed
  const [title, setTitle] = useState(meeting.title || "");
  const [date, setDate] = useState(toLocalDateValue(meeting.start_time));
  const [startTime, setStartTime] = useState(toLocalTimeValue(meeting.start_time));
  const [duration, setDuration] = useState(calculateDuration(meeting.start_time, meeting.end_time));
  const [isPrivate, setIsPrivate] = useState(meeting.is_private);
  const [metadata, setMetadata] = useState(meeting.metadata || "");
  const [error, setError] = useState("");

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    setError("");

    if (!title.trim()) {
      setError("Title is required");
      return;
    }
    if (!date || !startTime || !duration) {
      setError("Date, time, and duration are required");
      return;
    }

    const start_time = `${date}T${startTime}:00`;
    const startDate = new Date(start_time);
    const endDate = new Date(startDate.getTime() + parseInt(duration) * 60000);

    // Format end_time in local time (YYYY-MM-DDTHH:mm:ss)
    const endYear = endDate.getFullYear();
    const endMonth = String(endDate.getMonth() + 1).padStart(2, "0");
    const endDay = String(endDate.getDate()).padStart(2, "0");
    const endHours = String(endDate.getHours()).padStart(2, "0");
    const endMinutes = String(endDate.getMinutes()).padStart(2, "0");
    const end_time = `${endYear}-${endMonth}-${endDay}T${endHours}:${endMinutes}:00`;

    updateMeeting.mutate(
      {
        id: meetingId,
        data: {
          title: title.trim(),
          start_time,
          end_time,
          timezone: getUserTimezone(),
          is_private: isPrivate,
          metadata: metadata.trim() || undefined,
        },
      },
      {
        onSuccess: () => {
          router.push("/dashboard/meetings");
        },
        onError: (err: AxiosError<ApiErrorResponse>) => {
          const message =
            err.response?.data?.error?.message || "Failed to update meeting";
          setError(message);
        },
      },
    );
  };

  return (
    <div className="px-4 py-6 sm:px-6 sm:py-8 md:p-10 font-inter">
      {/* Back Button */}
      <button
        onClick={() => router.push("/dashboard/meetings")}
        className="flex items-center gap-2 text-[#343434] hover:text-[#029CD4] transition-colors mb-4"
      >
        <ArrowLeft className="w-5 h-5" />
        <span className="text-sm font-medium">Back to Meetings</span>
      </button>

      {/* Header */}
      <div className="mb-6 sm:mb-8">
        <h2 className="text-xl sm:text-2xl font-bold text-[#343434]">
          Edit Meeting
        </h2>
        <p className="text-sm text-[#00000080] mt-1">
          Update your meeting details
        </p>
      </div>

      {/* Form */}
      <form onSubmit={handleSubmit} className="max-w-lg space-y-5">
        {/* Title */}
        <div className="space-y-1.5">
          <Label htmlFor="title" className="text-sm font-medium text-[#343434]">
            Meeting Title
          </Label>
          <Input
            id="title"
            placeholder="e.g. Weekly Team Standup"
            value={title}
            onChange={(e) => setTitle(e.target.value)}
          />
        </div>

        {/* Date */}
        <div className="space-y-1.5">
          <Label htmlFor="date" className="text-sm font-medium text-[#343434]">
            <Calendar className="w-3.5 h-3.5 inline mr-1.5" />
            Date
          </Label>
          <Input
            id="date"
            type="date"
            value={date}
            onChange={(e) => setDate(e.target.value)}
          />
        </div>

        {/* Time and Duration */}
        <div className="grid grid-cols-2 gap-4">
          <div className="space-y-1.5">
            <Label
              htmlFor="startTime"
              className="text-sm font-medium text-[#343434]"
            >
              <Clock className="w-3.5 h-3.5 inline mr-1.5" />
              Start Time
            </Label>
            <Input
              id="startTime"
              type="time"
              value={startTime}
              onChange={(e) => setStartTime(e.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <Label
              htmlFor="duration"
              className="text-sm font-medium text-[#343434]"
            >
              <Clock className="w-3.5 h-3.5 inline mr-1.5" />
              Duration
            </Label>
            <select
              id="duration"
              value={duration}
              onChange={(e) => setDuration(e.target.value)}
              className="flex h-9 sm:h-10 w-full rounded-xl border border-[#029CD44D] bg-transparent px-3 sm:px-5 py-2 text-sm sm:text-base font-medium focus:outline-none focus:border-[#029CD4] focus:ring-1 focus:ring-[#029CD4]"
            >
              {DURATION_OPTIONS.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
          </div>
        </div>

        {/* Privacy Toggle */}
        <div className="space-y-1.5">
          <Label className="text-sm font-medium text-[#343434]">
            Meeting Type
          </Label>
          <TooltipProvider>
            <div className="flex gap-3">
              <Tooltip>
                <TooltipTrigger asChild>
                  <button
                    type="button"
                    onClick={() => setIsPrivate(true)}
                    className={`flex items-center gap-2 px-4 py-2.5 rounded-xl text-sm font-medium transition-all ${
                      isPrivate
                        ? "bg-[#029CD4] text-white shadow-sm"
                        : "bg-[#FAFAFB] text-[#00000080] hover:bg-gray-200"
                    }`}
                  >
                    <Lock className="w-4 h-4" />
                    Private
                  </button>
                </TooltipTrigger>
                <TooltipContent>
                  <p className="max-w-xs">
                    Private meetings require authentication. Guests cannot join,
                    and authenticated participants must wait for host approval
                    before entering.
                  </p>
                </TooltipContent>
              </Tooltip>
              <Tooltip>
                <TooltipTrigger asChild>
                  <button
                    type="button"
                    onClick={() => setIsPrivate(false)}
                    className={`flex items-center gap-2 px-4 py-2.5 rounded-xl text-sm font-medium transition-all ${
                      !isPrivate
                        ? "bg-[#029CD4] text-white shadow-sm"
                        : "bg-[#FAFAFB] text-[#00000080] hover:bg-gray-200"
                    }`}
                  >
                    <Globe className="w-4 h-4" />
                    Public
                  </button>
                </TooltipTrigger>
                <TooltipContent>
                  <p className="max-w-xs">
                    Public meetings allow anyone with the link to join
                    instantly, including guests. No waiting room or approval
                    needed.
                  </p>
                </TooltipContent>
              </Tooltip>
            </div>
          </TooltipProvider>
        </div>

        {/* Metadata / Description */}
        <div className="space-y-1.5">
          <Label
            htmlFor="metadata"
            className="text-sm font-medium text-[#343434]"
          >
            Description{" "}
            <span className="text-[#00000040] font-normal">(optional)</span>
          </Label>
          <textarea
            id="metadata"
            placeholder="Add a description or agenda..."
            value={metadata}
            onChange={(e) => setMetadata(e.target.value)}
            rows={3}
            className="flex w-full rounded-xl border border-[#029CD44D] bg-transparent px-3 sm:px-5 py-2 sm:py-3 text-sm sm:text-base font-medium placeholder:text-[#029CD44D] focus:outline-none focus:border-[#029CD4] focus:ring-1 focus:ring-[#029CD4] resize-none"
          />
        </div>

        {/* Error */}
        {error && <p className="text-sm text-red-500 font-medium">{error}</p>}

        {/* Actions */}
        <div className="flex gap-3">
          <Button
            type="submit"
            size="lg"
            className="flex-1"
            loading={updateMeeting.isPending}
          >
            Save Changes
          </Button>
          <Button
            type="button"
            variant="outline"
            size="lg"
            onClick={() => router.push("/dashboard/meetings")}
          >
            Cancel
          </Button>
        </div>
      </form>
    </div>
  );
}

export default function EditMeetingContent({ meetingId }: { meetingId: string }) {
  const router = useRouter();
  const { data: meetingData, isLoading: isFetching, isError } = useMeeting(meetingId);

  if (isFetching) {
    return (
      <div className="flex items-center justify-center py-20">
        <Spinner />
      </div>
    );
  }

  if (isError || !meetingData?.data) {
    return (
      <div className="px-4 py-6 sm:px-6 sm:py-8 md:p-10 font-inter">
        <div className="text-center py-20">
          <p className="text-red-500 font-medium">Failed to load meeting</p>
          <p className="text-sm text-[#00000080] mt-1">
            The meeting may not exist or you don&apos;t have access to it.
          </p>
          <Button
            variant="outline"
            className="mt-4"
            onClick={() => router.push("/dashboard/meetings")}
          >
            Back to Meetings
          </Button>
        </div>
      </div>
    );
  }

  // Render form only when data is available - state initializes from props
  return <EditMeetingForm meeting={meetingData.data} meetingId={meetingId} />;
}
