"use client";

import { useState } from "react";
import { Archive, BarChart3, Check, ChevronDown, CircleDot, ListChecks, ListOrdered, Play, Plus, Square, Star, TextCursorInput, AlignLeft, X } from "lucide-react";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { NiceSelect } from "@/components/ui/nice-select";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
import { cn } from "@/lib/utils";
import {
  useArchivePoll,
  useClosePoll,
  useCreatePoll,
  useOpenPoll,
  usePollResults,
  usePolls,
  useSubmitPollResponse,
} from "@/hooks/v2";
import type { PollQuestion, PollQuestionType, PollResults, PollStatus, PollView, ResultsVisibility } from "@/types/v2";

interface PollsPanelProps {
  conferenceId: string;
  isHost: boolean;
  /** Kept for callers; the room sidebar owns the title and close button. */
  onClose?: () => void;
}

const STATUS: Record<PollStatus, { label: string; className: string }> = {
  DRAFT: { label: "Draft", className: "bg-white/10 text-white/70" },
  OPEN: { label: "Live", className: "bg-emerald-500/15 text-emerald-300" },
  CLOSED: { label: "Closed", className: "bg-white/10 text-white/60" },
  ARCHIVED: { label: "Archived", className: "bg-white/5 text-white/40" },
};

const TYPES: { value: PollQuestionType; label: string; hint: string; icon: typeof CircleDot }[] = [
  { value: "SINGLE_CHOICE", label: "Single choice", hint: "Pick one answer", icon: CircleDot },
  { value: "MULTIPLE_CHOICE", label: "Multiple choice", hint: "Pick one or more", icon: ListChecks },
  { value: "RATING", label: "Rating", hint: "1 to 5 stars", icon: Star },
  { value: "RANKING", label: "Ranking", hint: "Put options in order", icon: ListOrdered },
  { value: "SHORT_TEXT", label: "Short answer", hint: "A line of text", icon: TextCursorInput },
  { value: "LONG_TEXT", label: "Long answer", hint: "A paragraph", icon: AlignLeft },
];

const VISIBILITY: { value: ResultsVisibility; label: string; hint: string }[] = [
  { value: "AFTER_VOTE", label: "After they vote", hint: "Each person sees results once they answer" },
  { value: "AFTER_CLOSE", label: "When the poll closes", hint: "Everyone sees results at the end" },
  { value: "HOST_ONLY", label: "Only hosts", hint: "Results stay private to hosts" },
];

const ended = (status: PollStatus) => status === "CLOSED" || status === "ARCHIVED";

export default function PollsPanel({ conferenceId, isHost }: PollsPanelProps) {
  const polls = usePolls(conferenceId);
  const [showCreate, setShowCreate] = useState(false);
  // undefined = default (live polls open); otherwise the poll the person picked, or "" for none.
  const [expandedId, setExpandedId] = useState<string>();
  const list = (polls.data?.polls ?? []).filter((poll) => isHost || poll.status !== "DRAFT");

  return (
    <div className="space-y-3">
      {polls.isError && <RequestError error={polls.error} retry={() => void polls.refetch()} />}
      {isHost && list.length > 0 && (
        <button type="button" onClick={() => setShowCreate(true)} className="flex h-11 w-full items-center justify-center gap-2 rounded-full bg-primary text-sm font-semibold text-primary-foreground hover:bg-primary/90">
          <Plus className="size-4" /> New poll
        </button>
      )}
      {polls.isLoading && <div className="space-y-3">{[0, 1].map((i) => <div key={i} className="h-20 animate-pulse rounded-2xl bg-white/5" />)}</div>}
      {!polls.isLoading && list.length === 0 && (
        <div className="flex flex-col items-center px-4 py-12 text-center">
          <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/15 text-primary"><BarChart3 className="size-6" /></span>
          <p className="mt-4 font-semibold">No polls yet</p>
          <p className="mt-1 max-w-[16rem] text-sm text-white/60">{isHost ? "Ask the room a quick question and see answers come in live." : "When a host starts a poll, it shows up here."}</p>
          {isHost && <button type="button" onClick={() => setShowCreate(true)} className="mt-5 inline-flex h-10 items-center gap-2 rounded-full bg-primary px-5 text-sm font-semibold text-primary-foreground hover:bg-primary/90"><Plus className="size-4" /> Create a poll</button>}
        </div>
      )}
      {list.map((poll) => {
        const expanded = expandedId === undefined ? poll.status === "OPEN" : expandedId === poll.id;
        return <PollCard key={poll.id} poll={poll} conferenceId={conferenceId} isHost={isHost} expanded={expanded} onToggle={() => setExpandedId(expanded ? "" : poll.id)} />;
      })}
      <CreatePollDialog open={showCreate} conferenceId={conferenceId} onClose={() => setShowCreate(false)} />
    </div>
  );
}

function PollCard({ poll, conferenceId, isHost, expanded, onToggle }: {
  poll: PollView;
  conferenceId: string;
  isHost: boolean;
  expanded: boolean;
  onToggle: () => void;
}) {
  const resultVisibleByPolicy = poll.resultsVisibility === "AFTER_VOTE" ? poll.hasResponded || ended(poll.status)
    : poll.resultsVisibility === "AFTER_CLOSE" ? ended(poll.status) : false;
  const results = usePollResults(conferenceId, poll.id, expanded && (isHost || resultVisibleByPolicy));
  const open = useOpenPoll(conferenceId);
  const close = useClosePoll(conferenceId);
  const archive = useArchivePoll(conferenceId);
  const transitionError = open.isError ? open.error : close.isError ? close.error : archive.isError ? archive.error : undefined;
  const status = STATUS[poll.status];

  return <article className={cn("rounded-2xl bg-white/[0.06] ring-1 ring-white/10", poll.status === "OPEN" && "ring-emerald-400/30")}>
    <button type="button" className="flex w-full items-start gap-3 p-4 text-left" aria-expanded={expanded} onClick={onToggle}>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className={cn("inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-[11px] font-semibold", status.className)}>
            {poll.status === "OPEN" && <span className="size-1.5 animate-pulse rounded-full bg-emerald-400" />}{status.label}
          </span>
          {poll.quizMode && <span className="rounded-full bg-amber-400/15 px-2 py-0.5 text-[11px] font-semibold text-amber-300">Quiz</span>}
          {poll.anonymous && <span className="text-[11px] text-white/50">Anonymous</span>}
        </div>
        <h4 className="mt-2 font-semibold leading-snug">{poll.title}</h4>
        <p className="mt-1 text-xs text-white/50">{poll.responseCount} {poll.responseCount === 1 ? "response" : "responses"}{poll.hasResponded && " · You answered"}</p>
      </div>
      <ChevronDown className={cn("mt-1 size-4 shrink-0 text-white/50 transition-transform", expanded && "rotate-180")} />
    </button>
    {expanded && <div className="space-y-4 px-4 pb-4">
      {transitionError && <RequestError error={transitionError} />}
      {results.isError && <RequestError error={results.error} />}
      {(poll.questions ?? []).map((question) => <PollQuestionCard
        key={question.id}
        poll={poll}
        question={question}
        result={results.data?.questions.find((item) => item.questionId === question.id)}
        responseCount={results.data?.responseCount ?? poll.responseCount}
        conferenceId={conferenceId}
        isHost={isHost}
        resultsLoading={results.isLoading}
      />)}
      {isHost && poll.status !== "ARCHIVED" && <div className="flex gap-2 border-t border-white/10 pt-3">
        {poll.status === "DRAFT" && <button type="button" onClick={() => open.mutate(poll.id)} disabled={open.isPending} className="inline-flex h-9 flex-1 items-center justify-center gap-1.5 rounded-full bg-primary px-4 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-60"><Play className="size-3.5" />{open.isPending ? "Starting…" : "Start poll"}</button>}
        {poll.status === "OPEN" && <button type="button" onClick={() => close.mutate(poll.id)} disabled={close.isPending} className="inline-flex h-9 flex-1 items-center justify-center gap-1.5 rounded-full bg-white/10 px-4 text-sm font-semibold hover:bg-white/15 disabled:opacity-60"><Square className="size-3.5" />{close.isPending ? "Ending…" : "End poll"}</button>}
        {(poll.status === "DRAFT" || poll.status === "CLOSED") && <button type="button" onClick={() => archive.mutate(poll.id)} disabled={archive.isPending} className="inline-flex h-9 items-center justify-center gap-1.5 rounded-full px-4 text-sm font-medium text-white/70 hover:bg-white/10 disabled:opacity-60"><Archive className="size-3.5" />Archive</button>}
      </div>}
    </div>}
  </article>;
}

function PollQuestionCard({ poll, question, result, responseCount, conferenceId, isHost, resultsLoading }: {
  poll: PollView;
  question: PollQuestion;
  result?: PollResults["questions"][number];
  responseCount: number;
  conferenceId: string;
  isHost: boolean;
  resultsLoading: boolean;
}) {
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [text, setText] = useState("");
  const [rating, setRating] = useState<number>();
  const submit = useSubmitPollResponse(conferenceId, poll.id);
  const hasResponded = poll.hasResponded;
  const locked = hasResponded || poll.status !== "OPEN";
  const showResults = isHost || (
    poll.resultsVisibility === "AFTER_VOTE" ? hasResponded || ended(poll.status)
      : poll.resultsVisibility === "AFTER_CLOSE" && ended(poll.status)
  );
  const toggleOption = (optionId: string) => {
    setSelectedIds((current) => question.type === "SINGLE_CHOICE"
      ? current.includes(optionId) ? [] : [optionId]
      : current.includes(optionId) ? current.filter((id) => id !== optionId) : [...current, optionId]);
  };
  const submitAnswer = () => {
    const answer = question.type === "SHORT_TEXT" || question.type === "LONG_TEXT"
      ? { questionId: question.id, text }
      : question.type === "RATING"
        ? { questionId: question.id, rating }
        : question.type === "RANKING"
          ? { questionId: question.id, ranking: selectedIds }
          : { questionId: question.id, optionIds: selectedIds };
    submit.mutate({ answers: [answer] });
  };
  const isChoice = ["SINGLE_CHOICE", "MULTIPLE_CHOICE", "RANKING"].includes(question.type);
  const isAnswered = question.type === "SHORT_TEXT" || question.type === "LONG_TEXT" ? !!text.trim()
    : question.type === "RATING" ? rating !== undefined : selectedIds.length > 0;
  const hint = question.type === "MULTIPLE_CHOICE" ? "Choose all that apply" : question.type === "RANKING" ? "Tap in order, first = top" : undefined;
  const ratings = Array.from({ length: (question.ratingMax ?? 5) - (question.ratingMin ?? 1) + 1 }, (_, index) => (question.ratingMin ?? 1) + index);

  return <div>
    <p className="font-medium">{question.prompt}</p>
    {hint && !locked && <p className="mt-0.5 text-xs text-white/50">{hint}</p>}

    {isChoice && <div className="mt-3 space-y-2">
      {(question.options ?? []).map((option) => {
        const selected = selectedIds.includes(option.id);
        const count = result?.optionCounts?.[option.id] ?? 0;
        const percent = responseCount ? Math.round((count / responseCount) * 100) : 0;
        const correct = poll.quizMode && result?.correct?.includes(option.id);
        return <button key={option.id} type="button" disabled={locked} onClick={() => toggleOption(option.id)} aria-pressed={selected}
          className={cn("relative flex min-h-11 w-full items-center gap-3 overflow-hidden rounded-xl px-3 py-2.5 text-left text-sm ring-1 transition-colors",
            selected ? "ring-2 ring-primary" : "ring-white/15", !locked && "hover:bg-white/5", correct && "ring-emerald-400/60")}>
          {showResults && <span aria-hidden className={cn("absolute inset-y-0 left-0 transition-[width] duration-500", correct ? "bg-emerald-500/20" : "bg-primary/20")} style={{ width: `${percent}%` }} />}
          <span className={cn("relative flex size-5 shrink-0 items-center justify-center border text-[11px] font-semibold", question.type === "SINGLE_CHOICE" ? "rounded-full" : "rounded-md",
            selected ? "border-primary bg-primary text-primary-foreground" : "border-white/30")}>
            {selected && (question.type === "RANKING" ? selectedIds.indexOf(option.id) + 1 : <Check className="size-3.5" strokeWidth={3} />)}
          </span>
          <span className="relative min-w-0 flex-1">{option.label}{correct && <span className="ml-2 text-xs font-semibold text-emerald-300">Correct</span>}</span>
          {showResults && <span className="relative text-xs font-semibold tabular-nums text-white/70">{percent}%</span>}
        </button>;
      })}
    </div>}

    {(question.type === "SHORT_TEXT" || question.type === "LONG_TEXT") && !locked && <textarea
      value={text} maxLength={question.maxLength} rows={question.type === "LONG_TEXT" ? 4 : 2}
      onChange={(event) => setText(event.target.value)}
      className="mt-3 w-full resize-none rounded-xl bg-white/5 p-3 text-sm outline-none ring-1 ring-white/15 placeholder:text-white/40 focus:ring-2 focus:ring-primary" placeholder="Type your answer" />}

    {question.type === "RATING" && <div className="mt-3 flex gap-1.5">
      {ratings.map((value) => {
        const on = rating !== undefined && value <= rating;
        return <button key={value} type="button" aria-label={`${value} of ${ratings.length}`} disabled={locked} onClick={() => setRating(value)} className="flex size-10 items-center justify-center rounded-xl ring-1 ring-white/15 transition-colors hover:bg-white/5 disabled:hover:bg-transparent">
          <Star className={cn("size-5", on ? "fill-amber-400 text-amber-400" : "text-white/40")} />
        </button>;
      })}
    </div>}
    {showResults && result?.ratingDistribution && <p className="mt-2 text-sm text-white/70">Average <span className="font-semibold text-white">{result.ratingAverage?.toFixed(1) ?? "—"}</span> / {ratings.length}</p>}

    {showResults && !!result?.texts?.length && <div className="mt-3 space-y-2">{result.texts.map((item, index) => <p key={`${item.text}-${index}`} className="rounded-xl bg-white/5 px-3 py-2 text-sm">{item.text}</p>)}</div>}
    {showResults && question.type === "RANKING" && result?.averagePositions && <div className="mt-3 space-y-1">
      {Object.entries(result.averagePositions).sort(([, a], [, b]) => a - b).map(([id, average], index) => <p key={id} className="flex justify-between text-sm"><span>{index + 1}. {question.options?.find((item) => item.id === id)?.label}</span><span className="text-xs text-white/50">avg {average.toFixed(1)}</span></p>)}
    </div>}

    {poll.status === "OPEN" && !hasResponded && <button type="button" onClick={submitAnswer} disabled={!isAnswered || submit.isPending} className="mt-3 h-10 w-full rounded-full bg-primary text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">
      {submit.isPending ? "Sending…" : "Submit"}
    </button>}
    {hasResponded && !showResults && <p className="mt-3 flex items-center gap-2 text-sm text-white/60"><Check className="size-4 text-emerald-400" />Answer sent. Results come later.</p>}
    {submit.isError && <div className="mt-2"><RequestError error={submit.error} /></div>}
    {resultsLoading && showResults && <p role="status" className="mt-2 text-xs text-white/50">Loading results…</p>}
  </div>;
}

function Toggle({ checked, onChange, label, hint, disabled }: { checked: boolean; onChange: (v: boolean) => void; label: string; hint: string; disabled?: boolean }) {
  return <button type="button" role="switch" aria-checked={checked} disabled={disabled} onClick={() => onChange(!checked)} className="flex w-full items-center gap-3 rounded-xl px-1 py-2 text-left disabled:opacity-40">
    <span className="min-w-0 flex-1"><span className="block text-sm font-medium">{label}</span><span className="block text-xs text-muted-foreground">{hint}</span></span>
    <span className={cn("relative h-6 w-10 shrink-0 rounded-full transition-colors", checked ? "bg-primary" : "bg-white/20")}>
      <span className={cn("absolute top-0.5 size-5 rounded-full bg-white shadow transition-[left]", checked ? "left-[18px]" : "left-0.5")} />
    </span>
  </button>;
}

function CreatePollDialog({ open, conferenceId, onClose }: { open: boolean; conferenceId: string; onClose: () => void }) {
  const [prompt, setPrompt] = useState("");
  const [type, setType] = useState<PollQuestionType>("SINGLE_CHOICE");
  const [options, setOptions] = useState(["", ""]);
  const [quizMode, setQuizMode] = useState(false);
  const [correct, setCorrect] = useState<number[]>([]);
  const [anonymous, setAnonymous] = useState(false);
  const [visibility, setVisibility] = useState<ResultsVisibility>("AFTER_VOTE");
  const create = useCreatePoll(conferenceId);
  const choices = ["SINGLE_CHOICE", "MULTIPLE_CHOICE", "RANKING"].includes(type);
  const isText = type === "SHORT_TEXT" || type === "LONG_TEXT";
  const canScoreQuiz = choices || type === "RATING";
  const canCreate = prompt.trim() && (!choices || options.filter((item) => item.trim()).length >= 2) && (!quizMode || (canScoreQuiz && correct.length > 0));

  const reset = () => { setPrompt(""); setType("SINGLE_CHOICE"); setOptions(["", ""]); setQuizMode(false); setCorrect([]); setAnonymous(false); setVisibility("AFTER_VOTE"); create.reset(); };
  const done = () => { reset(); onClose(); };

  const toggleCorrect = (index: number) => setCorrect((current) => type === "SINGLE_CHOICE" ? [index] : current.includes(index) ? current.filter((v) => v !== index) : [...current, index]);

  const submit = (startNow: boolean) => {
    if (!canCreate) return;
    const trimmed = prompt.trim();
    create.mutate({
      // One question per poll here, so the question doubles as the title.
      title: trimmed.slice(0, 300),
      quizMode,
      anonymous,
      resultsVisibility: visibility,
      questions: [{
        type,
        prompt: trimmed,
        required: true,
        ...(choices ? { options: options.map((item) => item.trim()).filter(Boolean) } : {}),
        ...(type === "MULTIPLE_CHOICE" ? { minSelections: 1 } : {}),
        ...(isText ? { maxLength: type === "SHORT_TEXT" ? 280 : 2000 } : {}),
        ...(type === "RATING" ? { ratingMin: 1, ratingMax: 5 } : {}),
        ...(quizMode ? { correct } : {}),
      }],
    }, {
      onSuccess: (poll) => {
        const id = (poll as { id?: string } | undefined)?.id;
        if (startNow && id) startPoll.mutate(id);
        done();
      },
    });
  };
  const startPoll = useOpenPoll(conferenceId);

  return <Dialog open={open} onOpenChange={(v) => !v && done()}>
    <DialogContent className="dark max-h-[90dvh] overflow-y-auto border-white/10 bg-[#2D2E31] text-foreground sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>New poll</DialogTitle>
        <DialogDescription>Ask one question. Answers come in live.</DialogDescription>
      </DialogHeader>
      {create.isError && <RequestError error={create.error} />}

      <div className="space-y-5">
        <textarea autoFocus aria-label="Question" value={prompt} onChange={(event) => setPrompt(event.target.value)} maxLength={500} rows={2} placeholder="Ask a question…"
          className="w-full resize-none rounded-xl bg-white/5 p-3 text-base font-medium outline-none ring-1 ring-white/15 placeholder:text-white/40 focus:ring-2 focus:ring-primary" />

        <NiceSelect label="Answer type" value={type} onChange={(next) => { setType(next); setCorrect([]); if (next === "SHORT_TEXT" || next === "LONG_TEXT") setQuizMode(false); }}
          options={TYPES.map(({ value, label, hint, icon }) => ({ value, label, hint, icon }))} />

        {choices && <div className="space-y-2">
          <p className="text-sm font-medium">Options{quizMode && <span className="ml-2 text-xs font-normal text-muted-foreground">Tap the circle to mark {type === "SINGLE_CHOICE" ? "the correct answer" : "correct answers"}</span>}</p>
          {options.map((option, index) => <div key={index} className="flex items-center gap-2">
            {quizMode
              ? <button type="button" aria-label={`Mark option ${index + 1} correct`} aria-pressed={correct.includes(index)} onClick={() => toggleCorrect(index)} className={cn("flex size-6 shrink-0 items-center justify-center rounded-full border transition-colors", correct.includes(index) ? "border-emerald-400 bg-emerald-500 text-white" : "border-white/30 hover:border-white/60")}>{correct.includes(index) && <Check className="size-3.5" strokeWidth={3} />}</button>
              : <span className="w-6 shrink-0 text-center text-xs font-semibold text-white/40">{index + 1}</span>}
            <input value={option} maxLength={200} aria-label={`Option ${index + 1}`} placeholder={`Option ${index + 1}`} onChange={(event) => setOptions((current) => current.map((item, i) => i === index ? event.target.value : item))}
              className="h-11 min-w-0 flex-1 rounded-xl bg-white/5 px-3 text-sm outline-none ring-1 ring-white/15 placeholder:text-white/40 focus:ring-2 focus:ring-primary" />
            <button type="button" aria-label={`Remove option ${index + 1}`} disabled={options.length <= 2} onClick={() => { setOptions((current) => current.filter((_, i) => i !== index)); setCorrect([]); }} className="flex size-9 shrink-0 items-center justify-center rounded-full text-white/50 hover:bg-white/10 hover:text-white disabled:invisible"><X className="size-4" /></button>
          </div>)}
          {options.length < 20 && <button type="button" onClick={() => setOptions((current) => [...current, ""])} className="ml-8 inline-flex h-9 items-center gap-1.5 rounded-full px-3 text-sm font-medium text-primary hover:bg-primary/10"><Plus className="size-4" />Add option</button>}
        </div>}

        {quizMode && type === "RATING" && <div className="space-y-2">
          <p className="text-sm font-medium">Correct rating</p>
          <div className="flex gap-1.5">{[1, 2, 3, 4, 5].map((value) => <button key={value} type="button" onClick={() => setCorrect([value])} aria-pressed={correct[0] === value} className={cn("size-10 rounded-xl text-sm font-semibold ring-1", correct[0] === value ? "bg-emerald-500 text-white ring-emerald-400" : "ring-white/15 hover:bg-white/5")}>{value}</button>)}</div>
        </div>}

        <NiceSelect label="Who sees results" value={visibility} onChange={setVisibility} options={VISIBILITY} />

        <div className="divide-y divide-white/10 rounded-xl bg-white/5 px-3">
          <Toggle label="Quiz" hint={canScoreQuiz ? "Mark the right answer and score responses" : "Not available for text answers"} checked={quizMode} disabled={!canScoreQuiz} onChange={(v) => { setQuizMode(v); setCorrect([]); }} />
          <Toggle label="Anonymous" hint="Hide who answered what" checked={anonymous} onChange={setAnonymous} />
        </div>
      </div>

      <div className="flex flex-col-reverse gap-2 pt-2 sm:flex-row sm:justify-end">
        <button type="button" onClick={() => submit(false)} disabled={!canCreate || create.isPending} className="h-11 rounded-full px-5 text-sm font-semibold text-white/80 ring-1 ring-white/15 hover:bg-white/5 disabled:opacity-50">Save as draft</button>
        <button type="button" onClick={() => submit(true)} disabled={!canCreate || create.isPending} className="h-11 rounded-full bg-primary px-6 text-sm font-semibold text-primary-foreground hover:bg-primary/90 disabled:opacity-50">{create.isPending ? "Creating…" : "Start poll"}</button>
      </div>
    </DialogContent>
  </Dialog>;
}
