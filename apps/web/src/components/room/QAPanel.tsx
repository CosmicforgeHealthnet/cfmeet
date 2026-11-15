"use client";

import { useState } from "react";
import { ArrowUp, Check, CheckCircle2, Eye, EyeOff, MessageCircleQuestion, Pin, PinOff, Trash2, VenetianMask } from "lucide-react";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { cn } from "@/lib/utils";
import {
  useQuestions,
  useSubmitQuestion,
  useVoteQuestion,
  useUnvoteQuestion,
  useAnswerQuestion,
  useDismissQuestion,
  useRestoreQuestion,
  useArchiveQuestion,
  usePinQuestion,
  useUnpinQuestion,
} from "@/hooks/v2";
import type { QuestionView } from "@/types/v2";

interface QAPanelProps {
  conferenceId: string;
  isHost: boolean;
  anonymousAllowed?: boolean;
  /** Kept for callers; the room sidebar owns the title and close button. */
  onClose?: () => void;
}

type Tab = "open" | "answered" | "hidden";

const AVATAR_COLORS = ["#1d3a6e", "#14503f", "#5a4216", "#3d2c6b", "#5c2236", "#164a5a"];
const colorFor = (name: string) => AVATAR_COLORS[([...name].reduce((h, c) => (h * 31 + c.charCodeAt(0)) | 0, 0) >>> 0) % AVATAR_COLORS.length];
const initials = (name: string) => name.split(/\s+/).filter(Boolean).slice(0, 2).map((part) => part[0]?.toUpperCase()).join("") || "?";

export default function QAPanel({ conferenceId, isHost, anonymousAllowed = false }: QAPanelProps) {
  const { data, isLoading, isError, error, refetch } = useQuestions(conferenceId);
  const questions = (data?.questions ?? []).filter((q) => q.status !== "ARCHIVED");
  const [tab, setTab] = useState<Tab>("open");
  const [draft, setDraft] = useState("");
  const [anonymous, setAnonymous] = useState(false);
  const submit = useSubmitQuestion(conferenceId);

  const byTab: Record<Tab, QuestionView[]> = {
    open: questions.filter((q) => q.status === "OPEN"),
    answered: questions.filter((q) => q.status === "ANSWERED"),
    hidden: questions.filter((q) => q.status === "DISMISSED"),
  };
  // Pinned first, then most upvoted.
  const shown = [...byTab[tab]].sort((a, b) => (a.pinned !== b.pinned ? (a.pinned ? -1 : 1) : b.upvoteCount - a.upvoteCount));
  const tabs: { id: Tab; label: string }[] = [{ id: "open", label: "Open" }, { id: "answered", label: "Answered" }, ...(isHost && byTab.hidden.length ? [{ id: "hidden" as const, label: "Hidden" }] : [])];

  const ask = () => {
    const body = draft.trim();
    if (!body) return;
    submit.mutate({ body, anonymous }, { onSuccess: () => { setDraft(""); setTab("open"); } });
  };

  return (
    <div className="flex min-h-full flex-col">
      <div role="tablist" className="flex gap-1 rounded-full bg-white/5 p-1">
        {tabs.map(({ id, label }) => (
          <button key={id} role="tab" type="button" aria-selected={tab === id} onClick={() => setTab(id)}
            className={cn("flex h-8 flex-1 items-center justify-center gap-1.5 rounded-full text-sm font-medium transition-colors", tab === id ? "bg-white/15 text-white" : "text-white/60 hover:text-white")}>
            {label}{byTab[id].length > 0 && <span className="text-xs tabular-nums text-white/50">{byTab[id].length}</span>}
          </button>
        ))}
      </div>

      <div className="flex-1 space-y-3 py-4">
        {isError && <RequestError error={error} retry={() => void refetch()} />}
        {isLoading && [0, 1].map((i) => <div key={i} className="h-24 animate-pulse rounded-2xl bg-white/5" />)}
        {!isLoading && shown.length === 0 && (
          <div className="flex flex-col items-center px-4 py-12 text-center">
            <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/15 text-primary"><MessageCircleQuestion className="size-6" /></span>
            <p className="mt-4 font-semibold">{tab === "answered" ? "Nothing answered yet" : tab === "hidden" ? "No hidden questions" : "No questions yet"}</p>
            <p className="mt-1 max-w-[16rem] text-sm text-white/60">
              {tab === "answered" ? "Answered questions move here." : isHost ? "Questions from the room show up here. The most upvoted rise to the top." : "Ask the host anything. Upvote questions you want answered too."}
            </p>
          </div>
        )}
        {shown.map((question) => <QuestionCard key={question.id} question={question} conferenceId={conferenceId} isHost={isHost} />)}
      </div>

      <div className="sticky bottom-0 -mx-5 bg-[var(--background)] px-5 pb-1 pt-2">
        {submit.isError && <div className="mb-2"><RequestError error={submit.error} /></div>}
        <div className="flex items-end gap-2 rounded-3xl bg-white/[0.07] p-1.5 pl-4 ring-1 ring-white/10 focus-within:ring-primary">
          <textarea value={draft} rows={1} maxLength={1000} placeholder="Ask a question"
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); ask(); } }}
            className="max-h-32 min-h-9 flex-1 resize-none bg-transparent py-2 text-sm outline-none placeholder:text-white/40 [field-sizing:content]" aria-label="Your question" />
          <button type="button" aria-label="Send question" onClick={ask} disabled={!draft.trim() || submit.isPending}
            className="flex size-9 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground transition-opacity disabled:opacity-40"><ArrowUp className="size-4" strokeWidth={2.5} /></button>
        </div>
        {anonymousAllowed && (
          <button type="button" aria-pressed={anonymous} onClick={() => setAnonymous(!anonymous)}
            className={cn("mt-2 inline-flex h-7 items-center gap-1.5 rounded-full px-2.5 text-xs font-medium transition-colors", anonymous ? "bg-primary/20 text-primary" : "text-white/60 hover:bg-white/10")}>
            <VenetianMask className="size-3.5" />{anonymous ? "Asking anonymously" : "Ask anonymously"}
          </button>
        )}
      </div>
    </div>
  );
}

function QuestionCard({ question, conferenceId, isHost }: { question: QuestionView; conferenceId: string; isHost: boolean }) {
  const [answering, setAnswering] = useState(false);
  const [answerText, setAnswerText] = useState("");
  const vote = useVoteQuestion(conferenceId);
  const unvote = useUnvoteQuestion(conferenceId);
  const answer = useAnswerQuestion(conferenceId);
  const dismiss = useDismissQuestion(conferenceId);
  const restore = useRestoreQuestion(conferenceId);
  const archive = useArchiveQuestion(conferenceId);
  const pin = usePinQuestion(conferenceId);
  const unpin = useUnpinQuestion(conferenceId);
  const failure = [vote, unvote, answer, dismiss, restore, archive, pin, unpin].find((m) => m.isError)?.error;

  const name = question.anonymous ? "Anonymous" : question.displayName || "Someone";
  const canVote = !question.isMine && (question.status === "OPEN" || question.status === "ANSWERED");
  const active = question.status === "OPEN" || question.status === "ANSWERED";

  const sendAnswer = () => {
    const text = answerText.trim();
    if (!text) return;
    answer.mutate({ questionId: question.id, data: { answer: text } }, { onSuccess: () => { setAnswering(false); setAnswerText(""); } });
  };

  const iconButton = "flex size-8 items-center justify-center rounded-full text-white/60 transition-colors hover:bg-white/10 hover:text-white disabled:opacity-40";

  return (
    <article className={cn("rounded-2xl bg-white/[0.06] p-4 ring-1", question.pinned ? "ring-primary/50" : "ring-white/10", question.status === "DISMISSED" && "opacity-70")}>
      <div className="flex items-start gap-3">
        <span className="flex size-8 shrink-0 items-center justify-center rounded-full text-xs font-semibold text-white" style={{ backgroundColor: question.anonymous ? "rgba(255,255,255,0.12)" : colorFor(name) }}>
          {question.anonymous ? <VenetianMask className="size-4" /> : initials(name)}
        </span>
        <div className="min-w-0 flex-1">
          <p className="flex flex-wrap items-center gap-x-2 text-sm">
            <span className="font-semibold">{name}</span>
            {question.isMine && <span className="text-xs text-white/50">You</span>}
            {question.pinned && <span className="inline-flex items-center gap-1 text-xs font-medium text-primary"><Pin className="size-3" />Pinned</span>}
          </p>
          <p className="mt-1 whitespace-pre-wrap break-words text-[15px] leading-relaxed">{question.body}</p>
        </div>
        <button type="button" onClick={() => (question.votedByMe ? unvote : vote).mutate(question.id)} disabled={!canVote || vote.isPending || unvote.isPending}
          aria-pressed={question.votedByMe} aria-label={question.votedByMe ? "Remove upvote" : "Upvote"} title={question.isMine ? "Your question" : question.votedByMe ? "Remove upvote" : "Upvote"}
          className={cn("flex w-11 shrink-0 flex-col items-center rounded-xl py-1.5 text-xs font-semibold tabular-nums ring-1 transition-colors",
            question.votedByMe ? "bg-primary/20 text-primary ring-primary/40" : "text-white/70 ring-white/15", canVote && !question.votedByMe && "hover:bg-white/10")}>
          <ArrowUp className="size-4" strokeWidth={2.5} />{question.upvoteCount}
        </button>
      </div>

      {question.answer && (
        <div className="mt-3 rounded-xl bg-emerald-500/10 px-3 py-2.5">
          <p className="flex items-center gap-1.5 text-xs font-semibold text-emerald-300"><CheckCircle2 className="size-3.5" />Host answer</p>
          <p className="mt-1 whitespace-pre-wrap text-sm">{question.answer}</p>
        </div>
      )}

      {answering && (
        <div className="mt-3 space-y-2">
          <textarea autoFocus value={answerText} onChange={(event) => setAnswerText(event.target.value)} rows={2} placeholder="Write an answer"
            className="w-full resize-none rounded-xl bg-white/5 p-3 text-sm outline-none ring-1 ring-white/15 placeholder:text-white/40 focus:ring-2 focus:ring-primary" />
          <div className="flex justify-end gap-2">
            <button type="button" onClick={() => setAnswering(false)} className="h-9 rounded-full px-4 text-sm font-medium text-white/70 hover:bg-white/10">Cancel</button>
            <button type="button" onClick={sendAnswer} disabled={!answerText.trim() || answer.isPending} className="h-9 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{answer.isPending ? "Posting…" : "Post answer"}</button>
          </div>
        </div>
      )}

      {isHost && !answering && (
        <div className="mt-3 flex items-center gap-1 border-t border-white/10 pt-2">
          {question.status === "OPEN" && (
            <button type="button" onClick={() => setAnswering(true)} className="mr-auto inline-flex h-8 items-center gap-1.5 rounded-full px-3 text-sm font-medium text-primary hover:bg-primary/10"><Check className="size-4" />Answer</button>
          )}
          {question.status !== "OPEN" && <span className="mr-auto" />}
          {active && (
            <button type="button" onClick={() => (question.pinned ? unpin : pin).mutate(question.id)} disabled={pin.isPending || unpin.isPending} className={iconButton} aria-label={question.pinned ? "Unpin" : "Pin to top"} title={question.pinned ? "Unpin" : "Pin to top"}>
              {question.pinned ? <PinOff className="size-4" /> : <Pin className="size-4" />}
            </button>
          )}
          {active && (
            <button type="button" onClick={() => dismiss.mutate(question.id)} disabled={dismiss.isPending} className={iconButton} aria-label="Hide from everyone" title="Hide from everyone"><EyeOff className="size-4" /></button>
          )}
          {question.status === "DISMISSED" && <button type="button" onClick={() => restore.mutate(question.id)} disabled={restore.isPending} className="mr-1 inline-flex h-8 items-center gap-1.5 rounded-full px-3 text-sm font-medium text-primary hover:bg-primary/10"><Eye className="size-4" />Show again</button>}
          <button type="button" onClick={() => archive.mutate(question.id)} disabled={archive.isPending} className={iconButton} aria-label="Remove question" title="Remove question"><Trash2 className="size-4" /></button>
        </div>
      )}

      {failure && <div className="mt-2"><RequestError error={failure} /></div>}
    </article>
  );
}
