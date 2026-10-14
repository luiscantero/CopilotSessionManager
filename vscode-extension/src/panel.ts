import * as crypto from "crypto";
import * as fs from "fs";
import * as vscode from "vscode";
import { openInChat, resumeInTerminal } from "./actions";
import { SessionDetail, Sidecar } from "./sidecar";

export const VIEW_TYPE = "copilotSessionManager";
const STORAGE_KEY = "webviewStorage";
/** Commands the extension handles itself instead of the backend; shown as extra buttons in the app. */
const HOST_ACTIONS = ["resume_session", "open_in_chat"];

type WebviewMessage =
  | { type: "ready" }
  | { type: "invoke"; id: number; command: string; args: Record<string, unknown> }
  | { type: "storage"; key: string; value: string | null };

/** The desktop app's UI (src/), running in a webview against the sidecar. */
export class SessionsPanel implements vscode.Disposable {
  private static current?: SessionsPanel;
  private ready = false;
  private disposed = false;
  private readonly outbox: unknown[] = [];
  private readonly disposables: vscode.Disposable[] = [];

  static show(context: vscode.ExtensionContext, sidecar: Sidecar): SessionsPanel {
    if (SessionsPanel.current) {
      SessionsPanel.current.panel.reveal();
      return SessionsPanel.current;
    }
    const panel = vscode.window.createWebviewPanel(VIEW_TYPE, "Copilot Sessions", vscode.ViewColumn.Active, {
      enableScripts: true,
      retainContextWhenHidden: true
    });
    return new SessionsPanel(panel, context, sidecar);
  }

  static revive(panel: vscode.WebviewPanel, context: vscode.ExtensionContext, sidecar: Sidecar): void {
    SessionsPanel.current?.dispose();
    new SessionsPanel(panel, context, sidecar);
  }

  private constructor(
    private readonly panel: vscode.WebviewPanel,
    private readonly context: vscode.ExtensionContext,
    private readonly sidecar: Sidecar
  ) {
    SessionsPanel.current = this;
    const media = vscode.Uri.joinPath(context.extensionUri, "media");
    panel.iconPath = vscode.Uri.joinPath(context.extensionUri, "icon.png");
    panel.webview.options = { enableScripts: true, localResourceRoots: [media] };
    panel.webview.html = this.html(media);
    this.disposables.push(
      sidecar.retain(),
      panel.onDidDispose(() => this.dispose()),
      panel.webview.onDidReceiveMessage((message: WebviewMessage) => this.onMessage(message)),
      sidecar.onDidChangeSessions(() => this.post({ type: "event", event: "sessions-changed" }))
    );
  }

  /** Shows a session in the app, optionally with the search that found it. */
  reveal(id: string, query = ""): void {
    this.panel.reveal();
    this.post({ type: "event", event: "reveal-session", payload: { id, query } });
  }

  dispose(): void {
    if (this.disposed) {
      return;
    }
    this.disposed = true;
    if (SessionsPanel.current === this) {
      SessionsPanel.current = undefined;
    }
    this.panel.dispose();
    vscode.Disposable.from(...this.disposables.splice(0)).dispose();
  }

  private send(message: unknown): void {
    if (!this.disposed) {
      this.panel.webview.postMessage(message);
    }
  }

  private post(message: unknown): void {
    if (this.ready) {
      this.send(message);
    } else {
      this.outbox.push(message);
    }
  }

  private async onMessage(message: WebviewMessage): Promise<void> {
    switch (message.type) {
      case "ready":
        this.ready = true;
        this.outbox.splice(0).forEach((queued) => this.send(queued));
        break;
      case "storage": {
        const storage = { ...this.context.globalState.get<Record<string, string>>(STORAGE_KEY, {}) };
        if (message.value === null) {
          delete storage[message.key];
        } else {
          storage[message.key] = message.value;
        }
        await this.context.globalState.update(STORAGE_KEY, storage);
        break;
      }
      case "invoke":
        try {
          const result = await this.invoke(message.command, message.args);
          this.send({ type: "response", id: message.id, result: result ?? null });
        } catch (error) {
          const text = error instanceof Error ? error.message : String(error);
          this.send({ type: "response", id: message.id, error: text });
        }
        break;
    }
  }

  private async invoke(command: string, args: Record<string, unknown>): Promise<unknown> {
    if (HOST_ACTIONS.includes(command)) {
      const { session } = await this.sidecar.request<SessionDetail>("get_session", { id: args.id });
      return command === "resume_session" ? resumeInTerminal(session) : openInChat(session);
    }
    if (command === "refresh_sessions") {
      const folders = Array.isArray(args.folders) ? args.folders.filter((folder) => typeof folder === "string") : [];
      return this.sidecar.refresh(folders);
    }
    return this.sidecar.request(command, args);
  }

  private html(media: vscode.Uri): string {
    const webview = this.panel.webview;
    const app = vscode.Uri.joinPath(media, "app");
    const nonce = crypto.randomBytes(16).toString("base64");
    const init = {
      hostActions: HOST_ACTIONS,
      storage: this.context.globalState.get<Record<string, string>>(STORAGE_KEY, {}),
      workspaceFolders: (vscode.workspace.workspaceFolders ?? [])
        .filter((folder) => folder.uri.scheme === "file")
        .map((folder) => folder.uri.fsPath)
    };
    const csp = [
      "default-src 'none'",
      `img-src ${webview.cspSource} data:`,
      `font-src ${webview.cspSource}`,
      `style-src ${webview.cspSource} 'unsafe-inline'`,
      `script-src 'nonce-${nonce}'`,
      `connect-src ${webview.cspSource}`
    ].join("; ");
    const head = [
      `<meta http-equiv="Content-Security-Policy" content="${csp}" />`,
      `<base href="${webview.asWebviewUri(app)}/" />`,
      `<script type="application/json" id="host-init">${JSON.stringify(init).replace(/</g, "\\u003c")}</script>`,
      `<script nonce="${nonce}" src="${webview.asWebviewUri(vscode.Uri.joinPath(media, "host.js"))}"></script>`
    ].join("\n    ");
    const theme = `<link rel="stylesheet" href="${webview.asWebviewUri(vscode.Uri.joinPath(media, "vscode-theme.css"))}" />`;

    return fs
      .readFileSync(vscode.Uri.joinPath(app, "index.html").fsPath, "utf8")
      .replace("<head>", `<head>\n    ${head}`)
      .replace(/(<link rel="stylesheet" href="styles\.css" \/>)/, `$1\n    ${theme}`)
      .replace(/<script src="main\.js"><\/script>/, `<script nonce="${nonce}" src="main.js"></script>`);
  }
}
