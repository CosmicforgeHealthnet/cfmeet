"use client";

import { useState } from "react";
import {
  BarChart3,
  Plus,
  Play,
  Square,
  Check,
  X,
  Loader2,
  ChevronDown,
  ChevronUp,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  usePolls,
  usePoll,
  usePollResults,
  useCreatePoll,
  useOpenPoll,
  useClosePoll,
  useSubmitPollResponse,
} from "@/hooks/v2";
import type { PollView, PollType, PollStatus, PollOptionResult } from "@/types/v2";

interface PollsPanelProps {
  conferenceId: string;
  isHost: boolean;
  participantId: string;
  onClose: () => void;
}

export default function PollsPanel({
  conferenceId,
  isHost,
  participantId,
  onClose,
}: PollsPanelProps) {
  const { data, isLoading } = usePolls(conferenceId);
  const polls = data?.polls || [];

  const [showCreateModal, setShowCreateModal] = useState(false);
  const [selectedPollId, setSelectedPollId] = useState<string | null>(null);

  const activePoll = polls.find((p) => p.status === "OPEN");

  return (
    <div className="flex flex-col h-full bg-card rounded-3xl shadow-sm">
      {/* Header */}
      <div className="flex items-center justify-between px-6 py-4 border-b">
        <h3 className="text-lg font-semibold text-foreground flex items-center gap-2">
          <BarChart3 className="w-5 h-5 text-primary" />
          Polls
        </h3>
        <button
          onClick={onClose}
          className="p-1.5 rounded-full hover:bg-muted transition-colors"
        >
          <X className="w-4 h-4 text-foreground" />
        </button>
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto px-6 py-4">
        {isLoading ? (
          <div className="flex items-center justify-center py-10">
            <Loader2 className="w-6 h-6 text-primary animate-spin" />
          </div>
        ) : polls.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-10 text-center">
            <BarChart3 className="w-12 h-12 text-muted-foreground mb-3" />
            <p className="text-foreground font-medium">No polls yet</p>
            <p className="text-sm text-muted-foreground mt-1">
              {isHost
                ? "Create a poll to gather responses from participants."
                : "The host hasn't created any polls yet."}
            </p>
            {isHost && (
              <Button
                className="mt-4"
                onClick={() => setShowCreateModal(true)}
              >
                <Plus className="w-4 h-4 mr-2" />
                Create Poll
              </Button>
            )}
          </div>
        ) : (
          <div className="space-y-4">
            {/* Active poll prominently displayed */}
            {activePoll && (
              <ActivePollCard
                poll={activePoll}
                conferenceId={conferenceId}
                isHost={isHost}
                participantId={participantId}
              />
            )}

            {/* Other polls */}
            {polls
              .filter((p) => p.id !== activePoll?.id)
              .map((poll) => (
                <PollCard
                  key={poll.id}
                  poll={poll}
                  conferenceId={conferenceId}
                  isHost={isHost}
                  isExpanded={selectedPollId === poll.id}
                  onToggle={() =>
                    setSelectedPollId(selectedPollId === poll.id ? null : poll.id)
                  }
                />
              ))}
          </div>
        )}
      </div>

      {/* Footer Actions (Host only) */}
      {isHost && polls.length > 0 && (
        <div className="px-6 py-4 border-t">
          <Button
            variant="outline"
            size="sm"
            className="w-full"
            onClick={() => setShowCreateModal(true)}
          >
            <Plus className="w-4 h-4 mr-2" />
            Create New Poll
          </Button>
        </div>
      )}

      {/* Create Modal */}
      {showCreateModal && (
        <CreatePollModal
          conferenceId={conferenceId}
          onClose={() => setShowCreateModal(false)}
        />
      )}
    </div>
  );
}

// ─── Active Poll Card ─────────────────────────────────────────────────────────

interface ActivePollCardProps {
  poll: PollView;
  conferenceId: string;
  isHost: boolean;
  participantId: string;
}

function ActivePollCard({
  poll,
  conferenceId,
  isHost,
  participantId,
}: ActivePollCardProps) {
  const [selectedOptions, setSelectedOptions] = useState<string[]>([]);
  const { data: resultsData } = usePollResults(conferenceId, poll.id);
  const closePoll = useClosePoll(conferenceId);
  const submitResponse = useSubmitPollResponse(conferenceId, poll.id);

  const hasVoted = resultsData?.myResponse && resultsData.myResponse.length > 0;
  const showResults = hasVoted || poll.showResultsMode === "ALWAYS";

  const handleOptionClick = (optionId: string) => {
    if (hasVoted) return;

    if (poll.allowMultiple) {
      setSelectedOptions((prev) =>
        prev.includes(optionId)
          ? prev.filter((id) => id !== optionId)
          : [...prev, optionId]
      );
    } else {
      setSelectedOptions([optionId]);
    }
  };

  const handleSubmit = () => {
    if (selectedOptions.length === 0) return;
    submitResponse.mutate({ optionIds: selectedOptions });
  };

  const handleClose = () => {
    closePoll.mutate(poll.id);
  };

  return (
    <div className="rounded-xl border-2 border-primary bg-primary/5 p-4">
      <div className="flex items-start justify-between mb-3">
        <div>
          <span className="text-xs font-medium text-primary uppercase tracking-wide">
            Live Poll
          </span>
          <h4 className="font-semibold text-foreground mt-1">{poll.question}</h4>
        </div>
        {isHost && (
          <Button
            size="sm"
            variant="outline"
            onClick={handleClose}
            disabled={closePoll.isPending}
          >
            <Square className="w-3.5 h-3.5 mr-1" />
            Close
          </Button>
        )}
      </div>

      <div className="space-y-2">
        {poll.options.map((option) => {
          const result = resultsData?.options.find((r) => r.optionId === option.id);
          const isSelected = selectedOptions.includes(option.id);
          const wasMyChoice = resultsData?.myResponse?.includes(option.id);

          return (
            <button
              key={option.id}
              onClick={() => handleOptionClick(option.id)}
              disabled={hasVoted}
              className={`w-full text-left p-3 rounded-xl border transition-all ${
                isSelected
                  ? "border-primary bg-primary/10"
                  : wasMyChoice
                    ? "border-primary/50 bg-primary/5"
                    : "border-border hover:border-primary/50"
              } ${hasVoted ? "cursor-default" : "cursor-pointer"}`}
            >
              <div className="flex items-center justify-between">
                <span className="text-sm font-medium text-foreground">
                  {option.text}
                </span>
                {(isSelected || wasMyChoice) && (
                  <Check className="w-4 h-4 text-primary" />
                )}
              </div>
              {showResults && result && (
                <div className="mt-2">
                  <div className="h-2 bg-muted rounded-full overflow-hidden">
                    <div
                      className="h-full bg-primary transition-all"
                      style={{ width: `${result.percentage}%` }}
                    />
                  </div>
                  <div className="flex justify-between mt-1 text-xs text-muted-foreground">
                    <span>{result.count} votes</span>
                    <span>{result.percentage.toFixed(0)}%</span>
                  </div>
                </div>
              )}
            </button>
          );
        })}
      </div>

      {!hasVoted && (
        <Button
          className="w-full mt-4"
          onClick={handleSubmit}
          disabled={selectedOptions.length === 0 || submitResponse.isPending}
          loading={submitResponse.isPending}
        >
          Submit Vote
        </Button>
      )}

      <p className="text-xs text-muted-foreground text-center mt-3">
        {resultsData?.totalResponses || 0} responses
      </p>
    </div>
  );
}

// ─── Poll Card ────────────────────────────────────────────────────────────────

interface PollCardProps {
  poll: PollView;
  conferenceId: string;
  isHost: boolean;
  isExpanded: boolean;
  onToggle: () => void;
}

function PollCard({
  poll,
  conferenceId,
  isHost,
  isExpanded,
  onToggle,
}: PollCardProps) {
  const { data: resultsData } = usePollResults(conferenceId, poll.id);
  const openPoll = useOpenPoll(conferenceId);

  const statusColors: Record<PollStatus, string> = {
    DRAFT: "bg-yellow-100 text-yellow-700 dark:bg-yellow-900/30 dark:text-yellow-400",
    OPEN: "bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400",
    CLOSED: "bg-muted text-muted-foreground",
    ARCHIVED: "bg-muted text-muted-foreground",
  };

  const handleOpen = () => {
    openPoll.mutate(poll.id);
  };

  return (
    <div className="rounded-xl border border-border p-4">
      <div
        className="flex items-center justify-between cursor-pointer"
        onClick={onToggle}
      >
        <div className="flex-1">
          <div className="flex items-center gap-2">
            <h4 className="font-medium text-foreground truncate">
              {poll.question}
            </h4>
            <span
              className={`text-[10px] px-1.5 py-0.5 rounded-full font-medium ${statusColors[poll.status]}`}
            >
              {poll.status}
            </span>
          </div>
          <p className="text-xs text-muted-foreground mt-1">
            {poll.responseCount} responses
          </p>
        </div>
        {isExpanded ? (
          <ChevronUp className="w-4 h-4 text-muted-foreground" />
        ) : (
          <ChevronDown className="w-4 h-4 text-muted-foreground" />
        )}
      </div>

      {isExpanded && (
        <div className="mt-4 space-y-3">
          {resultsData?.options.map((result) => (
            <div key={result.optionId}>
              <div className="flex justify-between text-sm mb-1">
                <span className="text-foreground">{result.text}</span>
                <span className="text-muted-foreground">
                  {result.percentage.toFixed(0)}%
                </span>
              </div>
              <div className="h-2 bg-muted rounded-full overflow-hidden">
                <div
                  className="h-full bg-primary transition-all"
                  style={{ width: `${result.percentage}%` }}
                />
              </div>
            </div>
          ))}

          {isHost && poll.status === "DRAFT" && (
            <Button
              size="sm"
              className="w-full mt-2"
              onClick={handleOpen}
              disabled={openPoll.isPending}
            >
              <Play className="w-3.5 h-3.5 mr-1" />
              Open Poll
            </Button>
          )}
        </div>
      )}
    </div>
  );
}

// ─── Create Poll Modal ────────────────────────────────────────────────────────

interface CreatePollModalProps {
  conferenceId: string;
  onClose: () => void;
}

function CreatePollModal({ conferenceId, onClose }: CreatePollModalProps) {
  const [question, setQuestion] = useState("");
  const [options, setOptions] = useState(["", ""]);
  const [pollType, setPollType] = useState<PollType>("SINGLE_CHOICE");
  const [allowMultiple, setAllowMultiple] = useState(false);

  const createPoll = useCreatePoll(conferenceId);

  const handleAddOption = () => {
    if (options.length < 10) {
      setOptions([...options, ""]);
    }
  };

  const handleRemoveOption = (index: number) => {
    if (options.length > 2) {
      setOptions(options.filter((_, i) => i !== index));
    }
  };

  const handleOptionChange = (index: number, value: string) => {
    const newOptions = [...options];
    newOptions[index] = value;
    setOptions(newOptions);
  };

  const handleCreate = () => {
    const validOptions = options.filter((o) => o.trim());
    if (!question.trim() || validOptions.length < 2) return;

    createPoll.mutate(
      {
        pollType,
        question: question.trim(),
        options: validOptions.map((text) => ({ text })),
        allowMultiple,
      },
      {
        onSuccess: () => {
          onClose();
        },
      }
    );
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      <div className="absolute inset-0 bg-black/40" onClick={onClose} />
      <div className="relative bg-card rounded-2xl shadow-xl w-full max-w-md mx-4 p-6 max-h-[90vh] overflow-y-auto">
        <h3 className="text-lg font-bold text-foreground mb-4">
          Create Poll
        </h3>

        <div className="space-y-4">
          <div>
            <Label htmlFor="question">Question</Label>
            <Input
              id="question"
              placeholder="What would you like to ask?"
              value={question}
              onChange={(e) => setQuestion(e.target.value)}
              className="mt-1.5"
            />
          </div>

          <div>
            <Label>Options</Label>
            <div className="space-y-2 mt-1.5">
              {options.map((option, index) => (
                <div key={index} className="flex gap-2">
                  <Input
                    placeholder={`Option ${index + 1}`}
                    value={option}
                    onChange={(e) => handleOptionChange(index, e.target.value)}
                  />
                  {options.length > 2 && (
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => handleRemoveOption(index)}
                    >
                      <X className="w-4 h-4" />
                    </Button>
                  )}
                </div>
              ))}
            </div>
            {options.length < 10 && (
              <Button
                variant="ghost"
                size="sm"
                className="mt-2"
                onClick={handleAddOption}
              >
                <Plus className="w-4 h-4 mr-1" />
                Add Option
              </Button>
            )}
          </div>

          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={allowMultiple}
              onChange={(e) => {
                setAllowMultiple(e.target.checked);
                setPollType(e.target.checked ? "MULTIPLE_CHOICE" : "SINGLE_CHOICE");
              }}
              className="rounded border-border"
            />
            <span className="text-sm text-foreground">
              Allow multiple answers
            </span>
          </label>
        </div>

        <div className="flex gap-3 mt-6">
          <Button variant="outline" className="flex-1" onClick={onClose}>
            Cancel
          </Button>
          <Button
            className="flex-1"
            onClick={handleCreate}
            disabled={!question.trim() || options.filter((o) => o.trim()).length < 2}
            loading={createPoll.isPending}
          >
            Create Poll
          </Button>
        </div>
      </div>
    </div>
  );
}
