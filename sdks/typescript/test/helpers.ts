// Test helpers: a local HTTP server with a scripted handler, and a tiny RFC 6455
// WebSocket server over the HTTP upgrade (no dependency).

import { createHash } from 'node:crypto';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import type { AddressInfo } from 'node:net';
import type { Duplex } from 'node:stream';

export interface Recorded {
  readonly method: string;
  readonly url: string;
  readonly headers: IncomingMessage['headers'];
  readonly body: string;
}

export type Handler = (req: Recorded, res: ServerResponse) => void | Promise<void>;

export interface UpgradeDecision {
  /** Reject the handshake with this HTTP status. */
  readonly reject?: number;
  /** Subprotocol to select. */
  readonly protocol?: string;
}

export interface TestServer {
  readonly url: string;
  readonly requests: Recorded[];
  readonly upgrades: Recorded[];
  handler: Handler;
  onUpgrade: (req: Recorded) => UpgradeDecision;
  onConnection: (peer: WsPeer) => void;
  close(): Promise<void>;
}

export function json(res: ServerResponse, status: number, body: unknown, headers: Record<string, string> = {}): void {
  const text = JSON.stringify(body);
  res.writeHead(status, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(text), ...headers });
  res.end(text);
}

export function errorEnvelope(
  res: ServerResponse,
  status: number,
  code: string,
  headers: Record<string, string> = {},
  requestId = 'req_test_1',
): void {
  json(res, status, { error: { code, message: `test ${code}`, requestId } }, { 'x-request-id': requestId, ...headers });
}

export async function startServer(handler: Handler = (_q, res) => json(res, 200, {})): Promise<TestServer> {
  const requests: Recorded[] = [];
  const upgrades: Recorded[] = [];
  const sockets = new Set<Duplex>();
  const state: TestServer = {
    url: '',
    requests,
    upgrades,
    handler,
    onUpgrade: () => ({}),
    onConnection: () => {},
    close: async () => {
      for (const s of sockets) s.destroy();
      server.closeAllConnections();
      await new Promise<void>((r) => server.close(() => r()));
    },
  };
  const server: Server = createServer((req, res) => {
    const chunks: Buffer[] = [];
    req.on('data', (c: Buffer) => chunks.push(c));
    req.on('end', () => {
      const rec = { method: req.method ?? '', url: req.url ?? '', headers: req.headers, body: Buffer.concat(chunks).toString('utf8') };
      requests.push(rec);
      Promise.resolve(state.handler(rec, res)).catch((e) => {
        res.destroy(e as Error);
      });
    });
  });
  server.on('upgrade', (req: IncomingMessage, socket: Duplex) => {
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
    socket.on('error', () => {});
    const rec = { method: req.method ?? '', url: req.url ?? '', headers: req.headers, body: '' };
    upgrades.push(rec);
    const decision = state.onUpgrade(rec);
    if (decision.reject) {
      socket.end(`HTTP/1.1 ${decision.reject} Refused\r\nContent-Length: 0\r\nConnection: close\r\n\r\n`);
      return;
    }
    const key = String(req.headers['sec-websocket-key'] ?? '');
    const accept = createHash('sha1').update(`${key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`).digest('base64');
    socket.write(
      'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n' +
        `Sec-WebSocket-Accept: ${accept}\r\n` +
        (decision.protocol ? `Sec-WebSocket-Protocol: ${decision.protocol}\r\n` : '') +
        '\r\n',
    );
    state.onConnection(new WsPeer(socket, rec));
  });
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', () => r()));
  const { port } = server.address() as AddressInfo;
  (state as { url: string }).url = `http://127.0.0.1:${port}`;
  return state;
}

function frame(opcode: number, payload: Buffer): Buffer {
  const len = payload.length;
  let header: Buffer;
  if (len < 126) {
    header = Buffer.from([0x80 | opcode, len]);
  } else if (len < 65536) {
    header = Buffer.alloc(4);
    header[0] = 0x80 | opcode;
    header[1] = 126;
    header.writeUInt16BE(len, 2);
  } else {
    header = Buffer.alloc(10);
    header[0] = 0x80 | opcode;
    header[1] = 127;
    header.writeBigUInt64BE(BigInt(len), 2);
  }
  return Buffer.concat([header, payload]);
}

/** One server-side WebSocket peer (text frames, ping / pong, close). */
export class WsPeer {
  readonly received: string[] = [];
  #buf = Buffer.alloc(0);
  #listeners: ((text: string) => void)[] = [];
  #closed = false;

  constructor(
    readonly socket: Duplex,
    readonly request: Recorded,
  ) {
    socket.on('data', (d: Buffer) => this.#onData(d));
  }

  onMessage(cb: (text: string) => void): void {
    this.#listeners.push(cb);
  }

  send(value: unknown): void {
    if (this.#closed) return;
    this.socket.write(frame(0x1, Buffer.from(typeof value === 'string' ? value : JSON.stringify(value), 'utf8')));
  }

  /** Sends a close frame with `code` and ends the TCP connection shortly after. */
  close(code: number, reason = ''): void {
    if (this.#closed) return;
    this.#closed = true;
    const payload = Buffer.alloc(2 + Buffer.byteLength(reason));
    payload.writeUInt16BE(code, 0);
    payload.write(reason, 2);
    this.socket.write(frame(0x8, payload));
    setTimeout(() => this.socket.end(), 20);
  }

  /** Drops the TCP connection without a close frame (abnormal closure, 1006). */
  drop(): void {
    this.#closed = true;
    this.socket.destroy();
  }

  #onData(d: Buffer): void {
    this.#buf = Buffer.concat([this.#buf, d]);
    for (;;) {
      if (this.#buf.length < 2) return;
      const opcode = this.#buf[0]! & 0x0f;
      const masked = (this.#buf[1]! & 0x80) !== 0;
      let len = this.#buf[1]! & 0x7f;
      let off = 2;
      if (len === 126) {
        if (this.#buf.length < 4) return;
        len = this.#buf.readUInt16BE(2);
        off = 4;
      } else if (len === 127) {
        if (this.#buf.length < 10) return;
        len = Number(this.#buf.readBigUInt64BE(2));
        off = 10;
      }
      const maskLen = masked ? 4 : 0;
      if (this.#buf.length < off + maskLen + len) return;
      const mask = masked ? this.#buf.subarray(off, off + 4) : null;
      const payload = Buffer.from(this.#buf.subarray(off + maskLen, off + maskLen + len));
      if (mask) for (let i = 0; i < payload.length; i++) payload[i]! ^= mask[i % 4]!;
      this.#buf = this.#buf.subarray(off + maskLen + len);
      if (opcode === 0x1) {
        const text = payload.toString('utf8');
        this.received.push(text);
        for (const l of this.#listeners) l(text);
      } else if (opcode === 0x8) {
        if (!this.#closed) {
          this.#closed = true;
          this.socket.write(frame(0x8, payload.subarray(0, 2)));
          setTimeout(() => this.socket.end(), 20);
        }
      } else if (opcode === 0x9) {
        this.socket.write(frame(0xa, payload));
      }
    }
  }
}

/** Resolves when `predicate` holds (polls), rejects after `timeoutMs`. */
export async function waitFor(predicate: () => boolean, timeoutMs = 5000, what = 'condition'): Promise<void> {
  const start = Date.now();
  while (!predicate()) {
    if (Date.now() - start > timeoutMs) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 10));
  }
}

/** Runs `f` with window/document defined, as in a browser. */
export async function inBrowserLikeRuntime<T>(f: () => T | Promise<T>): Promise<T> {
  const g = globalThis as { window?: unknown; document?: unknown };
  g.window = {};
  g.document = {};
  try {
    return await f();
  } finally {
    delete g.window;
    delete g.document;
  }
}
