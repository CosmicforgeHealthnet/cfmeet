// Drives an interop peer process (test/support/interop-peer.ts or the Dart SDK's
// test/support/interop_peer.dart) over JSON lines on stdin / stdout.

import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { createInterface } from 'node:readline';

export interface PeerAnswer {
  readonly ok?: boolean;
  readonly error?: string;
  readonly [k: string]: unknown;
}

export class PeerProcess {
  readonly #child: ChildProcessWithoutNullStreams;
  readonly #lines: string[] = [];
  readonly #waiters: ((line: string | null) => void)[] = [];
  #stderr = '';
  #ended = false;
  deviceId = '';
  sdk = '';

  private constructor(child: ChildProcessWithoutNullStreams) {
    this.#child = child;
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (d: string) => {
      this.#stderr = (this.#stderr + d).slice(-8000);
    });
    const rl = createInterface({ input: child.stdout, crlfDelay: Infinity });
    rl.on('line', (line) => {
      if (!line.trim().startsWith('{')) return; // tool chatter (e.g. dart run's build output)
      const w = this.#waiters.shift();
      if (w) w(line);
      else this.#lines.push(line);
    });
    rl.on('close', () => {
      this.#ended = true;
      for (const w of this.#waiters.splice(0)) w(null);
    });
  }

  /** Starts the peer and waits for its `ready` line (the device is registered and has a KeyPackage). */
  static async start(command: string, args: readonly string[], options: { cwd?: string; env?: NodeJS.ProcessEnv; readyTimeoutMs?: number } = {}): Promise<PeerProcess> {
    const child = spawn(command, [...args], { cwd: options.cwd, env: options.env ?? process.env, stdio: ['pipe', 'pipe', 'pipe'] });
    const peer = new PeerProcess(child);
    child.on('error', (e) => {
      peer.#stderr += `\ncannot run ${command}: ${e.message}`;
    });
    child.stdin.on('error', () => undefined); // the peer died: the pending answer reports it
    const ready = (await peer.#next(options.readyTimeoutMs ?? 180_000)) as PeerAnswer & { event?: string; deviceId?: string; sdk?: string };
    if (ready.event !== 'ready' || typeof ready.deviceId !== 'string') {
      peer.kill();
      throw new Error(`peer ${command} did not become ready: ${JSON.stringify(ready)}\n${peer.#stderr}`);
    }
    peer.deviceId = ready.deviceId;
    peer.sdk = String(ready.sdk ?? '');
    return peer;
  }

  #next(timeoutMs: number): Promise<PeerAnswer> {
    const queued = this.#lines.shift();
    if (queued !== undefined) return Promise.resolve(JSON.parse(queued) as PeerAnswer);
    if (this.#ended) return Promise.reject(new Error(`peer exited\n${this.#stderr}`));
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`peer answer timed out after ${timeoutMs} ms\n${this.#stderr}`)), timeoutMs);
      this.#waiters.push((line) => {
        clearTimeout(timer);
        if (line === null) reject(new Error(`peer exited\n${this.#stderr}`));
        else resolve(JSON.parse(line) as PeerAnswer);
      });
    });
  }

  /** Sends one command; resolves with the answer (throws on {"ok": false}). */
  async request(command: Record<string, unknown>, timeoutMs = 60_000): Promise<PeerAnswer> {
    this.#child.stdin.write(`${JSON.stringify(command)}\n`);
    const answer = await this.#next(timeoutMs);
    if (answer.ok !== true) throw new Error(`peer ${String(command.cmd)} failed: ${answer.error ?? JSON.stringify(answer)}\n${this.#stderr}`);
    return answer;
  }

  /** Asks the peer to exit; resolves with its exit code. */
  async exit(timeoutMs = 15_000): Promise<number | null> {
    if (this.#child.exitCode !== null) return this.#child.exitCode;
    const done = new Promise<number | null>((r) => this.#child.once('exit', (code) => r(code)));
    this.#child.stdin.write(`${JSON.stringify({ cmd: 'exit' })}\n`);
    const timer = setTimeout(() => this.kill(), timeoutMs);
    const code = await done;
    clearTimeout(timer);
    return code;
  }

  kill(): void {
    if (this.#child.exitCode === null) this.#child.kill('SIGKILL');
  }
}
