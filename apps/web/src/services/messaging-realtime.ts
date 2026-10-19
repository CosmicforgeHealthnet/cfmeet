/**
 * Part V §§58–68: browser Messaging device transport (not the MLS cryptographic SDK).
 * A trusted backend must provision the short-lived Messaging Client Access Token.
 * Never put a Platform Access service credential or CF-Acting-Principal authority here.
 * Tokens and tickets are kept in memory for one handshake only, never in a URL.
 */
export const MESSAGING_REALTIME_PROTOCOL = "cfmeet.messaging.v1";
const PREFIX = "cfmeet.ticket.";
const TICKET_ENDPOINT = "/v1/realtime/tickets";
const SOCKET_ENDPOINT = "/v1/realtime";

export type MessagingAccessToken = {
  token: string;
  tokenType: "CF-Client-Token";
  expiresAt: string;
};
export type MessagingRealtimeEvent = {
  eventId: string;
  conversationId: string;
  eventSequence: number;
  [key: string]: unknown;
};
export type MessagingTransportOptions = {
  baseUrl: string;
  /** MUST originate from an authenticated trusted application-backend exchange. */
  getAccessToken: () => Promise<MessagingAccessToken>;
  /** Throw on processing failure. The transport will not acknowledge that event. */
  onEvent: (event: MessagingRealtimeEvent) => void | Promise<void>;
  onSyncRequired: (conversationId: string, frame: Record<string, unknown>) => void;
  onConnectionState?: (state: "connected" | "disconnected" | "auth-required") => void;
  fetchImpl?: typeof fetch;
  socketFactory?: (url: string, protocols: string[]) => WebSocket;
};
export class MessagingTransportError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "MessagingTransportError";
  }
}
function secureOrigin(value: string): URL {
  const u = new URL(value);
  const local = ["localhost", "127.0.0.1", "[::1]"].includes(u.hostname);
  if ((u.protocol !== "https:" && !(local && u.protocol === "http:")) ||
      u.username || u.password || u.search || u.hash || u.pathname !== "/") {
    throw new MessagingTransportError("Expected a secure Messaging service origin");
  }
  return u;
}
function requireCursor(cursor: number): void {
  if (!Number.isSafeInteger(cursor) || cursor < 0) {
    throw new MessagingTransportError("Invalid event cursor");
  }
}
function requireConversation(id: string): void {
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id)) {
    throw new MessagingTransportError("Invalid conversation ID");
  }
}
function parseTicket(value: unknown): string {
  if (!value || typeof value !== "object") throw new MessagingTransportError("Invalid ticket");
  const t = value as Record<string, unknown>;
  if (t.path !== SOCKET_ENDPOINT || t.subprotocol !== MESSAGING_REALTIME_PROTOCOL ||
      t.ticketSubprotocolPrefix !== PREFIX || typeof t.ticket !== "string" ||
      !/^[0-9a-f]{64}$/.test(t.ticket) || typeof t.expiresAt !== "string" ||
      !(Date.parse(t.expiresAt) > Date.now())) {
    throw new MessagingTransportError("Invalid or expired ticket");
  }
  return t.ticket;
}

/** Reconnect explicitly using connect(); a new token and single-use ticket are required. */
export class MessagingRealtimeTransport {
  private readonly origin: URL;
  private readonly options: MessagingTransportOptions;
  private readonly subscriptions = new Map<string, number>();
  private socket: WebSocket | null = null;
  private connecting: Promise<void> | null = null;
  private processing: Promise<void> = Promise.resolve();
  private generation = 0;
  private disposed = false;

  constructor(options: MessagingTransportOptions) {
    this.origin = secureOrigin(options.baseUrl);
    this.options = options;
  }
  cursor(conversationId: string): number | undefined {
    return this.subscriptions.get(conversationId);
  }
  subscribe(conversationId: string, afterEventSequence = 0): void {
    requireConversation(conversationId);
    requireCursor(afterEventSequence);
    const value = Math.max(afterEventSequence, this.subscriptions.get(conversationId) ?? 0);
    this.subscriptions.set(conversationId, value);
    if (this.socket?.readyState === 1) {
      this.send({ type: "subscribe", conversationId, afterEventSequence: value });
    }
  }
  /** Must only be called after authoritative REST synchronization completes. */
  synced(conversationId: string, authoritativeCursor: number): void {
    requireConversation(conversationId);
    requireCursor(authoritativeCursor);
    const value = Math.max(authoritativeCursor, this.subscriptions.get(conversationId) ?? 0);
    this.subscriptions.set(conversationId, value);
    if (this.socket?.readyState === 1) {
      this.send({ type: "subscribe", conversationId, afterEventSequence: value });
    }
  }
  unsubscribe(conversationId: string): void {
    this.subscriptions.delete(conversationId);
    if (this.socket?.readyState === 1) this.send({ type: "unsubscribe", conversationId });
  }
  typing(conversationId: string, started: boolean): void {
    if (!this.subscriptions.has(conversationId)) throw new MessagingTransportError("Not subscribed");
    this.send({ type: "typing", conversationId, state: started ? "started" : "stopped" });
  }
  connect(): Promise<void> {
    if (this.disposed) return Promise.reject(new MessagingTransportError("Transport disposed"));
    if (this.socket?.readyState === 1) return Promise.resolve();
    if (this.connecting) return this.connecting;
    const run = this.open();
    this.connecting = run;
    void run.finally(() => { if (this.connecting === run) this.connecting = null; }).catch(() => {});
    return run;
  }
  private async open(): Promise<void> {
    const epoch = ++this.generation;
    const grant = await this.options.getAccessToken();
    if (epoch !== this.generation || this.disposed) return;
    if (!grant || grant.tokenType !== "CF-Client-Token" || !grant.token ||
        !(Date.parse(grant.expiresAt) > Date.now())) {
      throw new MessagingTransportError("A valid Messaging Client Access Token is required");
    }
    const request = this.options.fetchImpl ?? fetch;
    const response = await request(new URL(TICKET_ENDPOINT, this.origin), {
      method: "POST", credentials: "omit", cache: "no-store", redirect: "error",
      headers: { Authorization: "CF-Client-Token " + grant.token, Accept: "application/json" },
    });
    if (epoch !== this.generation || this.disposed) return;
    if (!response.ok) {
      this.options.onConnectionState?.([401, 403].includes(response.status) ? "auth-required" : "disconnected");
      throw new MessagingTransportError("Messaging ticket request failed: HTTP " + response.status);
    }
    const ticket = parseTicket(await response.json());
    const wsUrl = new URL(SOCKET_ENDPOINT, this.origin);
    wsUrl.protocol = wsUrl.protocol === "https:" ? "wss:" : "ws:";
    const factory = this.options.socketFactory ?? ((url: string, protocols: string[]) => new WebSocket(url, protocols));
    const ws = factory(wsUrl.toString(), [MESSAGING_REALTIME_PROTOCOL, PREFIX + ticket]);
    if (epoch !== this.generation || this.disposed) { ws.close(); return; }
    this.socket = ws;
    await new Promise<void>((resolve, reject) => {
      let settled = false;
      const finish = (error?: Error) => {
        if (settled) return;
        settled = true;
        if (error) reject(error); else resolve();
      };
      ws.addEventListener("open", () => {
        if (this.disposed || epoch !== this.generation) {
          ws.close();
          finish(new MessagingTransportError("Connection superseded"));
          return;
        }
        this.options.onConnectionState?.("connected");
        for (const [conversationId, afterEventSequence] of this.subscriptions) {
          ws.send(JSON.stringify({ type: "subscribe", conversationId, afterEventSequence }));
        }
        finish();
      }, { once: true });
      ws.addEventListener("error", () => finish(new MessagingTransportError("WebSocket handshake failed")));
      ws.addEventListener("close", (event: CloseEvent) => {
        if (this.socket === ws) this.socket = null;
        if (!this.disposed && epoch === this.generation) {
          this.options.onConnectionState?.(event.code === 4401 ? "auth-required" : "disconnected");
        }
        finish(new MessagingTransportError("WebSocket closed before opening"));
      });
      ws.addEventListener("message", (message: MessageEvent) => {
        if (this.socket !== ws || epoch !== this.generation || typeof message.data !== "string") return;
        const raw = message.data;
        // Serial processing prevents out-of-order acknowledgements for async callbacks.
        this.processing = this.processing.then(() => this.handleMessage(raw)).catch(() => {});
      });
    });
  }
  private async handleMessage(raw: string): Promise<void> {
    let frame: Record<string, unknown>;
    try {
      const candidate: unknown = JSON.parse(raw);
      if (!candidate || typeof candidate !== "object" || Array.isArray(candidate)) return;
      frame = candidate as Record<string, unknown>;
    } catch { return; }
    if (frame.type === "sync.required" && typeof frame.conversationId === "string" &&
        this.subscriptions.has(frame.conversationId)) {
      this.options.onSyncRequired(frame.conversationId, frame);
      return;
    }
    if (frame.type !== "event" || !frame.event || typeof frame.event !== "object") return;
    const event = frame.event as Record<string, unknown>;
    if (typeof event.eventId !== "string" || typeof event.conversationId !== "string" ||
        !Number.isSafeInteger(event.eventSequence) || (event.eventSequence as number) < 1) return;
    const current = this.subscriptions.get(event.conversationId);
    if (current === undefined || (event.eventSequence as number) <= current) return;
    if (event.eventSequence !== current + 1) {
      this.options.onSyncRequired(event.conversationId, {
        type: "sync.required", reason: "CLIENT_CURSOR_GAP", afterEventSequence: current,
      });
      return;
    }
    try {
      await this.options.onEvent(event as MessagingRealtimeEvent);
      if (this.subscriptions.get(event.conversationId) === current) {
        this.subscriptions.set(event.conversationId, event.eventSequence as number);
        this.send({ type: "ack", conversationId: event.conversationId, eventSequence: event.eventSequence });
      }
    } catch {
      // Never ack an event that the application failed to process.
    }
  }
  private send(frame: Record<string, unknown>): void {
    if (this.socket?.readyState !== 1) throw new MessagingTransportError("Not connected");
    this.socket.send(JSON.stringify(frame));
  }
  close(): void {
    this.disposed = true;
    this.generation++;
    this.subscriptions.clear();
    this.socket?.close();
    this.socket = null;
    this.options.onConnectionState?.("disconnected");
  }
}
