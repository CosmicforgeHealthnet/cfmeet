"use client";

import { useState } from "react";
import { useCreateSpace } from "@/hooks/v2";
import { conferencesService } from "@/services/v2";
import type {
  MeetingType,
  AccessPolicy,
  WaitingRoomPolicy,
} from "@/types/v2";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import {
  Calendar,
  Clock,
  Lock,
  Globe,
  Users,
  Video,
  Shield,
  Presentation,
} from "lucide-react";
import { getUserTimezone } from "@/utils/timezone";
import { getErrorMessage } from "@/utils/error";
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

const MEETING_TYPES: { value: MeetingType; label: string; icon: React.ReactNode; description: string }[] = [
  {
    value: "STANDARD",
    label: "Standard",
    icon: <Video className="w-4 h-4" />,
    description: "Regular meetings for teams and groups",
  },
  {
    value: "WEBINAR",
    label: "Webinar",
    icon: <Presentation className="w-4 h-4" />,
    description: "One-to-many broadcasts with viewer controls",
  },
  {
    value: "TOWN_HALL",
    label: "Town Hall",
    icon: <Users className="w-4 h-4" />,
    description: "Large company-wide or public sessions",
  },
  {
    value: "TRAINING",
    label: "Training",
    icon: <Calendar className="w-4 h-4" />,
    description: "Educational sessions and workshops",
  },
];

const ACCESS_POLICIES: { value: AccessPolicy; label: string; icon: React.ReactNode; description: string }[] = [
  {
    value: "PUBLIC",
    label: "Public",
    icon: <Globe className="w-4 h-4" />,
    description: "Anyone with the link can join",
  },
  {
    value: "PUBLIC_PASSCODE",
    label: "Passcode",
    icon: <Lock className="w-4 h-4" />,
    description: "Requires a passcode to join",
  },
  {
    value: "MEMBERS_ONLY",
    label: "Members Only",
    icon: <Shield className="w-4 h-4" />,
    description: "Only invited members can join",
  },
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
  const createSpace = useCreateSpace();

  const [title, setTitle] = useState("");
  const [date, setDate] = useState(getCurrentDate);
  const [startTime, setStartTime] = useState(getCurrentTime);
  const [duration, setDuration] = useState("30");
  const [meetingType, setMeetingType] = useState<MeetingType>("STANDARD");
  const [accessPolicy, setAccessPolicy] = useState<AccessPolicy>("PUBLIC");
  const [waitingRoom, setWaitingRoom] = useState<WaitingRoomPolicy>("OFF");
  const [passcode, setPasscode] = useState("");
  const [description, setDescription] = useState("");
  const [error, setError] = useState("");
  const [created, setCreated] = useState<{
    meetingCode: string;
    spaceId: string;
  } | null>(null);
  const [isCreatingConference, setIsCreatingConference] = useState(false);

  const handleSubmit = async (e: React.FormEvent) => {
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

    // Validate passcode if policy requires it
    if (accessPolicy === "PUBLIC_PASSCODE") {
      const trimmedPasscode = passcode.trim();
      if (!trimmedPasscode) {
        setError("Passcode is required for passcode-protected meetings");
        return;
      }
      if (trimmedPasscode.length < 4 || trimmedPasscode.length > 64) {
        setError("Passcode must be between 4 and 64 characters");
        return;
      }
    }

    try {
      // Create the meeting space
      const space = await createSpace.mutateAsync({
        title: title.trim(),
        description: description.trim() || undefined,
        meetingType,
        timezone: getUserTimezone(),
        accessPolicy,
        waitingRoomPolicy: waitingRoom,
        passcode: accessPolicy === "PUBLIC_PASSCODE" ? passcode.trim() : undefined,
      });

      // Calculate scheduled times for the conference
      const scheduledStartAt = new Date(`${date}T${startTime}:00`).toISOString();
      const endDate = new Date(
        new Date(`${date}T${startTime}:00`).getTime() + parseInt(duration) * 60000
      );
      const scheduledEndAt = endDate.toISOString();

      // Create a scheduled conference using the service directly
      setIsCreatingConference(true);
      await conferencesService.create(space.id, {
        scheduledStartAt,
        scheduledEndAt,
      });

      setCreated({
        meetingCode: space.meetingCode,
        spaceId: space.id,
      });
    } catch (err) {
      setError(getErrorMessage(err, "Failed to schedule meeting"));
    } finally {
      setIsCreatingConference(false);
    }
  };

  const isPending = createSpace.isPending || isCreatingConference;

  // Success state
  if (created) {
    const resetForm = () => {
      setCreated(null);
      setTitle("");
      setDate(getCurrentDate());
      setStartTime(getCurrentTime());
      setDuration("30");
      setMeetingType("STANDARD");
      setAccessPolicy("PUBLIC");
      setWaitingRoom("OFF");
      setPasscode("");
      setDescription("");
    };

    const joinUrl =
      typeof window !== "undefined"
        ? `${window.location.origin}/${created.meetingCode}`
        : "";

    return (
      <MeetingSuccessView
        meetingIdentifier={created.meetingCode}
        joinUrl={joinUrl}
        title="Meeting Space Created"
        description="Your meeting space has been created and scheduled successfully."
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
          Create Meeting Space
        </h2>
        <p className="text-sm text-muted-foreground mt-1">
          Set up a new meeting space and schedule your first conference
        </p>
      </div>

      {/* Form */}
      <form onSubmit={handleSubmit} className="max-w-xl space-y-6">
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

        {/* Meeting Type */}
        <div className="space-y-2">
          <Label className="text-sm font-medium text-foreground">
            Meeting Type
          </Label>
          <TooltipProvider>
            <div className="grid grid-cols-3 gap-3">
              {MEETING_TYPES.map((type) => (
                <Tooltip key={type.value}>
                  <TooltipTrigger asChild>
                    <button
                      type="button"
                      onClick={() => setMeetingType(type.value)}
                      className={`flex flex-col items-center gap-2 px-4 py-3 rounded-xl text-sm font-medium transition-all ${
                        meetingType === type.value
                          ? "bg-primary text-white shadow-sm"
                          : "bg-sidebar text-muted-foreground hover:bg-muted"
                      }`}
                    >
                      {type.icon}
                      {type.label}
                    </button>
                  </TooltipTrigger>
                  <TooltipContent>
                    <p className="max-w-xs">{type.description}</p>
                  </TooltipContent>
                </Tooltip>
              ))}
            </div>
          </TooltipProvider>
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
              className="flex h-9 sm:h-10 w-full rounded-xl border border-input bg-background text-foreground px-3 sm:px-5 py-2 text-sm sm:text-base font-medium focus:outline-none focus:border-primary focus:ring-1 focus:ring-primary cursor-pointer"
            >
              {DURATION_OPTIONS.map((option) => (
                <option
                  key={option.value}
                  value={option.value}
                  className="bg-background text-foreground"
                >
                  {option.label}
                </option>
              ))}
            </select>
          </div>
        </div>

        {/* Access Policy */}
        <div className="space-y-2">
          <Label className="text-sm font-medium text-foreground">
            Access Control
          </Label>
          <TooltipProvider>
            <div className="flex flex-wrap gap-3">
              {ACCESS_POLICIES.map((policy) => (
                <Tooltip key={policy.value}>
                  <TooltipTrigger asChild>
                    <button
                      type="button"
                      onClick={() => setAccessPolicy(policy.value)}
                      className={`flex items-center gap-2 px-4 py-2.5 rounded-xl text-sm font-medium transition-all ${
                        accessPolicy === policy.value
                          ? "bg-primary text-white shadow-sm"
                          : "bg-sidebar text-muted-foreground hover:bg-muted"
                      }`}
                    >
                      {policy.icon}
                      {policy.label}
                    </button>
                  </TooltipTrigger>
                  <TooltipContent>
                    <p className="max-w-xs">{policy.description}</p>
                  </TooltipContent>
                </Tooltip>
              ))}
            </div>
          </TooltipProvider>
        </div>

        {/* Passcode (shown when PUBLIC_PASSCODE is selected) */}
        {accessPolicy === "PUBLIC_PASSCODE" && (
          <div className="space-y-1.5">
            <Label htmlFor="passcode" className="text-sm font-medium text-foreground">
              <Lock className="w-3.5 h-3.5 inline mr-1.5" />
              Passcode
            </Label>
            <Input
              id="passcode"
              type="text"
              placeholder="Enter a passcode (4-64 characters)"
              value={passcode}
              onChange={(e) => setPasscode(e.target.value)}
              minLength={4}
              maxLength={64}
            />
            <p className="text-xs text-muted-foreground">
              Passcode must be 4-64 characters. Participants will need this to join.
            </p>
          </div>
        )}

        {/* Waiting Room */}
        <div className="space-y-2">
          <Label className="text-sm font-medium text-foreground">
            Waiting Room
          </Label>
          <div className="flex gap-3">
            <button
              type="button"
              onClick={() => setWaitingRoom("OFF")}
              className={`flex items-center gap-2 px-4 py-2.5 rounded-xl text-sm font-medium transition-all ${
                waitingRoom === "OFF"
                  ? "bg-primary text-white shadow-sm"
                  : "bg-sidebar text-muted-foreground hover:bg-muted"
              }`}
            >
              Off
            </button>
            <button
              type="button"
              onClick={() => setWaitingRoom("GUESTS_ONLY")}
              className={`flex items-center gap-2 px-4 py-2.5 rounded-xl text-sm font-medium transition-all ${
                waitingRoom === "GUESTS_ONLY"
                  ? "bg-primary text-white shadow-sm"
                  : "bg-sidebar text-muted-foreground hover:bg-muted"
              }`}
            >
              Guests Only
            </button>
            <button
              type="button"
              onClick={() => setWaitingRoom("EVERYONE")}
              className={`flex items-center gap-2 px-4 py-2.5 rounded-xl text-sm font-medium transition-all ${
                waitingRoom === "EVERYONE"
                  ? "bg-primary text-white shadow-sm"
                  : "bg-sidebar text-muted-foreground hover:bg-muted"
              }`}
            >
              Everyone
            </button>
          </div>
          <p className="text-xs text-muted-foreground">
            {waitingRoom === "OFF"
              ? "Participants join immediately without waiting."
              : waitingRoom === "GUESTS_ONLY"
                ? "Only guests will wait for host approval."
                : "All participants must be approved by the host."}
          </p>
        </div>

        {/* Description */}
        <div className="space-y-1.5">
          <Label
            htmlFor="description"
            className="text-sm font-medium text-foreground"
          >
            Description{" "}
            <span className="text-muted-foreground font-normal">(optional)</span>
          </Label>
          <textarea
            id="description"
            placeholder="Add a description or agenda..."
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            rows={3}
            className="flex w-full rounded-xl border border-input bg-background text-foreground px-3 sm:px-5 py-2 sm:py-3 text-sm sm:text-base font-medium placeholder:text-muted-foreground focus:outline-none focus:border-primary focus:ring-1 focus:ring-primary resize-none"
          />
        </div>

        {/* Error */}
        {error && <p className="text-sm text-red-500 font-medium">{error}</p>}

        {/* Submit */}
        <Button
          type="submit"
          size="lg"
          className="w-full"
          loading={isPending}
        >
          Create Meeting Space
        </Button>
      </form>
    </div>
  );
}
