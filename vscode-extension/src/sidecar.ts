import { ChildProcess, spawn } from "child_process";
import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";

export interface Project {
  key: string;
  name: string;
  detail: string;
}

export interface SessionSummary {
  id: string;
  title: string;
  source: "cli" | "vscode";
  cwd?: string | null;
  repository?: string | null;
  branch?: string | null;
  project: Project;
  createdAt?: string | null;
  updatedAt?: string | null;
  messageCount: number;
  done: boolean;
  inUse: boolean;
  windowOpen: boolean;
  protection?: string | null;
  resumeCommand?: string | null;
}

export interface SearchHit extends SessionSummary {
  snippet?: { role: string; text: string } | null;
}

export interface SessionDetail {
  session: SessionSummary;
  messages: unknown[];
}

interface Pending {
  resolve(value: unknown): void;
  reject(reason: unknown): void;
}

/** Shuts the backend down after this long without a request, unless something keeps it alive. */
const IDLE_TIMEOUT_MS = 10 * 60 * 1000;

/**
 * Talks to the Rust backend (vscode-extension/sidecar) over line-delimited JSON on stdio.
 * The process is started on first use and keeps the session index in memory.
 */
export class Sidecar implements vscode.Disposable {
  private process?: ChildProcess;
  private buffer = "";
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private indexed?: Promise<void>;
  private idleTimer?: NodeJS.Timeout;
  private keepAlive = 0;
  private readonly changed = new vscode.EventEmitter<void>();
  /** Sessions changed on disk; call {@link refresh} to update the index. */
  readonly onDidChangeSessions = this.changed.event;

  constructor(
    private readonly extensionPath: string,
    private readonly log: vscode.OutputChannel
  ) {}

  request<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    let child: ChildProcess;
    try {
      child = this.start();
    } catch (error) {
      return Promise.reject(error instanceof Error ? error.message : String(error));
    }
    this.resetIdleTimer();
    const id = this.nextId++;
    return new Promise<T>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
      child.stdin!.write(`${JSON.stringify({ id, method, params })}\n`);
    });
  }

  /** Updates the index from disk. Only changed session files are parsed again. */
  refresh(): Promise<void> {
    const run = this.request<number>("refresh_sessions").then(() => undefined);
    this.indexed = run.catch((error) => {
      this.indexed = undefined;
      throw error;
    });
    return this.indexed;
  }

  /** Resolves once the index has been built at least once. */
  ensureIndexed(): Promise<void> {
    return this.indexed ?? this.refresh();
  }

  get isIndexed(): boolean {
    return this.indexed !== undefined;
  }

  /** Keeps the backend running (and watching for changes) while the returned object lives. */
  retain(): vscode.Disposable {
    this.keepAlive++;
    clearTimeout(this.idleTimer);
    return new vscode.Disposable(() => {
      this.keepAlive--;
      this.resetIdleTimer();
    });
  }

  dispose(): void {
    clearTimeout(this.idleTimer);
    this.stop();
    this.changed.dispose();
  }

  private start(): ChildProcess {
    if (this.process) {
      return this.process;
    }
    const binary = this.binaryPath();
    this.log.appendLine(`Starting ${binary}`);
    const child = spawn(binary, [], { stdio: ["pipe", "pipe", "pipe"], windowsHide: true });
    this.process = child;
    this.buffer = "";
    child.stdout!.setEncoding("utf8");
    child.stdout!.on("data", (chunk: string) => this.onData(chunk));
    child.stderr!.setEncoding("utf8");
    child.stderr!.on("data", (chunk: string) => this.log.append(chunk));
    child.on("error", (error) => {
      this.log.appendLine(`Backend error: ${error.message}`);
      this.onExit(child, `The search backend could not be started: ${error.message}`);
    });
    child.on("exit", (code, signal) => {
      this.log.appendLine(`Backend exited (code ${code}, signal ${signal})`);
      this.onExit(child, "The search backend stopped unexpectedly. See the Copilot Session Search log.");
    });
    return child;
  }

  private stop(): void {
    const child = this.process;
    if (child) {
      this.log.appendLine("Stopping the backend");
      this.onExit(child, "The search backend was stopped.");
      child.stdin?.end();
      setTimeout(() => child.exitCode === null && child.kill(), 2000);
    }
  }

  private onExit(child: ChildProcess, message: string): void {
    if (this.process !== child) {
      return;
    }
    this.process = undefined;
    this.indexed = undefined;
    for (const request of this.pending.values()) {
      request.reject(message);
    }
    this.pending.clear();
  }

  private resetIdleTimer(): void {
    clearTimeout(this.idleTimer);
    if (this.keepAlive === 0 && this.process) {
      this.idleTimer = setTimeout(() => this.stop(), IDLE_TIMEOUT_MS);
    }
  }

  private onData(chunk: string): void {
    this.buffer += chunk;
    let newline: number;
    while ((newline = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, newline).trim();
      this.buffer = this.buffer.slice(newline + 1);
      if (line) {
        this.onLine(line);
      }
    }
  }

  private onLine(line: string): void {
    let message: { id?: number; result?: unknown; error?: string; event?: string };
    try {
      message = JSON.parse(line);
    } catch {
      this.log.appendLine(`Unexpected output: ${line}`);
      return;
    }
    if (message.event === "sessions-changed") {
      this.changed.fire();
      return;
    }
    const request = message.id === undefined ? undefined : this.pending.get(message.id);
    if (!request) {
      if (message.error) {
        this.log.appendLine(`Backend error: ${message.error}`);
      }
      return;
    }
    this.pending.delete(message.id!);
    "error" in message ? request.reject(message.error) : request.resolve(message.result);
  }

  private binaryPath(): string {
    const configured = vscode.workspace.getConfiguration("copilotSessionSearch").get<string>("sidecarPath")?.trim();
    if (configured) {
      return configured;
    }
    const name = process.platform === "win32" ? "copilot-session-search-sidecar.exe" : "copilot-session-search-sidecar";
    const bundled = path.join(this.extensionPath, "bin", name);
    if (!fs.existsSync(bundled)) {
      throw new Error(
        `The search backend is missing (${bundled}). This build of the extension may not support ${process.platform}-${process.arch}.`
      );
    }
    return bundled;
  }
}
