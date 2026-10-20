"use client";

/**
 * CF Meet Messages (Part V Milestone B): conversations of the signed-in account, with the
 * truth about each one's protection. PRIVATE_E2EE conversations are encrypted on this
 * device (the server cannot read them); MANAGED conversations are readable by CF Meet's
 * servers and say so. Message text is rendered only as text (React escapes it).
 */
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { Lock, LockOpen, Paperclip, Send, ShieldCheck, ShieldAlert, Wifi, WifiOff, Smartphone, History, UserPlus } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";
import { MessagingEngine, type ConversationView, type MessageView, type SecurityProfile } from "@/lib/messaging/engine";
import type { Colleague } from "@/lib/messaging/api";

function useEngine(): MessagingEngine {
  const ref = useRef<MessagingEngine | null>(null);
  ref.current ??= new MessagingEngine();
  const engine = ref.current;
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
        {connected ? "Live" : s.connection === "connecting" ? "Connecting…" : s.connection === "offline" ? "Live updates unavailable (refreshing periodically)" : "Reconnecting…"}
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
  const [busy, setBusy] = useState(false);
  const [devices, setDevices] = useState<{ deviceId: string; displayLabel: string | null }[]>([]);
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
          onClick={async () => {
            setBusy(true);
            await engine.registerDevice();
            setBusy(false);
          }}
        >
          {s.phase === "registering" ? <Spinner /> : null} Set up secure messaging
        </Button>
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
      {s.incomingTransfers.map((t) => (
        <div key={t.transferId} className="space-y-2 rounded-xl border border-border p-3" data-testid="incoming-transfer">
          <p className="text-sm">Another of your devices asks for your message history. Continue only if it shows this code:</p>
          <p className="font-mono text-lg tracking-widest" data-testid="incoming-code">{t.comparisonCode}</p>
          <div className="flex gap-2">
            <Button data-testid="approve-transfer" onClick={() => void engine.approveTransfer(t.transferId)}>Codes match — send history</Button>
            <Button variant="outline" onClick={() => void engine.declineTransfer(t.transferId)}>Decline</Button>
          </div>
        </div>
      ))}
      <div className="flex flex-wrap items-center gap-2">
        <Button
          variant="outline"
          size="sm"
          data-testid="recover-history"
          onClick={async () => setDevices(await engine.otherDevices())}
        >
          <History className="h-4 w-4" /> Recover history from another device
        </Button>
        <Button variant="outline" size="sm" data-testid="remove-device" onClick={() => void engine.removeThisDevice()}>
          Remove this device
        </Button>
      </div>
      {devices.length > 0 && !transfer ? (
        <ul className="space-y-1">
          {devices.map((d) => (
            <li key={d.deviceId}>
              <Button
                variant="ghost"
                size="sm"
                data-testid="transfer-source"
                onClick={async () => {
                  const r = await engine.requestHistory(d.deviceId);
                  setTransfer({ code: r.comparisonCode, state: "waiting" });
                  try {
                    const n = await engine.receiveHistory(r.transferId);
                    setTransfer({ code: r.comparisonCode, state: `received ${n} messages` });
                  } catch (e) {
                    setTransfer({ code: r.comparisonCode, state: `failed: ${e instanceof Error ? e.message : "error"}` });
                  }
                }}
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
  const [people, setPeople] = useState<Colleague[]>([]);
  const [chosen, setChosen] = useState<string[]>([]);
  const [profile, setProfile] = useState<SecurityProfile>("PRIVATE_E2EE");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    if (open) void engine.directory().then(setPeople).catch(() => setPeople([]));
  }, [open, engine]);
  if (!open) {
    return (
      <Button size="sm" data-testid="new-conversation" onClick={() => setOpen(true)}>
        <UserPlus className="h-4 w-4" /> New conversation
      </Button>
    );
  }
  return (
    <div className="space-y-2 rounded-xl border border-border p-3" data-testid="new-conversation-form">
      <p className="text-sm font-medium">Colleagues in your organizations</p>
      {people.length === 0 ? <p className="text-xs text-muted-foreground">No colleagues found.</p> : null}
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
          disabled={busy || chosen.length === 0}
          onClick={async () => {
            setBusy(true);
            setError(null);
            try {
              onCreated(await engine.createConversation(chosen, profile));
              setOpen(false);
              setChosen([]);
            } catch (e) {
              setError(e instanceof Error ? e.message : "Could not create the conversation");
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

function ConversationPane({ engine, conversation }: { engine: MessagingEngine; conversation: ConversationView }) {
  const s = useSyncExternalStore(engine.subscribe, engine.getSnapshot, engine.getSnapshot);
  const [text, setText] = useState("");
  const [members, setMembers] = useState(false);
  const file = useRef<HTMLInputElement>(null);
  const messages = useMemo(() => s.messages[conversation.id] ?? [], [s.messages, conversation.id]);
  const e2ee = conversation.securityProfile === "PRIVATE_E2EE";
  const canSend = !e2ee || (s.phase === "ready" && conversation.groupReady);
  useEffect(() => {
    void engine.sync(conversation.id).then(() => engine.markRead(conversation.id));
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
      <header className="flex flex-wrap items-center justify-between gap-2 border-b border-border p-3">
        <div>
          <h2 className="text-base font-semibold" data-testid="conversation-title">{conversation.title}</h2>
          <ProfileBadge profile={conversation.securityProfile} />
        </div>
        <Button variant="ghost" size="sm" onClick={() => setMembers(!members)}>
          Members
        </Button>
      </header>
      {members ? (
        <ul className="border-b border-border p-3 text-sm" data-testid="members">
          {conversation.members.map((p) => (
            <li key={p} className="flex items-center justify-between">
              <span>{p === s.identity?.principalId ? "You" : s.names[p] ?? "CF Meet user"}</span>
              {p !== s.identity?.principalId ? (
                <Button variant="ghost" size="sm" data-testid="remove-member" onClick={() => void engine.removeMember(conversation.id, p)}>
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
          <MessageBubble key={m.key} m={m} name={(m.senderPrincipalId && s.names[m.senderPrincipalId]) || "CF Meet user"} onDownload={() => void download(m)} />
        ))}
      </ol>
      <form
        className="flex items-center gap-2 border-t border-border p-3"
        onSubmit={(e) => {
          e.preventDefault();
          const t = text;
          setText("");
          void engine.send(conversation.id, t);
        }}
      >
        <input
          ref={file}
          type="file"
          className="hidden"
          data-testid="file-input"
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) void engine.sendFile(conversation.id, f);
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
  const current = s.conversations.find((c) => c.id === selected) ?? null;
  return (
    <div className="mx-auto flex h-[calc(100vh-6rem)] max-w-6xl flex-col gap-3 p-4" data-testid="messages-page">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 className="text-2xl font-bold tracking-tight">Messages</h1>
        <StatusBar engine={engine} />
      </div>
      <DevicePanel engine={engine} />
      {s.phase === "loading" ? (
        <div className="flex items-center gap-2 text-sm text-muted-foreground">
          <Spinner /> Loading…
        </div>
      ) : null}
      {s.phase === "error" ? (
        <p className="text-sm text-destructive" data-testid="messaging-error">Messaging error: {s.detail}</p>
      ) : null}
      <div className="grid min-h-0 flex-1 grid-cols-1 gap-3 md:grid-cols-[18rem_1fr]">
        <aside className="flex min-h-0 flex-col gap-2 rounded-2xl border border-border bg-card p-3">
          <NewConversation engine={engine} onCreated={setSelected} />
          <ul className="min-h-0 flex-1 space-y-1 overflow-y-auto" data-testid="conversation-list">
            {s.conversations.map((c) => (
              <li key={c.id}>
                <button
                  type="button"
                  data-testid="conversation-item"
                  data-profile={c.securityProfile}
                  onClick={() => setSelected(c.id)}
                  className={cn("w-full rounded-xl px-3 py-2 text-left text-sm hover:bg-muted", c.id === selected && "bg-muted")}
                >
                  <span className="flex items-center gap-1 font-medium">
                    {c.securityProfile === "PRIVATE_E2EE" ? <Lock className="h-3 w-3" /> : <LockOpen className="h-3 w-3" />} {c.title}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </aside>
        <main className="min-h-0 rounded-2xl border border-border bg-card">
          {current ? <ConversationPane key={current.id} engine={engine} conversation={current} /> : <p className="p-6 text-sm text-muted-foreground">Select or start a conversation.</p>}
        </main>
      </div>
    </div>
  );
}
