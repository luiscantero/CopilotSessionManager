import * as vscode from "vscode";
import { SessionsPanel, VIEW_TYPE } from "./panel";
import { searchSessions } from "./quickPick";
import { Sidecar } from "./sidecar";

export function activate(context: vscode.ExtensionContext): void {
  const log = vscode.window.createOutputChannel("Session Manager for GitHub Copilot");
  const sidecar = new Sidecar(context.extensionPath, log);

  context.subscriptions.push(
    log,
    sidecar,
    vscode.commands.registerCommand("copilotSessionManager.search", () => searchSessions(context, sidecar)),
    vscode.commands.registerCommand("copilotSessionManager.open", () => SessionsPanel.show(context, sidecar)),
    vscode.commands.registerCommand("copilotSessionManager.showLog", () => log.show()),
    vscode.window.registerWebviewPanelSerializer(VIEW_TYPE, {
      async deserializeWebviewPanel(panel: vscode.WebviewPanel) {
        SessionsPanel.revive(panel, context, sidecar);
      }
    })
  );
}

export function deactivate(): void {}
