"use client";

import { useState } from "react";
import {
  MessageCircleQuestion,
  ThumbsUp,
  Pin,
  CheckCircle2,
  X,
  Send,
  Loader2,
  EyeOff,
  Archive,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";
import {
  useQuestions,
  useSubmitQuestion,
  useVoteQuestion,
  useUnvoteQuestion,
  useAnswerQuestion,
  useDismissQuestion,
  usePinQuestion,
  useUnpinQuestion,
} from "@/hooks/v2";
import type { QuestionView, QuestionStatus } from "@/types/v2";

interface QAPanelProps {
  conferenceId: string;
  isHost: boolean;
  participantId: string;
  onClose: () => void;
}

export default function QAPanel({
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
          setNewQuestion("");
        },
      }
    );
  };

  return (
    <div className="flex flex-col h-full bg-card rounded-3xl shadow-sm">
      {/* Header */}
      <div className="flex items-center justify-between px-6 py-4 border-b">
        <h3 className="text-lg font-semibold text-foreground flex items-center gap-2">
          <MessageCircleQuestion className="w-5 h-5 text-primary" />
          Q&A
        </h3>
        <button
          onClick={onClose}
          className="p-1.5 rounded-full hover:bg-muted transition-colors"
        >
          <X className="w-4 h-4 text-foreground" />
        </button>
      </div>

      {/* Question Input */}
      <div className="px-6 py-4 border-b">
        <div className="flex gap-2">
          <input
            placeholder="Ask a question..."
            value={newQuestion}
            onChange={(e) => setNewQuestion(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleSubmitQuestion()}
            className="flex-1 h-10 bg-muted rounded-xl text-sm px-4 text-foreground focus:outline-none focus:ring-1 focus:ring-primary"
          />
          <Button
            size="icon"
            onClick={handleSubmitQuestion}
            disabled={!newQuestion.trim() || submitQuestion.isPending}
          >
            <Send className="w-4 h-4" />
          </Button>
        </div>
        <label className="flex items-center gap-2 mt-2 cursor-pointer">
          <input
            type="checkbox"
            checked={isAnonymous}
            onChange={(e) => setIsAnonymous(e.target.checked)}
            className="rounded border-border"
          />
          <span className="text-xs text-muted-foreground">
            Ask anonymously
          </span>
        </label>
      </div>

      {/* Questions List */}
      <div className="flex-1 overflow-y-auto px-6 py-4">
        {isLoading ? (
          <div className="flex items-center justify-center py-10">
            <Loader2 className="w-6 h-6 text-primary animate-spin" />
          </div>
        ) : questions.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-10 text-center">
            <MessageCircleQuestion className="w-12 h-12 text-muted-foreground mb-3" />
            <p className="text-foreground font-medium">No questions yet</p>
            <p className="text-sm text-muted-foreground mt-1">
              Be the first to ask a question!
            </p>
          </div>
        ) : (
          <div className="space-y-4">
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
          </div>
        )}
      </div>
    </div>
  );
}

// ─── Question Card ────────────────────────────────────────────────────────────

interface QuestionCardProps {
  question: QuestionView;
  conferenceId: string;
  isHost: boolean;
  participantId: string;
  isAnswering: boolean;
  onStartAnswer: () => void;
  onCancelAnswer: () => void;
}

function QuestionCard({
  question,
  conferenceId,
  isHost,
  participantId,
  isAnswering,
  onStartAnswer,
  onCancelAnswer,
}: QuestionCardProps) {
  const [answerText, setAnswerText] = useState("");

  const voteQuestion = useVoteQuestion(conferenceId);
  const unvoteQuestion = useUnvoteQuestion(conferenceId);
  const answerQuestion = useAnswerQuestion(conferenceId);
  const dismissQuestion = useDismissQuestion(conferenceId);
  const pinQuestion = usePinQuestion(conferenceId);
  const unpinQuestion = useUnpinQuestion(conferenceId);

  const handleVote = () => {
    if (question.hasMyVote) {
      unvoteQuestion.mutate(question.id);
    } else {
      voteQuestion.mutate(question.id);
    }
  };

  const handleAnswer = () => {
    if (!answerText.trim()) return;
    answerQuestion.mutate(
      { questionId: question.id, data: { answer: answerText.trim() } },
      {
        onSuccess: () => {
          onCancelAnswer();
          setAnswerText("");
        },
      }
    );
  };

  const handleDismiss = () => {
    dismissQuestion.mutate(question.id);
  };

  const handleTogglePin = () => {
    if (question.isPinned) {
      unpinQuestion.mutate(question.id);
    } else {
      pinQuestion.mutate(question.id);
    }
  };

  const statusColors: Record<QuestionStatus, string> = {
    PENDING: "",
    ANSWERED: "border-green-500/50 bg-green-50/50 dark:bg-green-900/10",
    DISMISSED: "opacity-60",
    ARCHIVED: "opacity-40",
  };

  return (
    <div
      className={`rounded-xl border p-4 transition-all ${
        question.isPinned
          ? "border-primary bg-primary/5"
          : `border-border ${statusColors[question.status]}`
      }`}
    >
      {/* Header */}
      <div className="flex items-start justify-between gap-2">
        <div className="flex items-center gap-2">
          {question.isAnonymous ? (
            <div className="w-8 h-8 rounded-full bg-muted flex items-center justify-center">
              <EyeOff className="w-4 h-4 text-muted-foreground" />
            </div>
          ) : (
            <Avatar className="w-8 h-8">
              <AvatarImage
                src={`https://api.dicebear.com/7.x/avataaars/svg?seed=${question.askerDisplayName}`}
              />
              <AvatarFallback>
                {question.askerDisplayName?.[0] || "?"}
              </AvatarFallback>
            </Avatar>
          )}
          <div>
            <span className="text-sm font-medium text-foreground">
              {question.isAnonymous
                ? "Anonymous"
                : question.askerDisplayName || "Unknown"}
            </span>
            {question.isPinned && (
              <span className="ml-2 text-xs text-primary font-medium">
                Pinned
              </span>
            )}
          </div>
        </div>
        {question.status === "ANSWERED" && (
          <span className="flex items-center gap-1 text-xs text-green-600 font-medium">
            <CheckCircle2 className="w-3.5 h-3.5" />
            Answered
          </span>
        )}
      </div>

      {/* Question Content */}
      <p className="text-sm text-foreground mt-3">{question.content}</p>

      {/* Answer */}
      {question.answer && (
        <div className="mt-3 p-3 rounded-lg bg-muted/50">
          <p className="text-xs text-muted-foreground font-medium mb-1">
            Answer:
          </p>
          <p className="text-sm text-foreground">{question.answer}</p>
        </div>
      )}

      {/* Answer Form */}
      {isAnswering && isHost && (
        <div className="mt-3 space-y-2">
          <textarea
            placeholder="Type your answer..."
            value={answerText}
            onChange={(e) => setAnswerText(e.target.value)}
            rows={2}
            className="w-full rounded-lg border border-border bg-transparent px-3 py-2 text-sm focus:outline-none focus:border-primary focus:ring-1 focus:ring-primary resize-none"
          />
          <div className="flex gap-2">
            <Button
              size="sm"
              variant="outline"
              onClick={onCancelAnswer}
              className="flex-1"
            >
              Cancel
            </Button>
            <Button
              size="sm"
              onClick={handleAnswer}
              disabled={!answerText.trim()}
              loading={answerQuestion.isPending}
              className="flex-1"
            >
              Answer
            </Button>
          </div>
        </div>
      )}

      {/* Actions */}
      <div className="flex items-center justify-between mt-3 pt-3 border-t border-border">
        {/* Vote */}
        <button
          onClick={handleVote}
          disabled={voteQuestion.isPending || unvoteQuestion.isPending}
          className={`flex items-center gap-1.5 text-sm font-medium transition-colors ${
            question.hasMyVote
              ? "text-primary"
              : "text-muted-foreground hover:text-foreground"
          }`}
        >
          <ThumbsUp className="w-4 h-4" />
          {question.upvotes}
        </button>

        {/* Host Actions */}
        {isHost && question.status !== "DISMISSED" && (
          <div className="flex gap-2">
            {!question.answer && !isAnswering && (
              <Button size="sm" variant="ghost" onClick={onStartAnswer}>
                Answer
              </Button>
            )}
            <Button
              size="sm"
              variant="ghost"
              onClick={handleTogglePin}
              disabled={pinQuestion.isPending || unpinQuestion.isPending}
              title={question.isPinned ? "Unpin" : "Pin"}
            >
              <Pin
                className={`w-4 h-4 ${question.isPinned ? "text-primary" : ""}`}
              />
            </Button>
            <Button
              size="sm"
              variant="ghost"
              onClick={handleDismiss}
              disabled={dismissQuestion.isPending}
              title="Dismiss"
            >
              <Archive className="w-4 h-4" />
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}
