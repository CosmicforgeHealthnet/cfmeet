import { useState } from "react";
import { View, ScrollView, Pressable, TextInput } from "react-native";
import { Ionicons } from "@expo/vector-icons";
import { Text } from "@/components/ui/text";
import { Button } from "@/components/ui/button";
import { Loading } from "@/components/shared/loading";
import { useToast } from "@/components/ui/toast";
import { useColors } from "@/hooks/useColors";
import {
  useQuestions,
  useSubmitQuestion,
  useVoteQuestion,
  useUnvoteQuestion,
  useAnswerQuestion,
  useDismissQuestion,
  usePinQuestion,
  useUnpinQuestion,
} from "@/features/meetings/v2";
import type { QuestionView, QuestionStatus } from "@/features/meetings/v2";

interface QAPanelProps {
  conferenceId: string;
  isHost: boolean;
  participantId: string;
  onClose: () => void;
}

const STATUS_COLORS: Record<QuestionStatus, string> = {
  PENDING: "",
  ANSWERED: "border-green-500/50 bg-green-50/50 dark:bg-green-900/10",
  DISMISSED: "opacity-60",
  ARCHIVED: "opacity-40",
};

export function QAPanel({
  conferenceId,
  isHost,
  participantId,
  onClose,
}: QAPanelProps) {
  const { data, isLoading } = useQuestions(conferenceId);
  const questions = data?.questions || [];

  const [newQuestion, setNewQuestion] = useState("");
  const [isAnonymous, setIsAnonymous] = useState(false);
  const [answeringId, setAnsweringId] = useState<string | null>(null);

  const submitQuestion = useSubmitQuestion(conferenceId);
  const { toast, success } = useToast();
  const colors = useColors();

  // Sort: pinned first, then by upvotes
  const sortedQuestions = [...questions].sort((a, b) => {
    if (a.isPinned !== b.isPinned) return a.isPinned ? -1 : 1;
    return b.upvotes - a.upvotes;
  });

  const handleSubmitQuestion = () => {
    if (!newQuestion.trim()) return;
    submitQuestion.mutate(
      {
        content: newQuestion.trim(),
        isAnonymous,
      },
      {
        onSuccess: () => {
          success("Question submitted!");
          setNewQuestion("");
        },
        onError: (e) => toast(e.message, "destructive"),
      }
    );
  };

  return (
    <View className="flex-1 bg-card dark:bg-card-dark">
      {/* Header */}
      <View className="flex-row items-center justify-between px-4 py-3 border-b border-border dark:border-border-dark">
        <View className="flex-row items-center gap-2">
          <Ionicons name="help-circle" size={20} color={colors.primary} />
          <Text className="text-lg font-bold text-foreground dark:text-foreground-dark">
            Q&A
          </Text>
        </View>
        <Pressable onPress={onClose} className="p-2 rounded-full">
          <Ionicons name="close" size={20} color={colors.foreground} />
        </Pressable>
      </View>

      {/* Question Input */}
      <View className="px-4 py-4 border-b border-border dark:border-border-dark">
        <View className="flex-row gap-2">
          <TextInput
            placeholder="Ask a question..."
            value={newQuestion}
            onChangeText={setNewQuestion}
            className="flex-1 h-10 bg-muted dark:bg-muted-dark rounded-xl text-sm px-4 text-foreground dark:text-foreground-dark"
            placeholderTextColor="#9ca3af"
          />
          <Pressable
            onPress={handleSubmitQuestion}
            disabled={!newQuestion.trim() || submitQuestion.isPending}
            className={`w-10 h-10 rounded-xl items-center justify-center ${
              !newQuestion.trim() || submitQuestion.isPending
                ? "bg-muted dark:bg-muted-dark"
                : "bg-primary dark:bg-primary-dark"
            }`}
          >
            <Ionicons
              name="send"
              size={18}
              color={!newQuestion.trim() || submitQuestion.isPending ? colors.mutedForeground : "#fff"}
            />
          </Pressable>
        </View>
        <Pressable
          onPress={() => setIsAnonymous(!isAnonymous)}
          className="flex-row items-center gap-2 mt-2"
        >
          <View
            className={`w-5 h-5 rounded border items-center justify-center ${
              isAnonymous
                ? "bg-primary dark:bg-primary-dark border-primary dark:border-primary-dark"
                : "border-border dark:border-border-dark"
            }`}
          >
            {isAnonymous && <Ionicons name="checkmark" size={14} color="#fff" />}
          </View>
          <Text className="text-xs text-muted-foreground dark:text-muted-foreground-dark">
            Ask anonymously
          </Text>
        </Pressable>
      </View>

      {/* Questions List */}
      <ScrollView className="flex-1 px-4 py-4">
        {isLoading ? (
          <Loading message="Loading questions..." />
        ) : questions.length === 0 ? (
          <View className="items-center py-10">
            <Ionicons name="help-circle-outline" size={48} color={colors.mutedForeground} style={{ opacity: 0.5 }} />
            <Text className="mt-4 text-base font-bold text-foreground dark:text-foreground-dark">
              No questions yet
            </Text>
            <Text className="mt-1 text-sm text-muted-foreground dark:text-muted-foreground-dark text-center">
              Be the first to ask a question!
            </Text>
          </View>
        ) : (
          <View className="gap-4">
            {sortedQuestions.map((question) => (
              <QuestionCard
                key={question.id}
                question={question}
                conferenceId={conferenceId}
                isHost={isHost}
                participantId={participantId}
                isAnswering={answeringId === question.id}
                onStartAnswer={() => setAnsweringId(question.id)}
                onCancelAnswer={() => setAnsweringId(null)}
              />
            ))}
          </View>
        )}
      </ScrollView>
    </View>
  );
}

// Question Card
function QuestionCard({
  question,
  conferenceId,
  isHost,
  participantId,
  isAnswering,
  onStartAnswer,
  onCancelAnswer,
}: {
  question: QuestionView;
  conferenceId: string;
  isHost: boolean;
  participantId: string;
  isAnswering: boolean;
  onStartAnswer: () => void;
  onCancelAnswer: () => void;
}) {
  const [answerText, setAnswerText] = useState("");

  const voteQuestion = useVoteQuestion(conferenceId);
  const unvoteQuestion = useUnvoteQuestion(conferenceId);
  const answerQuestion = useAnswerQuestion(conferenceId);
  const dismissQuestion = useDismissQuestion(conferenceId);
  const pinQuestion = usePinQuestion(conferenceId);
  const unpinQuestion = useUnpinQuestion(conferenceId);

  const { toast, success } = useToast();
  const colors = useColors();

  const handleVote = () => {
    if (question.hasMyVote) {
      unvoteQuestion.mutate(question.id, {
        onError: (e) => toast(e.message, "destructive"),
      });
    } else {
      voteQuestion.mutate(question.id, {
        onError: (e) => toast(e.message, "destructive"),
      });
    }
  };

  const handleAnswer = () => {
    if (!answerText.trim()) return;
    answerQuestion.mutate(
      { questionId: question.id, data: { answer: answerText.trim() } },
      {
        onSuccess: () => {
          success("Answer submitted!");
          onCancelAnswer();
          setAnswerText("");
        },
        onError: (e) => toast(e.message, "destructive"),
      }
    );
  };

  const handleDismiss = () => {
    dismissQuestion.mutate(question.id, {
      onSuccess: () => success("Question dismissed"),
      onError: (e) => toast(e.message, "destructive"),
    });
  };

  const handleTogglePin = () => {
    if (question.isPinned) {
      unpinQuestion.mutate(question.id, {
        onSuccess: () => success("Question unpinned"),
        onError: (e) => toast(e.message, "destructive"),
      });
    } else {
      pinQuestion.mutate(question.id, {
        onSuccess: () => success("Question pinned"),
        onError: (e) => toast(e.message, "destructive"),
      });
    }
  };

  return (
    <View
      className={`rounded-xl border p-4 ${
        question.isPinned
          ? "border-primary dark:border-primary-dark bg-primary/5 dark:bg-primary-dark/5"
          : `border-border dark:border-border-dark ${STATUS_COLORS[question.status]}`
      }`}
    >
      {/* Header */}
      <View className="flex-row items-start justify-between gap-2">
        <View className="flex-row items-center gap-2 flex-1">
          {question.isAnonymous ? (
            <View className="w-8 h-8 rounded-full bg-muted dark:bg-muted-dark items-center justify-center">
              <Ionicons name="eye-off" size={16} color={colors.mutedForeground} />
            </View>
          ) : (
            <View className="w-8 h-8 rounded-full bg-primary/10 dark:bg-primary-dark/10 items-center justify-center">
              <Text className="text-sm font-bold text-primary dark:text-primary-dark">
                {question.askerDisplayName?.[0] || "?"}
              </Text>
            </View>
          )}
          <View className="flex-1">
            <View className="flex-row items-center gap-2">
              <Text className="text-sm font-medium text-foreground dark:text-foreground-dark">
                {question.isAnonymous ? "Anonymous" : question.askerDisplayName || "Unknown"}
              </Text>
              {question.isPinned && (
                <View className="flex-row items-center gap-1">
                  <Ionicons name="pin" size={12} color={colors.primary} />
                  <Text className="text-xs font-medium text-primary dark:text-primary-dark">
                    Pinned
                  </Text>
                </View>
              )}
            </View>
          </View>
        </View>
        {question.status === "ANSWERED" && (
          <View className="flex-row items-center gap-1 bg-green-100 dark:bg-green-900/30 px-2 py-1 rounded-full">
            <Ionicons name="checkmark-circle" size={14} color="#16a34a" />
            <Text className="text-xs font-medium text-green-600 dark:text-green-400">
              Answered
            </Text>
          </View>
        )}
      </View>

      {/* Question Content */}
      <Text className="text-sm text-foreground dark:text-foreground-dark mt-3">
        {question.content}
      </Text>

      {/* Answer */}
      {question.answer && (
        <View className="mt-3 p-3 rounded-lg bg-muted/50 dark:bg-muted-dark/50">
          <Text className="text-xs text-muted-foreground dark:text-muted-foreground-dark font-medium mb-1">
            Answer:
          </Text>
          <Text className="text-sm text-foreground dark:text-foreground-dark">
            {question.answer}
          </Text>
        </View>
      )}

      {/* Answer Form */}
      {isAnswering && isHost && (
        <View className="mt-3 gap-2">
          <TextInput
            placeholder="Type your answer..."
            value={answerText}
            onChangeText={setAnswerText}
            multiline
            numberOfLines={2}
            className="w-full rounded-lg border border-border dark:border-border-dark bg-transparent px-3 py-2 text-sm text-foreground dark:text-foreground-dark"
            placeholderTextColor="#9ca3af"
          />
          <View className="flex-row gap-2">
            <Button
              size="sm"
              variant="outline"
              onPress={onCancelAnswer}
              className="flex-1"
            >
              Cancel
            </Button>
            <Button
              size="sm"
              onPress={handleAnswer}
              disabled={!answerText.trim()}
              isLoading={answerQuestion.isPending}
              className="flex-1"
            >
              Answer
            </Button>
          </View>
        </View>
      )}

      {/* Actions */}
      <View className="flex-row items-center justify-between mt-3 pt-3 border-t border-border dark:border-border-dark">
        {/* Vote */}
        <Pressable
          onPress={handleVote}
          disabled={voteQuestion.isPending || unvoteQuestion.isPending}
          className="flex-row items-center gap-1.5"
        >
          <Ionicons
            name={question.hasMyVote ? "thumbs-up" : "thumbs-up-outline"}
            size={16}
            color={question.hasMyVote ? colors.primary : colors.mutedForeground}
          />
          <Text
            className={`text-sm font-medium ${
              question.hasMyVote
                ? "text-primary dark:text-primary-dark"
                : "text-muted-foreground dark:text-muted-foreground-dark"
            }`}
          >
            {question.upvotes}
          </Text>
        </Pressable>

        {/* Host Actions */}
        {isHost && question.status !== "DISMISSED" && (
          <View className="flex-row gap-2">
            {!question.answer && !isAnswering && (
              <Pressable
                onPress={onStartAnswer}
                className="px-3 py-1.5 rounded-lg bg-muted dark:bg-muted-dark"
              >
                <Text className="text-sm font-medium text-foreground dark:text-foreground-dark">
                  Answer
                </Text>
              </Pressable>
            )}
            <Pressable
              onPress={handleTogglePin}
              disabled={pinQuestion.isPending || unpinQuestion.isPending}
              className="p-1.5"
            >
              <Ionicons
                name={question.isPinned ? "pin" : "pin-outline"}
                size={18}
                color={question.isPinned ? colors.primary : colors.mutedForeground}
              />
            </Pressable>
            <Pressable
              onPress={handleDismiss}
              disabled={dismissQuestion.isPending}
              className="p-1.5"
            >
              <Ionicons name="archive-outline" size={18} color={colors.mutedForeground} />
            </Pressable>
          </View>
        )}
      </View>
    </View>
  );
}
