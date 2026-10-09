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
- Copy button on every message bubble.
- Footer with the `copilot --resume=<id>` command (copy button), **Set as done** and **Delete**.
- VS Code titles, `+/-` change counts and "done" state are shown when VS Code knows the session.
  Done sessions are hidden by default (toggle with the double-check button in the header).
- Sessions open in a running Copilot process are badged **In use** and cannot be deleted.

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

Only user prompts and top-level assistant replies are shown; tool calls, sub-agent output and
injected system/skill messages are skipped.

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
- **Delete** removes the session folder under `~/.copilot/session-state` only. Entries in
  `~/.copilot/session-store.db` and VS Code's session list are not touched and may linger until
  those tools clean them up.
- Session paths assume the default VS Code (stable) profile location.
