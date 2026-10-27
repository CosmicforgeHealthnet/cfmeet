"use client";

import { useState } from "react";
import { Archive, BarChart3, Check, ChevronDown, ChevronUp, Play, Plus, Square, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RequestError } from "@/components/meeting-spaces/ManagementUI";
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
  onClose: () => void;
}

export default function PollsPanel({ conferenceId, isHost, onClose }: PollsPanelProps) {
  const polls = usePolls(conferenceId);
  const [showCreate, setShowCreate] = useState(false);
  const [expandedId, setExpandedId] = useState<string>();

  return (
    <section className="flex h-full min-h-0 flex-col overflow-hidden rounded-2xl bg-card">
      <header className="flex items-center justify-between border-b border-border px-4 py-3">
        <h3 className="flex items-center gap-2 font-semibold"><BarChart3 className="size-5 text-primary" />Polls & quizzes</h3>
        <Button aria-label="Close polls" variant="ghost" size="icon" onClick={onClose}><X className="size-4" /></Button>
      </header>
      {polls.isError && <div className="p-3"><RequestError error={polls.error} retry={() => void polls.refetch()} /></div>}
      <div className="min-h-0 flex-1 space-y-3 overflow-y-auto p-4">
        {polls.isLoading && <p role="status" className="text-sm text-muted-foreground">Loading polls…</p>}
        {polls.data?.polls.length === 0 && <p className="py-8 text-center text-sm text-muted-foreground">No polls have been created.</p>}
        {polls.data?.polls.map((poll) => (
          <PollCard
            key={poll.id}
            poll={poll}
            conferenceId={conferenceId}
            isHost={isHost}
            expanded={expandedId === poll.id}
            onToggle={() => setExpandedId(expandedId === poll.id ? undefined : poll.id)}
          />
        ))}
      </div>
      {isHost && <footer className="border-t border-border p-3">
        <Button className="w-full" onClick={() => setShowCreate(true)}><Plus className="mr-2 size-4" />Create poll or quiz</Button>
      </footer>}
      {showCreate && <CreatePollModal conferenceId={conferenceId} onClose={() => setShowCreate(false)} />}
    </section>
  );
}

function PollCard({ poll, conferenceId, isHost, expanded, onToggle }: {
  poll: PollView;
  conferenceId: string;
  isHost: boolean;
  expanded: boolean;
  onToggle: () => void;
}) {
  const resultVisibleByPolicy = poll.resultsVisibility === "AFTER_VOTE"
    ? poll.hasResponded || ["CLOSED", "ARCHIVED"].includes(poll.status)
    : poll.resultsVisibility === "AFTER_CLOSE"
      ? ["CLOSED", "ARCHIVED"].includes(poll.status)
      : false;
  const results = usePollResults(conferenceId, poll.id, expanded && (isHost || resultVisibleByPolicy));
  const open = useOpenPoll(conferenceId);
  const close = useClosePoll(conferenceId);
  const archive = useArchivePoll(conferenceId);
  const statusStyles: Record<PollStatus, string> = {
    DRAFT: "bg-yellow-100 text-yellow-800 dark:bg-yellow-900/30 dark:text-yellow-300",
    OPEN: "bg-green-100 text-green-800 dark:bg-green-900/30 dark:text-green-300",
    CLOSED: "bg-muted text-muted-foreground",
    ARCHIVED: "bg-muted text-muted-foreground",
  };
  const transitionError = open.isError ? open.error : close.isError ? close.error : archive.isError ? archive.error : undefined;

  return <article className="rounded-xl border border-border p-3">
    <button className="flex w-full items-center justify-between gap-2 text-left" onClick={onToggle}>
      <div className="min-w-0">
        <div className="flex items-center gap-2"><h4 className="truncate font-medium">{poll.title}</h4><span className={`rounded-full px-2 py-0.5 text-[10px] ${statusStyles[poll.status]}`}>{poll.status}</span></div>
        <p className="mt-1 text-xs text-muted-foreground">{poll.responseCount} responses · {poll.questions.length} question{poll.questions.length === 1 ? "" : "s"}</p>
      </div>
      {expanded ? <ChevronUp className="size-4 shrink-0" /> : <ChevronDown className="size-4 shrink-0" />}
    </button>
    {expanded && <div className="mt-3 space-y-3">
      {transitionError && <RequestError error={transitionError} />}
      {results.isError && <RequestError error={results.error} />}
      {poll.questions.map((question) => <PollQuestionCard
        key={question.id}
        poll={poll}
        question={question}
        result={results.data?.questions.find((item) => item.questionId === question.id)}
        responseCount={results.data?.responseCount ?? poll.responseCount}
        conferenceId={conferenceId}
        isHost={isHost}
        resultsLoading={results.isLoading}
      />)}
      {isHost && <div className="flex flex-wrap gap-2 border-t border-border pt-3">
        {poll.status === "DRAFT" && <Button size="sm" onClick={() => open.mutate(poll.id)} disabled={open.isPending}><Play className="mr-1 size-3.5" />Open</Button>}
        {poll.status === "OPEN" && <Button size="sm" variant="outline" onClick={() => close.mutate(poll.id)} disabled={close.isPending}><Square className="mr-1 size-3.5" />Close</Button>}
        {(poll.status === "DRAFT" || poll.status === "CLOSED") && <Button size="sm" variant="outline" onClick={() => archive.mutate(poll.id)} disabled={archive.isPending}><Archive className="mr-1 size-3.5" />Archive</Button>}
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
  const showResults = isHost || (
    poll.resultsVisibility === "AFTER_VOTE" ? hasResponded || ["CLOSED", "ARCHIVED"].includes(poll.status)
      : poll.resultsVisibility === "AFTER_CLOSE" && ["CLOSED", "ARCHIVED"].includes(poll.status)
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
  return <div className="rounded-lg bg-muted/40 p-3">
    <h5 className="font-medium">{question.prompt}</h5>
    {isChoice && <div className="mt-2 space-y-1.5">
      {question.options.map((option, index) => {
        const selected = selectedIds.includes(option.id);
        const count = result?.optionCounts?.[option.id] ?? 0;
        const percent = responseCount ? (count / responseCount) * 100 : 0;
        return <button key={option.id} type="button" disabled={hasResponded || poll.status !== "OPEN"} onClick={() => toggleOption(option.id)}
          className={`w-full rounded-md border p-2 text-left text-sm ${selected ? "border-primary bg-primary/10" : "border-border"}`}>
          <span className="flex items-center justify-between gap-2"><span>{option.label}{question.type === "RANKING" && selected ? ` · #${selectedIds.indexOf(option.id) + 1}` : ""}</span>{selected && <Check className="size-4 text-primary" />}</span>
          {showResults && <span className="mt-1 block text-xs text-muted-foreground">{count} · {percent.toFixed(0)}%</span>}
          {poll.quizMode && result?.correct?.includes(option.id) && <span className="block text-xs text-green-700">Correct answer</span>}
          {showResults && <span className="mt-1 block h-1.5 overflow-hidden rounded bg-background"><span className="block h-full bg-primary" style={{ width: `${percent}%` }} /></span>}
          {question.type === "RANKING" && selected && <span className="sr-only">Rank {index + 1}</span>}
        </button>;
      })}
    </div>}
    {(question.type === "SHORT_TEXT" || question.type === "LONG_TEXT") && <textarea
      value={text} maxLength={question.maxLength} rows={question.type === "LONG_TEXT" ? 4 : 2}
      disabled={hasResponded || poll.status !== "OPEN"} onChange={(event) => setText(event.target.value)}
      className="mt-2 w-full rounded-md border border-border bg-background p-2 text-sm" placeholder="Your response" />}
    {question.type === "RATING" && <div className="mt-2 flex flex-wrap gap-2">{Array.from({ length: (question.ratingMax ?? 5) - (question.ratingMin ?? 1) + 1 }, (_, index) => (question.ratingMin ?? 1) + index).map((value) =>
      <Button key={value} type="button" size="sm" variant={rating === value ? "default" : "outline"} disabled={hasResponded || poll.status !== "OPEN"} onClick={() => setRating(value)}>{value}</Button>)}</div>}
    {showResults && result?.ratingDistribution && <p className="mt-2 text-xs text-muted-foreground">Average: {result.ratingAverage?.toFixed(1) ?? "—"}</p>}
    {showResults && result?.texts?.map((item, index) => <p key={`${item.text}-${index}`} className="mt-2 rounded bg-background p-2 text-sm">{item.text}</p>)}
    {showResults && question.type === "RANKING" && result?.averagePositions && Object.entries(result.averagePositions).map(([id, average]) => <p key={id} className="text-xs text-muted-foreground">{question.options.find((item) => item.id === id)?.label}: average rank {average.toFixed(1)}</p>)}
    {poll.status === "OPEN" && !hasResponded && <Button className="mt-3 w-full" size="sm" onClick={submitAnswer} disabled={!isAnswered || submit.isPending}>
      {submit.isPending ? "Submitting…" : "Submit response"}
    </Button>}
    {submit.isError && <div className="mt-2"><RequestError error={submit.error} /></div>}
    {resultsLoading && showResults && <p role="status" className="mt-2 text-xs text-muted-foreground">Loading results…</p>}
  </div>;
}

function CreatePollModal({ conferenceId, onClose }: { conferenceId: string; onClose: () => void }) {
  const [title, setTitle] = useState("");
  const [prompt, setPrompt] = useState("");
  const [type, setType] = useState<PollQuestionType>("SINGLE_CHOICE");
  const [options, setOptions] = useState(["", ""]);
  const [quizMode, setQuizMode] = useState(false);
  const [correct, setCorrect] = useState<number[]>([]);
  const [anonymous, setAnonymous] = useState(false);
  const [visibility, setVisibility] = useState<ResultsVisibility>("AFTER_CLOSE");
  const create = useCreatePoll(conferenceId);
  const choices = ["SINGLE_CHOICE", "MULTIPLE_CHOICE", "RANKING"].includes(type);
  const isText = type === "SHORT_TEXT" || type === "LONG_TEXT";
  const canScoreQuiz = choices || type === "RATING";
  const canCreate = title.trim() && prompt.trim() && (!choices || options.filter((item) => item.trim()).length >= 2) && (!quizMode || (canScoreQuiz && correct.length > 0));

  const submit = () => {
    if (!canCreate) return;
    create.mutate({
      title: title.trim(),
      quizMode,
      anonymous,
      resultsVisibility: visibility,
      questions: [{
        type,
        prompt: prompt.trim(),
        required: true,
        ...(choices ? { options: options.map((item) => item.trim()).filter(Boolean) } : {}),
        ...(type === "MULTIPLE_CHOICE" ? { minSelections: 1 } : {}),
        ...(isText ? { maxLength: type === "SHORT_TEXT" ? 280 : 2000 } : {}),
        ...(type === "RATING" ? { ratingMin: 1, ratingMax: 5 } : {}),
        ...(quizMode ? { correct } : {}),
      }],
    }, { onSuccess: onClose });
  };

  return <div className="fixed inset-0 z-[60] flex items-center justify-center p-4">
    <button aria-label="Close create poll dialog" className="absolute inset-0 bg-black/50" onClick={onClose} />
    <div role="dialog" aria-modal="true" aria-labelledby="create-poll-title" className="relative max-h-[90vh] w-full max-w-lg space-y-4 overflow-y-auto rounded-2xl bg-card p-5 shadow-xl">
      <div className="flex items-center justify-between"><h3 id="create-poll-title" className="text-lg font-semibold">Create poll or quiz</h3><Button aria-label="Close" variant="ghost" size="icon" onClick={onClose}><X className="size-4" /></Button></div>
      {create.isError && <RequestError error={create.error} />}
      <div><Label htmlFor="poll-title">Title</Label><Input id="poll-title" value={title} onChange={(event) => setTitle(event.target.value)} maxLength={300} /></div>
      <div><Label htmlFor="poll-question">Question</Label><textarea id="poll-question" value={prompt} onChange={(event) => setPrompt(event.target.value)} maxLength={500} rows={2} className="mt-1 w-full rounded-md border border-border bg-background p-2 text-sm" /></div>
      <label className="grid gap-1 text-sm">Answer type<select value={type} onChange={(event) => { const nextType = event.target.value as PollQuestionType; setType(nextType); setCorrect([]); if (["SHORT_TEXT", "LONG_TEXT"].includes(nextType)) setQuizMode(false); }} className="h-10 rounded-md border border-input bg-background px-2">
        {(["SINGLE_CHOICE", "MULTIPLE_CHOICE", "SHORT_TEXT", "LONG_TEXT", "RATING", "RANKING"] as const).map((value) => <option key={value} value={value}>{value.replaceAll("_", " ").toLowerCase()}</option>)}
      </select></label>
      {choices && <div className="space-y-2"><Label>Options</Label>{options.map((option, index) => <div key={index} className="flex gap-2"><Input value={option} maxLength={200} aria-label={`Option ${index + 1}`} onChange={(event) => setOptions((current) => current.map((item, itemIndex) => itemIndex === index ? event.target.value : item))} /><Button type="button" variant="outline" disabled={options.length <= 2} onClick={() => setOptions((current) => current.filter((_, itemIndex) => itemIndex !== index))}>Remove</Button></div>)}{options.length < 20 && <Button type="button" size="sm" variant="outline" onClick={() => setOptions((current) => [...current, ""])}>Add option</Button>}</div>}
      <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={quizMode} disabled={!canScoreQuiz} onChange={(event) => { setQuizMode(event.target.checked); setCorrect([]); }} />Scored quiz</label>
      {quizMode && choices && <fieldset className="space-y-1"><legend className="text-sm font-medium">Correct answer{type === "MULTIPLE_CHOICE" || type === "RANKING" ? "s" : ""}</legend>{options.map((option, index) => <label key={index} className="flex items-center gap-2 text-sm"><input type={type === "SINGLE_CHOICE" ? "radio" : "checkbox"} name="correct-option" checked={correct.includes(index)} onChange={(event) => setCorrect((current) => type === "SINGLE_CHOICE" ? [index] : event.target.checked ? [...current, index] : current.filter((value) => value !== index))} />{option || `Option ${index + 1}`}</label>)}</fieldset>}
      {quizMode && type === "RATING" && <label className="grid gap-1 text-sm">Correct rating<select value={correct[0] ?? ""} onChange={(event) => setCorrect(event.target.value ? [Number(event.target.value)] : [])} className="h-10 rounded-md border border-input bg-background px-2"><option value="">Select correct rating</option>{[1, 2, 3, 4, 5].map((value) => <option key={value} value={value}>{value}</option>)}</select></label>}
      <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={anonymous} onChange={(event) => setAnonymous(event.target.checked)} />Anonymous responses</label>
      <label className="grid gap-1 text-sm">Results visibility<select value={visibility} onChange={(event) => setVisibility(event.target.value as ResultsVisibility)} className="h-10 rounded-md border border-input bg-background px-2">
        {(["HOST_ONLY", "AFTER_VOTE", "AFTER_CLOSE"] as const).map((value) => <option key={value} value={value}>{value.replaceAll("_", " ").toLowerCase()}</option>)}
      </select></label>
      <div className="flex gap-2"><Button variant="outline" className="flex-1" onClick={onClose}>Cancel</Button><Button className="flex-1" onClick={submit} disabled={!canCreate || create.isPending}>{create.isPending ? "Creating…" : "Create draft"}</Button></div>
    </div>
  </div>;
}
