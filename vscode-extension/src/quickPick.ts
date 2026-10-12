import * as vscode from "vscode";
import { copyResumeCommand, openInChat, resumeInTerminal, truncate } from "./actions";
import { SessionsPanel } from "./panel";
import { SearchHit, Sidecar } from "./sidecar";

const SHOW_DONE_KEY = "quickPick.showDone";

const resumeButton: vscode.QuickInputButton = { iconPath: new vscode.ThemeIcon("play"), tooltip: "Resume in terminal" };
const chatButton: vscode.QuickInputButton = { iconPath: new vscode.ThemeIcon("comment-discussion"), tooltip: "Open in VS Code chat" };
const copyButton: vscode.QuickInputButton = { iconPath: new vscode.ThemeIcon("copy"), tooltip: "Copy resume command" };
const panelButton: vscode.QuickInputButton = { iconPath: new vscode.ThemeIcon("history"), tooltip: "Open session browser" };

interface HitItem extends vscode.QuickPickItem {
  hit: SearchHit;
}

/** Searches session titles and messages as you type, like the session list of the app. */
export async function searchSessions(context: vscode.ExtensionContext, sidecar: Sidecar): Promise<void> {
  const quickPick = vscode.window.createQuickPick<HitItem>();
  const keepAlive = sidecar.retain();
  let showDone = context.globalState.get<boolean>(SHOW_DONE_KEY, false);
  let searchSeq = 0;
  let timer: NodeJS.Timeout | undefined;

  quickPick.title = "Copilot Sessions";
  quickPick.placeholder = "Search Copilot CLI sessions and VS Code chats by title or message";
  quickPick.matchOnDescription = false;
  quickPick.matchOnDetail = false;
  const doneButton = (): vscode.QuickInputButton => ({
    iconPath: new vscode.ThemeIcon(showDone ? "check-all" : "check"),
    tooltip: showDone ? "Hide sessions marked as done" : "Show sessions marked as done"
  });
  quickPick.buttons = [doneButton(), panelButton];

  const search = async () => {
    const seq = ++searchSeq;
    const query = quickPick.value;
    quickPick.busy = true;
    try {
      const hits = await sidecar.request<SearchHit[]>("search_sessions", { query });
      if (seq !== searchSeq) {
        return;
      }
      const items = hits.filter((hit) => showDone || !hit.done).map(toItem);
      const active = quickPick.activeItems[0]?.hit.id;
      quickPick.items = items;
      const keep = items.find((item) => item.hit.id === active);
      if (keep) {
        quickPick.activeItems = [keep];
      }
    } catch (error) {
      quickPick.items = [];
      vscode.window.showErrorMessage(`Session search failed: ${error}`);
    } finally {
      if (seq === searchSeq) {
        quickPick.busy = false;
      }
    }
  };
  const refreshAndSearch = async () => {
    quickPick.busy = true;
    try {
      await sidecar.refresh();
    } catch (error) {
      vscode.window.showErrorMessage(`Unable to index sessions: ${error}`);
    }
    await search();
  };

  quickPick.onDidChangeValue(() => {
    clearTimeout(timer);
    timer = setTimeout(search, 120);
  });
  quickPick.onDidAccept(() => {
    const item = quickPick.selectedItems[0] ?? quickPick.activeItems[0];
    if (item) {
      quickPick.hide();
      runDefaultAction(context, sidecar, item.hit, quickPick.value);
    }
  });
  quickPick.onDidTriggerItemButton(({ item, button }) => {
    if (button === copyButton) {
      copyResumeCommand(item.hit);
      return;
    }
    quickPick.hide();
    if (button === resumeButton) {
      resumeInTerminal(item.hit);
    } else if (button === chatButton) {
      openInChat(item.hit);
    }
  });
  quickPick.onDidTriggerButton((button) => {
    if (button === panelButton) {
      const query = quickPick.value;
      const active = quickPick.activeItems[0]?.hit.id;
      quickPick.hide();
      const panel = SessionsPanel.show(context, sidecar);
      if (active) {
        panel.reveal(active, query);
      }
      return;
    }
    showDone = !showDone;
    context.globalState.update(SHOW_DONE_KEY, showDone);
    quickPick.buttons = [doneButton(), panelButton];
    search();
  });
  const changes = sidecar.onDidChangeSessions(() => refreshAndSearch());
  quickPick.onDidHide(() => {
    clearTimeout(timer);
    changes.dispose();
    keepAlive.dispose();
    quickPick.dispose();
  });

  quickPick.show();
  // Show results from the current index right away and update them once it is fresh.
  if (sidecar.isIndexed) {
    await search();
  }
  await refreshAndSearch();
}

function runDefaultAction(context: vscode.ExtensionContext, sidecar: Sidecar, hit: SearchHit, query: string): void {
  const action = vscode.workspace.getConfiguration("copilotSessionManager").get<string>("quickPick.defaultAction");
  if (action === "resume") {
    resumeInTerminal(hit);
  } else if (action === "openInChat") {
    openInChat(hit);
  } else {
    SessionsPanel.show(context, sidecar).reveal(hit.id, query);
  }
}

function toItem(hit: SearchHit): HitItem {
  const badges = [hit.inUse ? "In use" : "", hit.done ? "Done" : "", hit.protection ? "Protected" : ""].filter(Boolean);
  const description = [hit.project.name, relativeTime(hit.updatedAt), ...badges].filter(Boolean).join(" · ");
  const snippet = hit.snippet
    ? `${hit.snippet.role === "user" ? "You" : "Copilot"}: ${truncate(hit.snippet.text.replace(/\s+/g, " "), 160)}`
    : undefined;
  const buttons = [
    ...(hit.source === "cli" ? [resumeButton] : []),
    chatButton,
    ...(hit.resumeCommand ? [copyButton] : [])
  ];
  return {
    hit,
    label: `$(${hit.source === "vscode" ? "comment-discussion" : "terminal"}) ${hit.title}`,
    description,
    detail: snippet,
    buttons,
    alwaysShow: true
  };
}

const UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ["year", 365 * 24 * 3600],
  ["month", 30 * 24 * 3600],
  ["week", 7 * 24 * 3600],
  ["day", 24 * 3600],
  ["hour", 3600],
  ["minute", 60]
];
const relativeFormat = new Intl.RelativeTimeFormat(vscode.env.language, { numeric: "auto", style: "short" });

function relativeTime(iso?: string | null): string {
  const time = Date.parse(iso ?? "");
  if (!time) {
    return "";
  }
  const seconds = (time - Date.now()) / 1000;
  for (const [unit, size] of UNITS) {
    if (Math.abs(seconds) >= size) {
      return relativeFormat.format(Math.round(seconds / size), unit);
    }
  }
  return "just now";
}
