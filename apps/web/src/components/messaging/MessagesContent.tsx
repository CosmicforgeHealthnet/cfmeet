"use client";

/**
 * CF Meet Messages (Part V Milestone B): conversations of the signed-in account, with the
 * truth about each one's protection. PRIVATE_E2EE conversations are encrypted on this
 * device (the server cannot read them); MANAGED conversations are readable by CF Meet's
 * servers and say so. Message text is rendered only as text (React escapes it).
 */
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { ArrowLeft, Check, Lock, LockOpen, Paperclip, Search, Send, ShieldCheck, ShieldAlert, Wifi, WifiOff, Smartphone, History, UserPlus } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { MessagingEngine, type ConversationView, type MessageView, type SecurityProfile } from "@/lib/messaging/engine";
import { useMessagingDirectory, useMessagingDevices, useRegisterMessagingDevice, useRemoveCurrentMessagingDevice } from "@/hooks/v2/useMessaging";
import { messagingErrorMessage } from "@/lib/messaging/errors";
import MessagingDevicesPanel from "./MessagingDevicesPanel";

function useEngine(): MessagingEngine {
  // One engine per mounted page (lazy state initializer, never re-created on render).
  const [engine] = useState(() => new MessagingEngine());
  useEffect(() => {
    void engine.start();
    return () => engine.stop();
  }, [engine]);
  return engine;
}

function ProfileBadge({ profile }: { profile: SecurityProfile }) {
  return profile === "PRIVATE_E2EE" ? (
    <span data-testid="profile-badge" data-profile={profile} className="inline-flex items-center gap-1 rounded-full bg-emerald-500/10 px-2 py-0.5 text-xs font-medium text-emerald-700 dark:text-emerald-400">
      <Lock className="h-3 w-3" /> End-to-end encrypted
    </span>
  ) : (
    <span data-testid="profile-badge" data-profile={profile} className="inline-flex items-center gap-1 rounded-full bg-amber-500/10 px-2 py-0.5 text-xs font-medium text-amber-700 dark:text-amber-400">
      <LockOpen className="h-3 w-3" /> Managed — readable by CF Meet servers
    </span>
  );
}

function StatusBar({ engine }: { engine: MessagingEngine }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const connected = s.connection === "connected";
  return (
    <div className="flex flex-wrap items-center gap-3 text-xs text-muted-foreground" data-testid="messaging-status">
      <span className="inline-flex items-center gap-1" data-testid="device-state" data-phase={s.phase}>
        <Smartphone className="h-3.5 w-3.5" />
        {s.phase === "ready" ? "This browser is a secure messaging device" : s.phase === "needs-device" ? "No secure device in this browser" : s.phase === "registering" ? "Setting up this device…" : s.phase}
      </span>
      <span className="inline-flex items-center gap-1" data-testid="connection-state" data-connection={s.connection}>
        {connected ? <Wifi className="h-3.5 w-3.5" /> : <WifiOff className="h-3.5 w-3.5" />}
        {s.sessionsPaused ? "Live updates paused" : connected ? "Live" : s.connection === "connecting" ? "Connecting…" : s.connection === "offline" ? "Live updates unavailable (refreshing periodically)" : "Reconnecting…"}
      </span>
      <span className="inline-flex items-center gap-1" data-testid="kt-state">
        {s.ktPinned ? <ShieldCheck className="h-3.5 w-3.5" /> : <ShieldAlert className="h-3.5 w-3.5" />}
        {s.ktPinned ? "Contact keys verified against the transparency log" : "Contact keys not verified against a pinned transparency log"}
      </span>
    </div>
  );
}

function DevicePanel({ engine }: { engine: MessagingEngine }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [recovering, setRecovering] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const devices = useMessagingDevices(recovering ? s.identity?.principalId ?? "" : "");
  const registration = useRegisterMessagingDevice(engine);
  const removal = useRemoveCurrentMessagingDevice(engine);
  const busy = registration.isPending || removal.isPending;
  const sources = devices.data?.devices.filter((d) => d.status === "ACTIVE" && d.deviceId !== s.deviceId) ?? [];
  const run = async (action: () => Promise<unknown>) => {
    setError(null);
    try { await action(); } catch (cause) { setError(messagingErrorMessage(cause)); }
  };
  const [transfer, setTransfer] = useState<{ code: string; state: string } | null>(null);
  if (s.phase === "loading") return null;
  if (s.phase === "needs-device" || s.phase === "revoked" || s.phase === "registering") {
    return (
      <div className="rounded-2xl border border-border bg-card p-4 space-y-2" data-testid="device-setup">
        {s.detail ? <p className="text-sm text-muted-foreground">{s.detail}</p> : null}
        <p className="text-sm">Set up this browser to read and send end-to-end encrypted messages. Its keys are created here and never leave this browser.</p>
        <Button
          data-testid="register-device"
          disabled={busy || s.phase === "registering"}
          onClick={() => void run(() => registration.mutateAsync())}
        >
          {s.phase === "registering" ? <Spinner /> : null} Set up secure messaging
        </Button>
        {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      </div>
    );
  }
  if (s.phase !== "ready") {
    return (
      <div className="rounded-2xl border border-border bg-card p-4 text-sm" data-testid="messaging-unavailable">
        {s.detail ?? "Messaging is unavailable."}
      </div>
    );
  }
  return (
    <div className="rounded-2xl border border-border bg-card p-4 space-y-3">
      {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
      {s.incomingTransfers.map((t) => (
        <div key={t.transferId} className="space-y-2 rounded-xl border border-border p-3" data-testid="incoming-transfer">
          <p className="text-sm">Another of your devices asks for your message history. Continue only if it shows this code:</p>
          <p className="font-mono text-lg tracking-widest" data-testid="incoming-code">{t.comparisonCode}</p>
          <div className="flex gap-2">
            <Button data-testid="approve-transfer" onClick={() => void run(() => engine.approveTransfer(t.transferId))}>Codes match — send history</Button>
            <Button variant="outline" onClick={() => void run(() => engine.declineTransfer(t.transferId))}>Decline</Button>
          </div>
        </div>
      ))}
      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="outline"
          size="sm"
          data-testid="recover-history"
          onClick={() => setRecovering(!recovering)}
          aria-expanded={recovering}
        >
          <History className="h-4 w-4" /> Recover history from another device
        </Button>
        <Button variant="outline" size="sm" disabled={busy} data-testid="remove-device" onClick={() => void run(() => removal.mutateAsync())}>
          Remove this device
        </Button>
      </div>
      {recovering && devices.isPending ? <p className="text-sm text-muted-foreground">Loading your other devices…</p> : null}
      {recovering && devices.error ? <p role="alert" className="text-sm text-destructive">{messagingErrorMessage(devices.error)} <button className="underline" onClick={() => void devices.refetch()}>Retry</button></p> : null}
      {recovering && devices.isSuccess && sources.length === 0 ? <p className="text-sm text-muted-foreground">No other active devices to recover history from.</p> : null}
      {recovering && sources.length > 0 && !transfer ? (
        <ul className="space-y-1">
          {sources.map((d) => (
            <li key={d.deviceId}>
              <Button
                variant="ghost"
                size="sm"
                data-testid="transfer-source"
                onClick={() => void run(async () => {
                  const r = await engine.requestHistory(d.deviceId);
                  setTransfer({ code: r.comparisonCode, state: "waiting" });
                  try {
                    const n = await engine.receiveHistory(r.transferId);
                    setTransfer({ code: r.comparisonCode, state: `received ${n} messages` });
                  } catch (e) {
                    setTransfer({ code: r.comparisonCode, state: `failed: ${e instanceof Error ? e.message : "error"}` });
                  }
                })}
              >
                {d.displayLabel ?? "Device"} · {d.deviceId.slice(0, 8)}
              </Button>
            </li>
          ))}
        </ul>
      ) : null}
      {transfer ? (
        <div className="rounded-xl border border-border p-3 text-sm" data-testid="outgoing-transfer" data-state={transfer.state}>
          <p>Confirm on your other device that it shows:</p>
          <p className="font-mono text-lg tracking-widest" data-testid="outgoing-code">{transfer.code}</p>
          <p className="text-muted-foreground">{transfer.state}</p>
        </div>
      ) : null}
    </div>
  );
}

function NewConversation({ engine, onCreated }: { engine: MessagingEngine; onCreated: (id: string) => void }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const directory = useMessagingDirectory(search, open && !!s.identity);
  const people = directory.data?.people ?? [];
  const [chosen, setChosen] = useState<string[]>([]);
  const [profile, setProfile] = useState<SecurityProfile>("PRIVATE_E2EE");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  if (!open) {
    return (
      <Button size="sm" data-testid="new-conversation" disabled={!s.identity} onClick={() => setOpen(true)}>
        <UserPlus className="h-4 w-4" /> New conversation
      </Button>
    );
  }
  return (
    <div className="space-y-2 rounded-xl border border-border p-3" data-testid="new-conversation-form">
      <p className="text-sm font-medium">Colleagues in your organizations</p>
      <Input aria-label="Search colleagues" placeholder="Search colleagues" maxLength={64} value={search} onChange={(event) => setSearch(event.target.value)} />
      {directory.isPending ? <p className="text-xs text-muted-foreground">Loading colleagues…</p> : directory.error ? <p role="alert" className="text-xs text-destructive">{messagingErrorMessage(directory.error)} <button className="underline" onClick={() => void directory.refetch()}>Retry</button></p> : people.length === 0 ? <p className="text-xs text-muted-foreground">No colleagues found.</p> : null}
      <ul className="max-h-40 space-y-1 overflow-auto">
        {people.map((p) => (
          <li key={p.userId}>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                data-testid="colleague"
                data-name={p.displayName}
                checked={chosen.includes(p.userId)}
                onChange={(e) => setChosen(e.target.checked ? [...chosen, p.userId] : chosen.filter((x) => x !== p.userId))}
              />
              {p.displayName}
            </label>
          </li>
        ))}
      </ul>
      <fieldset className="space-y-1 text-sm">
        <label className="flex items-center gap-2">
          <input type="radio" name="profile" data-testid="profile-e2ee" checked={profile === "PRIVATE_E2EE"} onChange={() => setProfile("PRIVATE_E2EE")} disabled={s.phase !== "ready"} />
          End-to-end encrypted (only the participants&apos; devices can read it)
        </label>
        <label className="flex items-center gap-2">
          <input type="radio" name="profile" data-testid="profile-managed" checked={profile === "MANAGED"} onChange={() => setProfile("MANAGED")} />
          Managed (CF Meet servers can read it, e.g. for compliance)
        </label>
      </fieldset>
      {error ? <p className="text-xs text-destructive" data-testid="create-error">{error}</p> : null}
      <div className="flex gap-2">
        <Button
          size="sm"
          data-testid="create-conversation"
          disabled={busy || chosen.length === 0 || (profile === "PRIVATE_E2EE" && s.phase !== "ready")}
          onClick={async () => {
            setBusy(true);
            setError(null);
            try {
              onCreated(await engine.createConversation(chosen, profile));
              setOpen(false);
              setChosen([]);
            } catch (e) {
              setError(messagingErrorMessage(e));
            } finally {
              setBusy(false);
            }
          }}
        >
          Create
        </Button>
        <Button size="sm" variant="ghost" onClick={() => setOpen(false)}>
          Cancel
        </Button>
      </div>
    </div>
  );
}

function MessageBubble({ m, name, onDownload }: { m: MessageView; name: string; onDownload: () => void }) {
  return (
    <li
      data-testid="message"
      data-state={m.state}
      data-mine={m.mine ? "true" : "false"}
      className={cn("max-w-[75%] rounded-2xl px-3 py-2 text-sm", m.mine ? "ml-auto bg-primary text-primary-foreground" : "bg-muted", m.state === "failed" && "border border-destructive")}
    >
      {!m.mine ? <p className="text-xs font-medium opacity-70">{name}</p> : null}
      {m.text !== null ? <p className="whitespace-pre-wrap break-words" data-testid="message-text">{m.text}</p> : null}
      {m.file ? (
        <button type="button" className="mt-1 inline-flex items-center gap-1 underline" data-testid="attachment" onClick={onDownload}>
          <Paperclip className="h-3.5 w-3.5" /> {m.file.name}
        </button>
      ) : null}
      {m.note ? <p className="text-xs italic opacity-80" data-testid="message-note">{m.note}</p> : null}
      <p className="mt-0.5 text-[10px] opacity-60">
        {m.state === "sending" ? "Sending…" : m.state === "failed" ? "Failed" : m.createdAt ? new Date(m.createdAt).toLocaleTimeString() : ""}
        {m.transferred ? " · recovered" : ""}
      </p>
    </li>
  );
}

function ConversationPane({ engine, conversation, onBack }: { engine: MessagingEngine; conversation: ConversationView; onBack?: () => void }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [text, setText] = useState("");
  const [members, setMembers] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const file = useRef<HTMLInputElement>(null);
  const messages = useMemo(() => s.messages[conversation.id] ?? [], [s.messages, conversation.id]);
  const e2ee = conversation.securityProfile === "PRIVATE_E2EE";
  const canSend = !!s.identity && (!e2ee || (s.phase === "ready" && conversation.groupReady));
  useEffect(() => {
    let active = true;
    void engine.sync(conversation.id).then(() => engine.markRead(conversation.id)).catch((cause) => {
      if (active) setError(messagingErrorMessage(cause));
    });
    return () => { active = false; };
  }, [engine, conversation.id]);
  const download = useCallback(
    async (m: MessageView) => {
      const { blob, name } = await engine.download(conversation.id, m);
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = name;
      a.rel = "noopener";
      a.click();
      setTimeout(() => URL.revokeObjectURL(url), 10_000);
    },
    [engine, conversation.id],
  );
  return (
    <section className="flex h-full flex-col" data-testid="conversation" data-conversation-id={conversation.id} data-profile={conversation.securityProfile}>
      <header className="flex min-h-[4.5rem] items-center justify-between gap-3 border-b border-border px-4 py-3">
        <div className="flex min-w-0 items-center gap-3">
          {onBack ? (
            <Button type="button" variant="ghost" size="icon" className="shrink-0 md:hidden" onClick={onBack} aria-label="Back to conversations" data-testid="back-to-conversations">
              <ArrowLeft className="h-4 w-4" />
            </Button>
          ) : null}
          <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-full bg-primary/10 text-sm font-semibold text-primary" aria-hidden="true">
            {conversation.title.trim().slice(0, 1).toUpperCase() || "?"}
          </div>
          <div className="min-w-0">
            <h2 className="truncate text-sm font-semibold" data-testid="conversation-title">{conversation.title}</h2>
            <div className="mt-1"><ProfileBadge profile={conversation.securityProfile} /></div>
          </div>
        </div>
        <Button variant="outline" size="sm" onClick={() => setMembers(!members)} aria-expanded={members} data-testid="conversation-members-toggle">
          Members
        </Button>
      </header>
      {members ? (
        <ul className="border-b border-border p-3 text-sm" data-testid="members">
          {conversation.members.map((p) => (
            <li key={p} className="flex items-center justify-between">
              <span>{p === s.identity?.principalId ? "You" : s.names[p] ?? "CF Meet user"}</span>
              {p !== s.identity?.principalId ? (
                <Button variant="ghost" size="sm" data-testid="remove-member" onClick={() => {
                  setError(null);
                  void engine.removeMember(conversation.id, p).catch((cause) => setError(messagingErrorMessage(cause)));
                }}>
                  Remove
                </Button>
              ) : null}
            </li>
          ))}
        </ul>
      ) : null}
      {e2ee && !conversation.groupReady ? (
        <p className="m-3 rounded-xl bg-muted p-3 text-sm" data-testid="e2ee-unavailable">
          {s.phase === "ready" ? "Waiting for this device to be added to the encrypted conversation." : "Set up secure messaging in this browser to read this end-to-end encrypted conversation."}
        </p>
      ) : null}
      <ol className="flex-1 space-y-2 overflow-y-auto p-3" data-testid="messages">
        {messages.map((m) => (
          <MessageBubble key={m.key} m={m} name={(m.senderPrincipalId && s.names[m.senderPrincipalId]) || "CF Meet user"} onDownload={() => {
            setError(null);
            void download(m).catch((cause) => setError(messagingErrorMessage(cause)));
          }} />
        ))}
      </ol>
      {error ? <p role="alert" className="px-3 pb-2 text-sm text-destructive">{error}</p> : null}
      <form
        className="flex items-center gap-2 border-t border-border p-3"
        onSubmit={(e) => {
          e.preventDefault();
          const t = text;
          setText("");
          setError(null);
          void engine.send(conversation.id, t).catch((cause) => {
            setText((draft) => draft || t);
            setError(messagingErrorMessage(cause));
          });
        }}
      >
        <input
          ref={file}
          type="file"
          className="hidden"
          data-testid="file-input"
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) {
              setError(null);
              void engine.sendFile(conversation.id, f).catch((cause) => setError(messagingErrorMessage(cause)));
            }
            e.target.value = "";
          }}
        />
        <Button type="button" variant="ghost" size="icon" disabled={!canSend} onClick={() => file.current?.click()} aria-label="Attach a file">
          <Paperclip className="h-4 w-4" />
        </Button>
        <Input data-testid="composer" value={text} maxLength={16384} disabled={!canSend} placeholder={canSend ? (e2ee ? "Encrypted message" : "Message (readable by CF Meet servers)") : "Unavailable on this device"} onChange={(e) => setText(e.target.value)} />
        <Button type="submit" size="icon" data-testid="send" disabled={!canSend || !text.trim()} aria-label="Send">
          <Send className="h-4 w-4" />
        </Button>
      </form>
    </section>
  );
}

export default function MessagesContent() {
  const engine = useEngine();
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [selected, setSelected] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const [profileFilter, setProfileFilter] = useState<"ALL" | SecurityProfile>("ALL");
  const [securityOpen, setSecurityOpen] = useState(false);
  const current = s.conversations.find((c) => c.id === selected) ?? null;
  const conversations = s.conversations.filter((conversation) => {
    const matchesSearch = conversation.title.toLocaleLowerCase().includes(search.trim().toLocaleLowerCase());
    return matchesSearch && (profileFilter === "ALL" || conversation.securityProfile === profileFilter);
  });
  return (
    <div className="mx-auto flex h-[calc(100dvh-2rem)] min-h-[32rem] max-w-[88rem] flex-col gap-3 p-3 sm:p-5" data-testid="messages-page">
      <header className="flex flex-wrap items-center justify-between gap-x-6 gap-y-2">
        <div>
          <h1 className="text-2xl font-bold tracking-tight">Messages</h1>
          <p className="mt-0.5 text-sm text-muted-foreground">Your conversations, all in one place.</p>
        </div>
        <StatusBar engine={engine} />
      </header>
      {s.phase !== "loading" && s.phase !== "ready" ? (
        <section className="rounded-xl border border-border bg-card px-4 py-3" aria-label="Secure messaging setup">
          <p className="mb-2 text-sm font-medium">Finish setting up secure messaging</p>
          <DevicePanel engine={engine} />
        </section>
      ) : null}
      <div className="flex min-h-0 flex-1 overflow-hidden rounded-2xl border border-border bg-card shadow-sm">
        <aside className={cn("flex w-full min-h-0 flex-col border-r border-border md:w-[21rem] md:shrink-0", current && "hidden md:flex")} aria-label="Conversations">
          <div className="space-y-3 border-b border-border p-4">
            <div className="flex items-center justify-between gap-2">
              <div>
                <h2 className="text-base font-semibold">Chats</h2>
                <p className="text-xs text-muted-foreground">{s.conversations.length} {s.conversations.length === 1 ? "conversation" : "conversations"}</p>
              </div>
              <NewConversation engine={engine} onCreated={setSelected} />
            </div>
            <label className="flex h-10 items-center gap-2 rounded-lg border border-input bg-background px-3 text-muted-foreground focus-within:ring-2 focus-within:ring-ring">
              <Search className="h-4 w-4 shrink-0" />
              <input
                type="search"
                aria-label="Search conversations"
                data-testid="conversation-search"
                placeholder="Search conversations"
                value={search}
                onChange={(event) => setSearch(event.target.value)}
                className="min-w-0 flex-1 bg-transparent text-sm text-foreground outline-none placeholder:text-muted-foreground"
              />
            </label>
            <div className="flex gap-1" role="group" aria-label="Filter conversations">
              {([
                ["ALL", "All"],
                ["PRIVATE_E2EE", "Private"],
                ["MANAGED", "Managed"],
              ] as const).map(([value, label]) => (
                <button
                  type="button"
                  key={value}
                  aria-pressed={profileFilter === value}
                  onClick={() => setProfileFilter(value)}
                  className={cn(
                    "rounded-full px-3 py-1.5 text-xs font-medium transition-colors",
                    profileFilter === value ? "bg-primary text-primary-foreground" : "text-muted-foreground hover:bg-muted",
                  )}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>
          <ul className="min-h-0 flex-1 overflow-y-auto p-2" data-testid="conversation-list">
            {s.phase === "loading" ? (
              <li className="flex items-center gap-2 px-3 py-5 text-sm text-muted-foreground"><Spinner /> Loading conversations…</li>
            ) : conversations.map((c) => {
              const latest = s.messages[c.id]?.at(-1);
              const preview = latest?.text ?? latest?.file?.name ?? latest?.note ?? "No messages yet";
              return (
                <li key={c.id}>
                  <button
                    type="button"
                    data-testid="conversation-item"
                    data-profile={c.securityProfile}
                    aria-current={c.id === selected ? "true" : undefined}
                    onClick={() => setSelected(c.id)}
                    className={cn(
                      "flex w-full items-center gap-3 rounded-xl px-3 py-3 text-left transition-colors hover:bg-muted/70",
                      c.id === selected && "bg-muted",
                    )}
                  >
                    <span className="flex h-11 w-11 shrink-0 items-center justify-center rounded-full bg-primary/10 text-sm font-semibold text-primary" aria-hidden="true">
                      {c.title.trim().slice(0, 1).toUpperCase() || "?"}
                    </span>
                    <span className="min-w-0 flex-1">
                      <span className="flex items-center justify-between gap-2">
                        <span className="flex min-w-0 items-center gap-1.5 font-medium">
                          {c.securityProfile === "PRIVATE_E2EE" ? <Lock className="h-3.5 w-3.5 shrink-0 text-emerald-600" /> : <LockOpen className="h-3.5 w-3.5 shrink-0 text-amber-600" />}
                          <span className="truncate">{c.title}</span>
                        </span>
                        {latest?.createdAt ? <time className="shrink-0 text-[10px] text-muted-foreground">{new Date(latest.createdAt).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" })}</time> : null}
                      </span>
                      <span className="mt-1 flex items-center justify-between gap-2">
                        <span className="truncate text-xs text-muted-foreground">{latest?.mine ? "You: " : ""}{preview}</span>
                        {latest?.mine && latest.state !== "failed" ? <Check className="h-3 w-3 shrink-0 text-muted-foreground" aria-label="Sent" /> : null}
                      </span>
                    </span>
                  </button>
                </li>
              );
            })}
            {s.phase !== "loading" && conversations.length === 0 ? (
              <li className="px-4 py-10 text-center">
                <p className="text-sm font-medium">{search ? "No matching conversations" : "No conversations yet"}</p>
                <p className="mt-1 text-xs text-muted-foreground">{search ? "Try another name or search term." : "Start a new chat to get the conversation going."}</p>
              </li>
            ) : null}
          </ul>
          {s.phase === "ready" ? (
            <div className="border-t border-border p-3">
              <button type="button" className="w-full text-left text-xs text-muted-foreground hover:text-foreground" onClick={() => setSecurityOpen(!securityOpen)} aria-expanded={securityOpen}>
                {securityOpen ? "Hide device & security settings" : "Device & security settings"}
              </button>
              {securityOpen ? <div className="mt-3 space-y-3"><DevicePanel engine={engine} /><MessagingDevicesPanel engine={engine} /></div> : null}
            </div>
          ) : null}
        </aside>
        <main className={cn("min-w-0 min-h-0 flex-1", !current && "hidden md:flex md:items-center md:justify-center")} data-testid="conversation-pane">
          {current ? (
            <ConversationPane key={current.id} engine={engine} conversation={current} onBack={() => setSelected(null)} />
          ) : (
            <div className="max-w-sm px-6 text-center">
              <div className="mx-auto flex h-16 w-16 items-center justify-center rounded-full bg-primary/10 text-primary"><Send className="h-7 w-7" /></div>
              <h2 className="mt-5 text-lg font-semibold">Your messages</h2>
              <p className="mt-2 text-sm text-muted-foreground">Choose a conversation to pick up where you left off, or start a new chat with a colleague.</p>
            </div>
          )}
        </main>
      </div>
      {s.phase === "error" ? (
        <p className="text-sm text-destructive" data-testid="messaging-error">Messaging error: {s.detail}</p>
      ) : null}
    </div>
  );
}
