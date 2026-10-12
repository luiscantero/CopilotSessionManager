import * as fs from "fs";
import * as vscode from "vscode";
import { SessionSummary } from "./sidecar";

/** Starts `copilot --resume=<id>` in a new terminal in the session's folder. */
export async function resumeInTerminal(session: SessionSummary): Promise<void> {
  if (session.source === "vscode") {
    return openInChat(session);
  }
  if (session.inUse) {
    const resume = "Resume Anyway";
    const choice = await vscode.window.showWarningMessage(
      `"${session.title}" is in use by another Copilot CLI process. Resuming it twice can mix up the conversation.`,
      { modal: true },
      resume
    );
    if (choice !== resume) {
      return;
    }
  }
  const command = vscode.workspace.getConfiguration("copilotSessionSearch").get<string>("copilotCommand")?.trim() || "copilot";
  const cwd = session.cwd && fs.existsSync(session.cwd) ? session.cwd : undefined;
  const terminal = vscode.window.createTerminal({
    name: `Copilot: ${truncate(session.title, 40)}`,
    cwd,
    iconPath: new vscode.ThemeIcon("copilot")
  });
  terminal.show();
  terminal.sendText(`${command} --resume=${session.id}`);
}

/** Opens the session in VS Code's Chat: VS Code chats natively, Copilot CLI sessions through Copilot Chat. */
export async function openInChat(session: SessionSummary): Promise<void> {
  if (session.source === "cli") {
    if (!vscode.extensions.getExtension("GitHub.copilot-chat")) {
      const resume = "Resume in Terminal";
      const choice = await vscode.window.showWarningMessage(
        "Opening Copilot CLI sessions in the Chat view requires the GitHub Copilot Chat extension.",
        resume
      );
      if (choice === resume) {
        await resumeInTerminal(session);
      }
      return;
    }
    await openUri(vscode.Uri.from({ scheme: "copilotcli", path: `/${session.id}` }));
    return;
  }

  const workspace = chatWorkspace(session);
  if (isCurrentWorkspace(workspace)) {
    await openUri(vscode.Uri.from({ scheme: "vscode-chat-session", authority: "local", path: `/${base64Url(session.id)}` }));
    return;
  }
  if (!workspace) {
    vscode.window.showInformationMessage(
      `"${session.title}" was created in a window without a folder. Open it from the Chat view of an empty VS Code window.`
    );
    return;
  }
  const open = "Open in New Window";
  const choice = await vscode.window.showInformationMessage(
    `"${session.title}" belongs to ${workspace.label}. VS Code shows a chat only in the window of its folder or workspace.`,
    { modal: true, detail: "In the new window, the chat is listed in the Chat view's session history." },
    open
  );
  if (choice === open) {
    await vscode.commands.executeCommand("vscode.openFolder", workspace.uri, { forceNewWindow: true });
  }
}

export async function copyResumeCommand(session: SessionSummary): Promise<void> {
  if (!session.resumeCommand) {
    return;
  }
  await vscode.env.clipboard.writeText(session.resumeCommand);
  vscode.window.setStatusBarMessage(`$(copy) Copied: ${session.resumeCommand}`, 3000);
}

interface ChatWorkspace {
  uri: vscode.Uri;
  isWorkspaceFile: boolean;
  label: string;
}

/** Folder or .code-workspace a VS Code chat belongs to, from the backend's `code ...` command. */
function chatWorkspace(session: SessionSummary): ChatWorkspace | undefined {
  const match = /^code (?:--(folder|file)-uri )?"(.*)"$/.exec(session.resumeCommand ?? "");
  if (!match) {
    return undefined;
  }
  const [, kind, target] = match;
  const uri = kind ? vscode.Uri.parse(target) : vscode.Uri.file(target);
  const isWorkspaceFile = kind === "file" || /\.code-workspace$/i.test(target);
  return { uri, isWorkspaceFile, label: kind ? target : uri.fsPath };
}

function isCurrentWorkspace(workspace: ChatWorkspace | undefined): boolean {
  const { workspaceFile, workspaceFolders } = vscode.workspace;
  const untitledWorkspace = workspaceFile?.scheme === "untitled";
  if (!workspace) {
    return !workspaceFolders?.length;
  }
  if (workspace.isWorkspaceFile) {
    return Boolean(workspaceFile && !untitledWorkspace && sameUri(workspaceFile, workspace.uri));
  }
  return !workspaceFile && workspaceFolders?.length === 1 && sameUri(workspaceFolders[0].uri, workspace.uri);
}

function sameUri(a: vscode.Uri, b: vscode.Uri): boolean {
  const normalize = (uri: vscode.Uri) => {
    const text = uri.with({ path: uri.path.replace(/\/+$/, "") }).toString();
    return uri.scheme === "file" && process.platform !== "linux" ? text.toLowerCase() : text;
  };
  return normalize(a) === normalize(b);
}

async function openUri(uri: vscode.Uri): Promise<void> {
  try {
    await vscode.commands.executeCommand("vscode.open", uri);
  } catch (error) {
    vscode.window.showErrorMessage(`Unable to open the chat: ${error instanceof Error ? error.message : error}`);
  }
}

function base64Url(text: string): string {
  return Buffer.from(text, "utf8").toString("base64url");
}

export function truncate(text: string, length: number): string {
  return text.length > length ? `${text.slice(0, length - 1)}…` : text;
}
