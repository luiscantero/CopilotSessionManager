# Copilot Session Search

A small Rust + [Tauri 2](https://tauri.app) desktop app to find, read, resume and clean up
local GitHub Copilot (CLI / VS Code agent) sessions. Styled after VS Code Dark Modern.

## Features

- Session list (left) and chat view (right) in a portrait window with a draggable splitter.
- Remembers the window size between runs and always shrinks it to fit the current screen.
- Type-to-filter search over session titles and user/agent messages. All terms must match
  (AND), case-insensitive; matches are highlighted in the list and in the chat.
- Repository filter next to the search box (remembered between runs). Sessions are grouped by
  `repository` from `workspace.yaml`; sessions without one (e.g. started before the folder was a
  git repo, or in a subfolder) join the repository whose `git_root` contains their `cwd`, and
  otherwise fall back to their working folder.
- Merge projects that were renamed or moved (e.g. ShotNotebook -> ShotGallery -> ShotCollector):
  select the old project in the repository filter, click the merge button next to it and choose
  the new project. Its sessions are then grouped under the new name; chains resolve to the newest
  project, and merges can be undone from the same dialog. Projects whose folder no longer exists
  are marked "folder missing" as merge candidates. Merges only affect grouping in this app and are
  saved in `project-merges.json` in the app's config folder
  (`%APPDATA%/com.lcantero.copilot-session-search` on Windows).
- Copy button on every message bubble.
- Footer with the resume command (`copilot --resume=<id>`, copy button), **Set as done** and
  **Delete**.
- VS Code titles, `+/-` change counts and "done" state are shown when VS Code knows the session.
  Done sessions are hidden by default (toggle with the double-check button in the header).
- Sessions open in a running Copilot process are badged **In use** and cannot be deleted.
- Sessions owned by another app are badged **Protected**: **Delete** and **Set as done** are
  disabled (and deletion is refused by the backend), because removing them could break that
  app. A session counts as app-owned when `workspace.yaml` names a `client_name` other than the
  Copilot CLI (`github/cli`) or VS Code (`vscode-agent-host`), e.g. Windows Terminal Agent
  (`wta-master`), or when its working folder is inside application data (`%APPDATA%`,
  `%LOCALAPPDATA%`, `~/.config`, `~/.copilot`, ...) or a system folder (`C:\Windows`,
  `Program Files`, `/usr`, ...). Such sessions are grouped by app (for example
  "Windows Terminal Agent" or "Open Design") instead of by folder.
- VS Code local chats (the "Local" sessions in VS Code's Chat view, stored per workspace) are
  listed and searchable next to the Copilot CLI sessions, grouped by their workspace folder, and
  badged **VS Code chat**. The footer shows a `code "<folder>"` command to open the folder in VS
  Code. Chats that VS Code copied into several workspace storages are shown once, under their real
  folder. **Delete** removes the chat the way VS Code does: every copy of the chat file, its
  `chatEditingSessions/<id>` folder and its entries in the workspace's `state.vscdb` (chat index,
  agent session caches, todo list, terminal sessions). It is disabled while the chat's folder is
  open in a running VS Code window, because VS Code keeps the chat list in memory and would write
  the chat back; close that window first. A running VS Code is detected via `code.lock` (process
  id) and its open windows via `storage.json`.
- Refreshes automatically: a file watcher on `~/.copilot/session-state`, VS Code's
  `agent-host.db`, VS Code's chat folders and its window list (`storage.json`) picks up new,
  updated and removed sessions and opened or closed VS Code windows
  (debounced, at most every 5 s), with a 45 s polling fallback (e.g. for **In use** locks released
  by exiting processes, or chats archived in VS Code). The open chat keeps its scroll position, or
  follows new messages when scrolled to the bottom.
- Localized UI: English (default), German, French, Spanish, Italian, Portuguese (Brazil), Swedish,
  Chinese (Simplified), Japanese, Korean, Vietnamese, Thai, Hebrew and Arabic (right-to-left
  layout). On first start the language follows the OS/WebView language (falling back to English);
  change it with the cog button in the header. Dates and relative times use the selected locale.
  Messages from the backend (errors, protection reasons) are English.

## Translations

UI strings live in `src/locales/<code>.json` (flat `key: text` objects, `{name}` placeholders) and
the language list in `src/locales/languages.json` (`code`, native `name`, optional `"dir": "rtl"`).
`en-us.json` is the default and the reference. In the frontend, use `t("key", { name })` in
`main.js` and `data-i18n` / `data-i18n-title` / `data-i18n-placeholder` / `data-i18n-aria-label`
attributes in `index.html`. To add a language, add its JSON file and an entry in `languages.json`.
`cargo test --test locales` checks that every language has exactly the keys and placeholders of
`en-us.json`, that `languages.json` matches the files, and that every key used in the frontend
exists (and every key is used).

Keyboard: `Ctrl+F` / `Ctrl+K` focus search, `Esc` clear, `Up` / `Down` move, `Enter` open,
`Delete` delete, `F5` refresh.

## Data sources

| Data                     | Location                                                                         | Access    |
| ------------------------ | -------------------------------------------------------------------------------- | --------- |
| Conversations            | `~/.copilot/session-state/<id>/events.jsonl`                                     | read      |
| Session metadata         | `~/.copilot/session-state/<id>/workspace.yaml`                                   | read      |
| In-use detection         | `~/.copilot/session-state/<id>/inuse.<pid>.hold` (exclusively locked while live) | read      |
| VS Code title/done/diffs | `%APPDATA%/Code/User/globalStorage/agent-host.db` (`sessions_v2`)                | read-only |
| VS Code id -> session id | `%APPDATA%/Code/agentSessionData/<vscode-id>/session.db`                         | read-only |
| VS Code local chats      | `%APPDATA%/Code/User/workspaceStorage/<hash>/chatSessions/<id>.json[l]`          | read/delete |
| VS Code chat edits       | `%APPDATA%/Code/User/workspaceStorage/<hash>/chatEditingSessions/<id>/`          | delete    |
| VS Code chat folder      | `%APPDATA%/Code/User/workspaceStorage/<hash>/workspace.json`                     | read-only |
| VS Code chat title/done  | `%APPDATA%/Code/User/workspaceStorage/<hash>/state.vscdb` (`ItemTable`)          | read/write |
| VS Code no-folder chats  | `%APPDATA%/Code/User/globalStorage/emptyWindowChatSessions/<id>.json[l]` (index in `globalStorage/state.vscdb`) | read/delete |
| VS Code running          | `%APPDATA%/Code/code.lock` (main process id)                                     | read-only |
| VS Code open windows     | `%APPDATA%/Code/User/globalStorage/storage.json` (`windowsState`)                | read-only |

Only user prompts and top-level assistant replies are shown; tool calls, sub-agent output,
thinking and injected system/skill messages are skipped.

## Prerequisites

- Rust (stable, MSVC toolchain on Windows) and the WebView2 runtime (preinstalled on Windows 11).
- Tauri CLI: `cargo install tauri-cli --version "^2"`

## Run and build

```powershell
cargo tauri dev     # run with hot reload
cargo tauri build   # release exe + installers under src-tauri/target/release
```

Tests (the ignored test indexes your real local sessions and prints timings):

```powershell
cd src-tauri
cargo test
cargo test --release -- --include-ignored --nocapture
cargo clippy --all-targets -- -D warnings
```

## Limitations

- **Set as done** is display-only for now. VS Code owns that state in `agent-host.db`
  (versioned, hashed records), so it is not written externally; use "Set as done" in VS Code.
- **Delete** of a Copilot CLI session removes its folder under `~/.copilot/session-state` only.
  Entries in `~/.copilot/session-store.db` and VS Code's session list are not touched and may
  linger until those tools clean them up.
- Session paths assume the default VS Code (stable) profile location.
- VS Code local chats cannot be archived or resumed from here; the footer command only opens their
  folder. Chats of remote (WSL/SSH) workspaces are grouped by their remote path. If VS Code is
  running but its window list can't be read, every VS Code chat counts as open and can't be
  deleted until VS Code is closed.
