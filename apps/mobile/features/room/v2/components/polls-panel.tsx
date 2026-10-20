import { useState } from "react";
import { View, ScrollView, Pressable, Modal, TextInput } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Loading } from "@/components/shared/loading";
import { useToast } from "@/components/ui/toast";
import { useColors } from "@/hooks/useColors";
import {
  usePolls,
  usePollResults,
  useCreatePoll,
  useOpenPoll,
  useClosePoll,
  useSubmitPollResponse,
} from "@/features/meetings/v2";
import type { PollView, PollStatus, PollType } from "@/features/meetings/v2";

interface PollsPanelProps {
  conferenceId: string;
  isHost: boolean;
  participantId: string;
  onClose: () => void;
}

const STATUS_COLORS: Record<PollStatus, { bg: string; text: string }> = {
  DRAFT: { bg: "bg-yellow-100 dark:bg-yellow-900/30", text: "text-yellow-700 dark:text-yellow-400" },
  OPEN: { bg: "bg-green-100 dark:bg-green-900/30", text: "text-green-700 dark:text-green-400" },
  CLOSED: { bg: "bg-gray-100 dark:bg-gray-900/30", text: "text-gray-700 dark:text-gray-400" },
  ARCHIVED: { bg: "bg-gray-100 dark:bg-gray-900/30", text: "text-gray-500 dark:text-gray-500" },
};

export function PollsPanel({
  conferenceId,
  isHost,
  participantId,
  onClose,
}: PollsPanelProps) {
  const { data, isLoading } = usePolls(conferenceId);
  const polls = data?.polls || [];

  const [showCreateModal, setShowCreateModal] = useState(false);
  const [selectedPollId, setSelectedPollId] = useState<string | null>(null);
  const colors = useColors();

  const activePoll = polls.find((p) => p.status === "OPEN");

  return (
    <View className="flex-1 bg-card dark:bg-card-dark">
      {/* Header */}
      <View className="flex-row items-center justify-between px-4 py-3 border-b border-border dark:border-border-dark">
        <View className="flex-row items-center gap-2">
          <Ionicons name="bar-chart" size={20} color={colors.primary} />
          <Text className="text-lg font-bold text-foreground dark:text-foreground-dark">
            Polls
          </Text>
        </View>
        <Pressable onPress={onClose} className="p-2 rounded-full">
          <Ionicons name="close" size={20} color={colors.foreground} />
        </Pressable>
      </View>

      {/* Content */}
      <ScrollView className="flex-1 px-4 py-4">
        {isLoading ? (
          <Loading message="Loading polls..." />
        ) : polls.length === 0 ? (
          <View className="items-center py-10">
            <Ionicons name="bar-chart-outline" size={48} color={colors.mutedForeground} style={{ opacity: 0.5 }} />
            <Text className="mt-4 text-base font-bold text-foreground dark:text-foreground-dark">
              No polls yet
            </Text>
            <Text className="mt-1 text-sm text-muted-foreground dark:text-muted-foreground-dark text-center">
              {isHost
                ? "Create a poll to gather responses from participants."
                : "The host hasn't created any polls yet."}
            </Text>
            {isHost && (
              <Button className="mt-4" onPress={() => setShowCreateModal(true)}>
                <Ionicons name="add" size={16} color="#fff" />
                <Text className="text-primary-foreground dark:text-primary-foreground-dark ml-1">
                  Create Poll
                </Text>
              </Button>
            )}
          </View>
        ) : (
          <View className="gap-4">
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
          </View>
        )}
      </ScrollView>

      {/* Footer Actions (Host only) */}
      {isHost && polls.length > 0 && (
        <View className="px-4 py-4 border-t border-border dark:border-border-dark">
          <Button
            variant="outline"
            size="sm"
            onPress={() => setShowCreateModal(true)}
          >
            <Ionicons name="add" size={16} color={colors.primary} />
            <Text className="text-primary dark:text-primary-dark ml-1">Create New Poll</Text>
          </Button>
        </View>
      )}

      {/* Create Modal */}
      {showCreateModal && (
        <CreatePollModal
          conferenceId={conferenceId}
          onClose={() => setShowCreateModal(false)}
        />
      )}
    </View>
  );
}

// Active Poll Card
function ActivePollCard({
  poll,
  conferenceId,
  isHost,
  participantId,
}: {
  poll: PollView;
  conferenceId: string;
  isHost: boolean;
  participantId: string;
}) {
  const [selectedOptions, setSelectedOptions] = useState<string[]>([]);
  const { data: resultsData } = usePollResults(conferenceId, poll.id);
  const closePoll = useClosePoll(conferenceId);
  const submitResponse = useSubmitPollResponse(conferenceId, poll.id);
  const { toast, success } = useToast();
  const colors = useColors();

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
    submitResponse.mutate(
      { optionIds: selectedOptions },
      {
        onSuccess: () => success("Vote submitted!"),
        onError: (e) => toast(e.message, "destructive"),
      }
    );
  };

  const handleClose = () => {
    closePoll.mutate(poll.id, {
      onSuccess: () => success("Poll closed"),
      onError: (e) => toast(e.message, "destructive"),
    });
  };

  return (
    <View className="rounded-xl border-2 border-primary dark:border-primary-dark bg-primary/5 dark:bg-primary-dark/5 p-4">
      <View className="flex-row items-start justify-between mb-3">
        <View className="flex-1">
          <Text className="text-xs font-bold text-primary dark:text-primary-dark uppercase tracking-wide">
            Live Poll
          </Text>
          <Text className="font-bold text-foreground dark:text-foreground-dark mt-1">
            {poll.question}
          </Text>
        </View>
        {isHost && (
          <Button
            size="sm"
            variant="outline"
            onPress={handleClose}
            isLoading={closePoll.isPending}
          >
            <Ionicons name="stop" size={14} color={colors.primary} />
            <Text className="text-primary dark:text-primary-dark ml-1">Close</Text>
          </Button>
        )}
      </View>

      <View className="gap-2">
        {poll.options.map((option) => {
          const result = resultsData?.options.find((r) => r.optionId === option.id);
          const isSelected = selectedOptions.includes(option.id);
          const wasMyChoice = resultsData?.myResponse?.includes(option.id);

          return (
            <Pressable
              key={option.id}
              onPress={() => handleOptionClick(option.id)}
              disabled={hasVoted}
              className={`p-3 rounded-xl border ${
                isSelected
                  ? "border-primary dark:border-primary-dark bg-primary/10 dark:bg-primary-dark/10"
                  : wasMyChoice
                    ? "border-primary/50 dark:border-primary-dark/50 bg-primary/5 dark:bg-primary-dark/5"
                    : "border-border dark:border-border-dark"
              }`}
            >
              <View className="flex-row items-center justify-between">
                <Text className="text-sm font-medium text-foreground dark:text-foreground-dark">
                  {option.text}
                </Text>
                {(isSelected || wasMyChoice) && (
                  <Ionicons name="checkmark" size={18} color={colors.primary} />
                )}
              </View>
              {showResults && result && (
                <View className="mt-2">
                  <View className="h-2 bg-muted dark:bg-muted-dark rounded-full overflow-hidden">
                    <View
                      className="h-full bg-primary dark:bg-primary-dark"
                      style={{ width: `${result.percentage}%` }}
                    />
                  </View>
                  <View className="flex-row justify-between mt-1">
                    <Text className="text-xs text-muted-foreground dark:text-muted-foreground-dark">
                      {result.count} votes
                    </Text>
                    <Text className="text-xs text-muted-foreground dark:text-muted-foreground-dark">
                      {result.percentage.toFixed(0)}%
                    </Text>
                  </View>
                </View>
              )}
            </Pressable>
          );
        })}
      </View>

      {!hasVoted && (
        <Button
          className="mt-4"
          onPress={handleSubmit}
          disabled={selectedOptions.length === 0}
          isLoading={submitResponse.isPending}
        >
          Submit Vote
        </Button>
      )}

      <Text className="text-xs text-muted-foreground dark:text-muted-foreground-dark text-center mt-3">
        {resultsData?.totalResponses || 0} responses
      </Text>
    </View>
  );
}

// Poll Card
function PollCard({
  poll,
  conferenceId,
  isHost,
  isExpanded,
  onToggle,
}: {
  poll: PollView;
  conferenceId: string;
  isHost: boolean;
  isExpanded: boolean;
  onToggle: () => void;
}) {
  const { data: resultsData } = usePollResults(conferenceId, poll.id);
  const openPoll = useOpenPoll(conferenceId);
  const { toast, success } = useToast();
  const colors = useColors();

  const handleOpen = () => {
    openPoll.mutate(poll.id, {
      onSuccess: () => success("Poll opened"),
      onError: (e) => toast(e.message, "destructive"),
    });
  };

  return (
    <View className="rounded-xl border border-border dark:border-border-dark p-4">
      <Pressable
        className="flex-row items-center justify-between"
        onPress={onToggle}
      >
        <View className="flex-1 mr-2">
          <View className="flex-row items-center gap-2">
            <Text
              className="font-medium text-foreground dark:text-foreground-dark flex-1"
              numberOfLines={1}
            >
              {poll.question}
            </Text>
            <View className={`px-2 py-0.5 rounded-full ${STATUS_COLORS[poll.status].bg}`}>
              <Text className={`text-[10px] font-bold ${STATUS_COLORS[poll.status].text}`}>
                {poll.status}
              </Text>
            </View>
          </View>
          <Text className="text-xs text-muted-foreground dark:text-muted-foreground-dark mt-1">
            {poll.responseCount} responses
          </Text>
        </View>
        <Ionicons
          name={isExpanded ? "chevron-up" : "chevron-down"}
          size={18}
          color={colors.mutedForeground}
        />
      </Pressable>

      {isExpanded && (
        <View className="mt-4 gap-3">
          {resultsData?.options.map((result) => (
            <View key={result.optionId}>
              <View className="flex-row justify-between text-sm mb-1">
                <Text className="text-foreground dark:text-foreground-dark">
                  {result.text}
                </Text>
                <Text className="text-muted-foreground dark:text-muted-foreground-dark">
                  {result.percentage.toFixed(0)}%
                </Text>
              </View>
              <View className="h-2 bg-muted dark:bg-muted-dark rounded-full overflow-hidden">
                <View
                  className="h-full bg-primary dark:bg-primary-dark"
                  style={{ width: `${result.percentage}%` }}
                />
              </View>
            </View>
          ))}

          {isHost && poll.status === "DRAFT" && (
            <Button
              size="sm"
              className="mt-2"
              onPress={handleOpen}
              isLoading={openPoll.isPending}
            >
              <Ionicons name="play" size={14} color="#fff" />
              <Text className="text-white ml-1">Open Poll</Text>
            </Button>
          )}
        </View>
      )}
    </View>
  );
}

// Create Poll Modal
function CreatePollModal({
  conferenceId,
  onClose,
}: {
  conferenceId: string;
  onClose: () => void;
}) {
  const [question, setQuestion] = useState("");
  const [options, setOptions] = useState(["", ""]);
  const [pollType, setPollType] = useState<PollType>("SINGLE_CHOICE");
  const [allowMultiple, setAllowMultiple] = useState(false);

  const createPoll = useCreatePoll(conferenceId);
  const { toast, success } = useToast();
  const colors = useColors();

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
    if (!question.trim() || validOptions.length < 2) {
      toast("Please enter a question and at least 2 options", "destructive");
      return;
    }

    createPoll.mutate(
      {
        pollType,
        question: question.trim(),
        options: validOptions.map((text) => ({ text })),
        allowMultiple,
      },
      {
        onSuccess: () => {
          success("Poll created");
          onClose();
        },
        onError: (e) => toast(e.message, "destructive"),
      }
    );
  };

  return (
    <Modal visible animationType="slide" transparent onRequestClose={onClose}>
      <View className="flex-1 bg-black/50 justify-end">
        <View className="bg-card dark:bg-card-dark rounded-t-3xl p-6 max-h-[90%]">
          <ScrollView showsVerticalScrollIndicator={false}>
            <Text className="text-lg font-bold text-foreground dark:text-foreground-dark mb-4">
              Create Poll
            </Text>

            <Input
              label="Question"
              placeholder="What would you like to ask?"
              value={question}
              onChangeText={setQuestion}
            />

            <Text className="mt-4 mb-2 text-sm font-medium text-foreground dark:text-foreground-dark">
              Options
            </Text>
            <View className="gap-2">
              {options.map((option, index) => (
                <View key={index} className="flex-row gap-2">
                  <Input
                    placeholder={`Option ${index + 1}`}
                    value={option}
                    onChangeText={(value) => handleOptionChange(index, value)}
                    className="flex-1"
                  />
                  {options.length > 2 && (
                    <Pressable
                      onPress={() => handleRemoveOption(index)}
                      className="p-2 items-center justify-center"
                    >
                      <Ionicons name="close" size={20} color={colors.mutedForeground} />
                    </Pressable>
                  )}
                </View>
              ))}
            </View>

            {options.length < 10 && (
              <Pressable
                onPress={handleAddOption}
                className="flex-row items-center gap-1 mt-2"
              >
                <Ionicons name="add" size={16} color={colors.primary} />
                <Text className="text-sm font-medium text-primary dark:text-primary-dark">
                  Add Option
                </Text>
              </Pressable>
            )}

            <Pressable
              onPress={() => {
                setAllowMultiple(!allowMultiple);
                setPollType(!allowMultiple ? "MULTIPLE_CHOICE" : "SINGLE_CHOICE");
              }}
              className="flex-row items-center justify-between py-4 mt-4"
            >
              <Text className="text-foreground dark:text-foreground-dark">
                Allow multiple answers
              </Text>
              <View
                className={`w-11 h-6 rounded-full p-0.5 ${
                  allowMultiple ? "bg-primary dark:bg-primary-dark" : "bg-muted dark:bg-muted-dark"
                }`}
              >
                <View
                  className={`h-5 w-5 rounded-full bg-white ${allowMultiple ? "ml-5" : "ml-0"}`}
                />
              </View>
            </Pressable>

            <View className="flex-row gap-3 mt-4">
              <Button variant="outline" className="flex-1" onPress={onClose}>
                Cancel
              </Button>
              <Button
                className="flex-1"
                onPress={handleCreate}
                disabled={!question.trim() || options.filter((o) => o.trim()).length < 2}
                isLoading={createPoll.isPending}
              >
                Create Poll
              </Button>
            </View>
          </ScrollView>
        </View>
      </View>
    </Modal>
  );
}
