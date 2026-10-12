# Session Manager for GitHub Copilot (VS Code extension)

Full-text search across your local **GitHub Copilot CLI sessions** and **VS Code chats**, right
inside VS Code. It is the [Session Manager for GitHub Copilot](https://github.com/luiscantero/CopilotSessionSearch)
desktop app packaged as an extension: the same Rust search backend and the same UI, themed like
your editor. Unofficial; not affiliated with GitHub or Microsoft.

## Features

- **Quick search** (`Ctrl+Alt+F`, `Cmd+Alt+F` on macOS, or *Session Manager for GitHub Copilot: Search
  Sessions…*): type to search session titles and every user and Copilot message. All terms must
  match (case-insensitive). Results show the project, age, badges and the matching message.
  - `Enter` shows the conversation in the session browser (configurable, see below).
  - Item buttons: **Resume in terminal** (Copilot CLI sessions), **Open in VS Code chat** and
    **Copy resume command**.
  - Title buttons: show or hide sessions marked as done, and open the session browser.
- **Session browser** (*Session Manager for GitHub Copilot: Open Session Browser*): a session list and a
  conversation reader in an editor tab, with highlighting of matches, a repository filter,
  project merging for renamed or moved repositories, copy buttons and delete. It updates
  automatically while sessions change on disk.
- **Resume in terminal**: opens a terminal in the session's folder and runs
  `copilot --resume=<id>`. Asks first if the session is in use by another Copilot process.
- **Open in VS Code chat**:
  - VS Code chats open in the Chat editor when they belong to the current window's folder or
    workspace. Otherwise the extension offers to open their folder in a new window, where the chat
    is listed in the Chat view's history (VS Code only shows the chats of the open workspace).
  - Copilot CLI sessions open as Copilot CLI chat sessions (requires the GitHub Copilot Chat
    extension).

## How it works

The extension starts a small native backend (`bin/copilot-session-manager-sidecar`) and talks to it
with JSON lines over stdio. The backend keeps an in-memory index of
`~/.copilot/session-state/*/events.jsonl` and VS Code's chat storage. The first search after
starting indexes everything (a few seconds for about a thousand sessions); after that, only
changed files are read again, and a file watcher reports changes. Searches take milliseconds. Nothing is
written to disk except when you delete a session or merge projects. The backend stops after 10
minutes without use while no session browser is open.

Project merges are shared with the desktop app (`project-merges.json` in
`%APPDATA%/com.luiscantero.copilot-session-manager` on Windows).

## Settings

| Setting | Default | Description |
| --- | --- | --- |
| `copilotSessionManager.quickPick.defaultAction` | `showConversation` | What `Enter` does in the quick search: `showConversation`, `resume` or `openInChat`. |
| `copilotSessionManager.copilotCommand` | `copilot` | Command used to resume Copilot CLI sessions (`--resume=<id>` is appended). |
| `copilotSessionManager.sidecarPath` | | Path to a custom build of the backend. |

## Build from source

Requirements: Node.js 20+, Rust (stable).

```powershell
cd vscode-extension
npm install
npm run package   # copies ../src, builds the sidecar and TypeScript, writes copilot-session-manager-<platform>-<version>.vsix
code --install-extension copilot-session-manager-win32-x64-0.3.0.vsix
```

The VSIX contains a native binary, so it is platform-specific. `npm run package` builds for the
current platform; to build for another one, set `SIDECAR_TARGET` (Rust target triple, for
`npm run build`) and `VSCE_TARGET` (for example `darwin-arm64`, for `node scripts/package.mjs`).

For development, run `npm run build` once, then `npm run watch`, and start VS Code with
`code --extensionDevelopmentPath=<this folder>`.
The UI files in `media/app` are copied from `../src` by `npm run build:assets`.

## Limitations

- Same as the desktop app: *Set as done* is display-only, and session paths assume the default VS
  Code (stable) profile.
- Opening Copilot CLI sessions in the Chat view relies on the Copilot Chat extension's
  `copilotcli` chat session provider.
