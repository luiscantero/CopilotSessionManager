"""Creates a sandbox with fictional Copilot CLI sessions and VS Code chats for screenshots.

Usage: python make_demo_data.py <sandbox dir>
Run the sidecar / VS Code with COPILOT_HOME=<sandbox>/copilot and APPDATA=<sandbox>/appdata
(see start-demo.ps1).
"""
import json
import shutil
import sqlite3
import sys
import uuid
from datetime import datetime, timedelta, timezone
from pathlib import Path
from urllib.parse import quote

root = Path(sys.argv[1]).resolve()
shutil.rmtree(root, ignore_errors=True)
copilot = root / "copilot" / "session-state"
appdata = root / "appdata"
repos = root / "repos"
now = datetime.now(timezone.utc)
ns = uuid.UUID("6f1c2a43-9b7e-4d1a-8c55-0d3e5b2f7a10")


def compact(value):
    # The CLI writes compact JSON with `type` first; the backend relies on that to skip tool events.
    return json.dumps(value, separators=(",", ":"))


def iso(t):
    return t.strftime("%Y-%m-%dT%H:%M:%S.") + f"{t.microsecond // 1000:03d}Z"


def repo(name, org):
    path = repos / name
    (path / ".git").mkdir(parents=True, exist_ok=True)
    return {"path": str(path), "repository": f"{org}/{name}"}


REPOS = {
    "storefront": repo("storefront-web", "contoso"),
    "payments": repo("payments-api", "contoso"),
    "mobile": repo("field-service-app", "fabrikam"),
    "analytics": repo("sales-analytics", "northwind"),
    "game": repo("game-server", "tailspin"),
}

FLAKY_ANSWER = """The flaky failure comes from `CheckoutSummary.test.tsx`: it renders before the mocked cart
request resolves, so `getByText("Total")` sometimes runs against the loading state.

I changed the test to wait for the request instead of relying on timing:

```tsx
render(<CheckoutSummary cartId="c-42" />);
expect(await screen.findByText("Total: $128.40")).toBeInTheDocument();
```

- Replaced 3 `getBy*` queries with `findBy*` after async renders
- Removed the `setTimeout(..., 50)` workaround in `test/setup.ts`
- Ran the suite 50 times in a loop: **0 failures** (was 7/50 before)"""

SESSIONS = [
    # key, repo, branch, summary, hours ago, duration (min), (additions, deletions), done, messages
    ("flaky", "storefront", "fix/flaky-checkout-tests", "Fix flaky checkout tests", 0.6, 40, (64, 31), False, [
        ("user", "The checkout tests fail randomly in CI, maybe 1 in 7 runs. Can you find out why and fix it?"),
        ("assistant", "I'll run the checkout tests in a loop to reproduce the failure, then look at the failing assertions."),
        ("assistant", FLAKY_ANSWER),
        ("user", "Great. Does the same pattern exist in other tests?"),
        ("assistant", "Yes, two more files use `getBy*` right after an async render: `OrderHistory.test.tsx` and "
                      "`AddressForm.test.tsx`. I updated both the same way; the full suite passes 50/50 runs now."),
    ]),
    ("refunds", "payments", "main", "Add idempotency keys to refund endpoint", 3, 55, (212, 18), False, [
        ("user", "Refunds are sometimes processed twice when the client retries. Add idempotency keys to POST /refunds."),
        ("assistant", "I'll store each `Idempotency-Key` header with the response for 24 hours and return the stored "
                      "response for repeated keys.\n\n```csharp\nif (await _keys.TryGetAsync(key) is { } cached)\n"
                      "    return cached.ToResult();\n```\n\nAdded a unique index on `(merchant_id, idempotency_key)` "
                      "so concurrent retries cannot both succeed."),
        ("user", "Please also add tests for concurrent retries."),
        ("assistant", "Added `RefundIdempotencyTests` with 3 cases: sequential retry, concurrent retry (10 parallel "
                      "requests) and key reuse with a different body (returns 422). All tests pass."),
    ]),
    ("react19", "storefront", "chore/upgrade-react-19", "Upgrade to React 19", 26, 90, (418, 377), False, [
        ("user", "Upgrade the storefront to React 19 and fix whatever breaks."),
        ("assistant", "Upgraded `react`, `react-dom` and the types. Three breaking changes needed fixes:\n\n"
                      "1. `ReactDOM.render` -> `createRoot` in `src/main.tsx`\n"
                      "2. Removed `defaultProps` from 6 function components\n"
                      "3. Replaced string refs in the legacy `Carousel`\n\nBuild and tests pass."),
    ]),
    ("offline", "mobile", "feature/offline-sync", "Offline sync for work orders", 30, 120, (936, 104), False, [
        ("user", "Technicians lose their edits when the app goes offline. Design an offline sync for work orders."),
        ("assistant", "Proposal: keep a local SQLite outbox of edits, sync it when connectivity returns, and resolve "
                      "conflicts per field with last-writer-wins plus a conflict banner for the technician."),
        ("user", "Sounds good, implement it."),
        ("assistant", "Implemented `OutboxRepository`, `SyncWorker` (exponential backoff) and the conflict banner. "
                      "Edits made in airplane mode now sync within 5 seconds of reconnecting."),
    ]),
    ("pipeline", "analytics", "main", "Speed up nightly sales pipeline", 50, 35, (88, 140), False, [
        ("user", "The nightly sales pipeline takes 3 hours. Find the bottleneck."),
        ("assistant", "Profiling shows 70% of the time in `join_regions()`: it loads the full regions table for every "
                      "partition. Broadcasting the small table and filtering early brings the run down to **38 minutes**."),
    ]),
    ("matchmaking", "game", "feature/skill-matchmaking", "Skill-based matchmaking", 75, 80, (521, 66), False, [
        ("user", "Players complain about unfair matches. Add skill-based matchmaking."),
        ("assistant", "Added a Glicko-2 rating per player and a matchmaking queue that widens the allowed rating gap "
                      "by 50 points every 10 seconds of waiting, so nobody waits longer than a minute."),
    ]),
    ("docker", "payments", "chore/docker-slim", "Shrink the API container image", 98, 25, (24, 39), True, [
        ("user", "Our container image is 1.2 GB. Make it smaller."),
        ("assistant", "Switched to a multi-stage build on the chiseled .NET runtime image. The image is now **118 MB** "
                      "and has no shell or package manager."),
    ]),
    ("a11y", "storefront", "fix/a11y-audit", "Fix accessibility audit findings", 120, 60, (143, 52), False, [
        ("user", "Fix the issues from the accessibility audit report in docs/a11y-audit.md."),
        ("assistant", "Fixed 14 of 15 findings: missing labels, focus order in the cart drawer, color contrast of the "
                      "sale badge and keyboard access to the size picker. The remaining one (video captions) needs "
                      "content from marketing."),
    ]),
    ("ratelimit", "payments", "main", "Rate limiting per merchant", 140, 45, (176, 12), False, [
        ("user", "Add rate limiting per merchant to the public API."),
        ("assistant", "Added a token bucket per merchant (100 requests/s, burst 200) backed by Redis, with "
                      "`Retry-After` headers on 429 responses."),
    ]),
    ("camera", "mobile", "fix/camera-crash", "Camera crash on Android 15", 170, 30, (19, 7), False, [
        ("user", "The app crashes when opening the camera on Android 15 devices."),
        ("assistant", "The crash is a missing `READ_MEDIA_VISUAL_USER_SELECTED` permission check. Added the check "
                      "and a fallback to the photo picker."),
    ]),
    ("dashboard", "analytics", "feature/region-dashboard", "Regional sales dashboard", 220, 70, (402, 28), False, [
        ("user", "Build a dashboard of sales per region with a month filter."),
        ("assistant", "Added the dashboard with a choropleth map, a month range filter and a table of the top 10 "
                      "products per region."),
    ]),
    ("leaderboard", "game", "main", "Weekly leaderboard reset", 300, 20, (57, 9), False, [
        ("user", "Reset the leaderboard every Monday at 00:00 UTC and archive the old one."),
        ("assistant", "Added a scheduled job that snapshots the leaderboard to `leaderboard_archive` and resets "
                      "scores in one transaction."),
    ]),
]

agent_rows = []
for key, repo_key, branch, summary, hours, minutes, (adds, dels), done, messages in SESSIONS:
    sid = str(uuid.uuid5(ns, key))
    info = REPOS[repo_key]
    d = copilot / sid
    d.mkdir(parents=True)
    end = now - timedelta(hours=hours)
    start = end - timedelta(minutes=minutes)
    step = (end - start) / len(messages)
    lines = [compact({"type": "session.start", "data": {}, "timestamp": iso(start)})]
    for i, (role, text) in enumerate(messages):
        lines.append(compact({"type": f"{role}.message", "data": {"content": text},
                                 "timestamp": iso(start + step * (i + 1))}))
    (d / "events.jsonl").write_text("\n".join(lines) + "\n", encoding="utf-8")
    (d / "workspace.yaml").write_text(
        f"id: {sid}\ncwd: {info['path']}\ngit_root: {info['path']}\nrepository: {info['repository']}\n"
        f"branch: {branch}\nsummary: {summary}\nclient_name: github/cli\n"
        f"created_at: {iso(start)}\nupdated_at: {iso(end)}\n",
        encoding="utf-8",
    )
    agent_rows.append((sid, summary, done, adds, dels))

# A session started by another app is shown as Protected and cannot be deleted.
sid = str(uuid.uuid5(ns, "terminal-agent"))
d = copilot / sid
d.mkdir(parents=True)
t = now - timedelta(hours=8)
(d / "events.jsonl").write_text("\n".join(compact(e) for e in [
    {"type": "user.message", "data": {"content": "Which process is using port 8080?"}, "timestamp": iso(t)},
    {"type": "assistant.message", "data": {"content": "`node.exe` (PID 21844) is listening on port 8080."},
     "timestamp": iso(t + timedelta(seconds=20))},
]) + "\n", encoding="utf-8")
(d / "workspace.yaml").write_text(
    f"id: {sid}\ncwd: {repos}\nsummary: Find process using port 8080\nclient_name: wta-master\n"
    f"created_at: {iso(t)}\nupdated_at: {iso(t + timedelta(seconds=20))}\n",
    encoding="utf-8",
)

# VS Code's agent host database: titles, done state and change counts of CLI sessions.
global_storage = appdata / "Code" / "User" / "globalStorage"
global_storage.mkdir(parents=True)
db = sqlite3.connect(global_storage / "agent-host.db")
db.execute("CREATE TABLE sessions_v2 (session_uri TEXT, provider TEXT, payload TEXT)")
for sid, summary, done, adds, dels in agent_rows:
    payload = {"data": {"summary": summary, "isArchived": done, "changes": {"additions": adds, "deletions": dels}}}
    db.execute("INSERT INTO sessions_v2 VALUES (?, 'copilotcli', ?)", (f"copilotcli:/{sid}", json.dumps(payload)))
db.commit()
db.close()

# VS Code local chats, one workspace storage per repository.
CHATS = [
    ("storefront", "Explain the cart state management", 5, [
        ("How does the cart state work? I see both a context and a reducer.",
         "The cart uses a reducer (`cartReducer.ts`) wrapped in `CartProvider`. Components read it with `useCart()`; "
         "every action is persisted to `localStorage` by the `persistCart` middleware, so the cart survives reloads."),
    ]),
    ("payments", "Webhook signature validation", 52, [
        ("How do we validate incoming webhook signatures?",
         "`WebhookSignatureFilter` computes an HMAC-SHA256 of the raw body with the merchant's secret and compares it "
         "with the `X-Signature` header using a constant-time comparison. Requests older than 5 minutes are rejected."),
        ("Can we rotate secrets without downtime?",
         "Yes: store two active secrets per merchant and accept a signature from either during the rotation window."),
    ]),
    ("analytics", "Write a SQL query for monthly churn", 130, [
        ("Write a query for monthly customer churn.",
         "```sql\nSELECT month, 1.0 * lost / NULLIF(active_start, 0) AS churn_rate\n"
         "FROM monthly_customer_counts\nORDER BY month;\n```"),
    ]),
    ("game", "Tune the server tick rate", 260, [
        ("Should we raise the server tick rate from 30 to 60?",
         "At 60 Hz CPU use per match roughly doubles. Measured headroom allows it for up to 40 concurrent matches "
         "per node."),
    ]),
]
for i, (repo_key, title, hours, turns) in enumerate(CHATS):
    info = REPOS[repo_key]
    storage = appdata / "Code" / "User" / "workspaceStorage" / f"{i + 1:032x}"
    (storage / "chatSessions").mkdir(parents=True)
    uri = "file:///" + quote(Path(info["path"]).as_posix(), safe="/")
    (storage / "workspace.json").write_text(json.dumps({"folder": uri}), encoding="utf-8")
    start = now - timedelta(hours=hours)
    requests = [{
        "requestId": f"request_{j}",
        "message": {"text": question, "parts": []},
        "timestamp": int((start + timedelta(minutes=3 * j)).timestamp() * 1000),
        "response": [{"value": answer}],
    } for j, (question, answer) in enumerate(turns)]
    chat = {
        "version": 3,
        "creationDate": int(start.timestamp() * 1000),
        "lastMessageDate": int((start + timedelta(minutes=3 * len(turns))).timestamp() * 1000),
        "customTitle": title,
        "requests": requests,
    }
    cid = str(uuid.uuid5(ns, f"chat-{i}"))
    (storage / "chatSessions" / f"{cid}.json").write_text(json.dumps(chat), encoding="utf-8")

print(json.dumps({
    "copilotHome": str(root / "copilot"),
    "appData": str(appdata),
    "inUseHold": str(copilot / str(uuid.uuid5(ns, "flaky")) / "inuse.4242.hold"),
}))
