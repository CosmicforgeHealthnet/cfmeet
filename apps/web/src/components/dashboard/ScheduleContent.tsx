"use client";

import { useState } from "react";
import { useCreateMeeting } from "@/hooks";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import type { ApiErrorResponse } from "@/types/auth";
import type { AxiosError } from "axios";
import { Calendar, Clock, Lock, Globe } from "lucide-react";
import { getUserTimezone } from "@/utils/timezone";
import MeetingSuccessView from "./MeetingSuccessView";

const DURATION_OPTIONS = [
  { value: "15", label: "15 minutes" },
  { value: "30", label: "30 minutes" },
  { value: "45", label: "45 minutes" },
  { value: "60", label: "1 hour" },
  { value: "90", label: "1.5 hours" },
  { value: "120", label: "2 hours" },
  { value: "180", label: "3 hours" },
];

function getCurrentDate() {
  const now = new Date();
  return now.toISOString().split("T")[0];
}

function getCurrentTime() {
  const now = new Date();
  return now.toTimeString().slice(0, 5);
}

export default function ScheduleContent() {
  const createMeeting = useCreateMeeting();

  const [title, setTitle] = useState("");
  const [date, setDate] = useState(getCurrentDate);
  const [startTime, setStartTime] = useState(getCurrentTime);
  const [duration, setDuration] = useState("30");
  const [isPrivate, setIsPrivate] = useState(false);
  const [metadata, setMetadata] = useState("");
  const [error, setError] = useState("");
  const [created, setCreated] = useState<{
    meeting_identifier: string;
    join_url: string;
  } | null>(null);

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

    createMeeting.mutate(
      {
        title: title.trim(),
        start_time,
        end_time,
        timezone: getUserTimezone(),
        is_private: isPrivate,
        metadata: metadata.trim() || undefined,
      },
      {
        onSuccess: (response) => {
          setCreated({
            meeting_identifier: response.data.meeting_identifier,
            join_url: response.data.join_url,
          });
        },
        onError: (err: AxiosError<ApiErrorResponse>) => {
          const message =
            err.response?.data?.error?.message || "Failed to schedule meeting";
          setError(message);
        },
      },
    );
  };

  // Success state
  if (created) {
    const resetForm = () => {
      setCreated(null);
      setTitle("");
      setDate(getCurrentDate());
      setStartTime(getCurrentTime());
      setDuration("30");
      setMetadata("");
      setIsPrivate(false);
    };

    return (
      <MeetingSuccessView
        meetingIdentifier={created.meeting_identifier}
        joinUrl={created.join_url}
        title="Meeting Scheduled"
        description="Your meeting has been created successfully."
        showScheduleAnother={true}
        onScheduleAnother={resetForm}
        onBack={resetForm}
      />
    );
  }

  return (
    <div className="px-4 py-6 sm:px-6 sm:py-8 md:p-10 font-inter">
      {/* Header */}
      <div className="mb-6 sm:mb-8">
        <h2 className="text-xl sm:text-2xl font-bold text-foreground">
          Schedule a Meeting
        </h2>
        <p className="text-sm text-muted-foreground mt-1">
          Set up a new meeting and share the link with participants
        </p>
      </div>

      {/* Form */}
      <form onSubmit={handleSubmit} className="max-w-lg space-y-5">
        {/* Title */}
        <div className="space-y-1.5">
          <Label htmlFor="title" className="text-sm font-medium text-foreground">
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
          <Label htmlFor="date" className="text-sm font-medium text-foreground">
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
              className="text-sm font-medium text-foreground"
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
              className="text-sm font-medium text-foreground"
            >
              <Clock className="w-3.5 h-3.5 inline mr-1.5" />
              Duration
            </Label>
            <select
              id="duration"
              value={duration}
              onChange={(e) => setDuration(e.target.value)}
              className="flex h-9 sm:h-10 w-full rounded-xl border border-[var(--primary)4D] bg-transparent px-3 sm:px-5 py-2 text-sm sm:text-base font-medium focus:outline-none focus:border-[var(--primary)] focus:ring-1 focus:ring-[var(--primary)]"
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
          <Label className="text-sm font-medium text-foreground">
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
                        ? "bg-primary text-white shadow-sm"
                        : "bg-sidebar text-muted-foreground hover:bg-muted"
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
                        ? "bg-primary text-white shadow-sm"
                        : "bg-sidebar text-muted-foreground hover:bg-muted"
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
            className="text-sm font-medium text-foreground"
          >
            Description{" "}
            <span className="text-muted-foreground font-normal">(optional)</span>
          </Label>
          <textarea
            id="metadata"
            placeholder="Add a description or agenda..."
            value={metadata}
            onChange={(e) => setMetadata(e.target.value)}
            rows={3}
            className="flex w-full rounded-xl border border-[var(--primary)4D] bg-transparent px-3 sm:px-5 py-2 sm:py-3 text-sm sm:text-base font-medium placeholder:text-[var(--primary)4D] focus:outline-none focus:border-[var(--primary)] focus:ring-1 focus:ring-[var(--primary)] resize-none"
          />
        </div>

        {/* Error */}
        {error && <p className="text-sm text-red-500 font-medium">{error}</p>}

        {/* Submit */}
        <Button
          type="submit"
          size="lg"
          className="w-full"
          loading={createMeeting.isPending}
        >
          Schedule Meeting
        </Button>
      </form>
    </div>
  );
}
