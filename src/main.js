"use strict";

const invoke = window.__TAURI__?.core?.invoke;

const ICONS = {
  check: '<svg viewBox="0 0 16 16"><path d="M3 8.5l3 3 7-7"/></svg>',
  checkAll: '<svg viewBox="0 0 16 16"><path d="M1.5 8.5l3 3 7-7M7.4 11.1l.9.9 6.2-6.2"/></svg>',
  close: '<svg viewBox="0 0 16 16"><path d="M4 4l8 8M12 4l-8 8"/></svg>',
  copilot: '<svg viewBox="0 0 16 16"><path d="M4 6.2C4 4.4 5.4 3 7.2 3h1.6C10.6 3 12 4.4 12 6.2v3.6c0 1.8-1.4 3.2-3.2 3.2H7.2C5.4 13 4 11.6 4 9.8z"/><path d="M2 8h2M12 8h2M6.5 7.5v1M9.5 7.5v1"/></svg>',
  merge: '<svg viewBox="0 0 16 16"><path d="M4.5 2v3c0 2.5 3.5 3 3.5 5.5V14M11.5 2v3c0 2.5-3.5 3-3.5 5.5M6 12.5l2 2 2-2"/></svg>',
  copy: '<svg viewBox="0 0 16 16"><path d="M3.5 10.5V3.2c0-.4.3-.7.7-.7h6.3"/><rect x="5.5" y="4.5" width="7" height="9" rx=".8"/></svg>',
  refresh: '<svg viewBox="0 0 16 16"><path d="M13 8a5 5 0 1 1-1.5-3.6M13 2.5v3h-3"/></svg>',
  search: '<svg viewBox="0 0 16 16"><circle cx="6.8" cy="6.8" r="4.3"/><path d="M10 10l4 4"/></svg>',
  terminal: '<svg viewBox="0 0 16 16"><path d="M3 4.5l3.5 3.5L3 11.5M8 12h5"/></svg>',
  trash: '<svg viewBox="0 0 16 16"><path d="M2.5 4.5h11M6 4.5v-2h4v2M4 4.5l.7 9h6.6l.7-9M6.8 7v4.5M9.2 7v4.5"/></svg>',
  user: '<svg viewBox="0 0 16 16"><circle cx="8" cy="5.5" r="2.7"/><path d="M2.8 13.5c.6-2.6 2.6-4 5.2-4s4.6 1.4 5.2 4"/></svg>'
};

const el = {
  setDone: document.getElementById("set-done"),
  chat: document.getElementById("chat"),
  chatHeader: document.getElementById("chat-header"),
  chatMeta: document.getElementById("chat-meta"),
  chatTitle: document.getElementById("chat-title"),
  clearSearch: document.getElementById("clear-search"),
  columns: document.getElementById("columns"),
  copyResume: document.getElementById("copy-resume"),
  count: document.getElementById("count"),
  delete: document.getElementById("delete"),
  dialog: document.getElementById("dialog-backdrop"),
  dialogCancel: document.getElementById("dialog-cancel"),
  dialogConfirm: document.getElementById("dialog-confirm"),
  dialogText: document.getElementById("dialog-text"),
  list: document.getElementById("session-list"),
  listEmpty: document.getElementById("list-empty"),
  mergeButton: document.getElementById("merge-project"),
  mergeCancel: document.getElementById("merge-cancel"),
  mergeConfirm: document.getElementById("merge-confirm"),
  mergeDialog: document.getElementById("merge-backdrop"),
  mergedList: document.getElementById("merged-list"),
  mergedSection: document.getElementById("merged-section"),
  mergeTarget: document.getElementById("merge-target"),
  mergeText: document.getElementById("merge-text"),
  projectFilter: document.getElementById("project-filter"),
  refresh: document.getElementById("refresh"),
  resumeCommand: document.getElementById("resume-command"),
  search: document.getElementById("search"),
  splitter: document.getElementById("splitter"),
  toast: document.getElementById("toast"),
  toggleDone: document.getElementById("toggle-done")
};

const state = {
  hits: [],
  selectedId: null,
  selectedSession: null,
  showDone: localStorage.getItem("showDone") === "true",
  project: localStorage.getItem("project") || "",
  projectName: localStorage.getItem("projectName") || "",
  searchSeq: 0,
  detailSeq: 0,
  searchTimer: 0,
  toastTimer: 0,
  refreshing: false,
  refreshPending: false,
  chatSessionId: null,
  chatMessageCount: 0
};

function icon(name) {
  return ICONS[name] || "";
}

function hydrateIcons(root = document) {
  root.querySelectorAll("[data-icon]").forEach((node) => {
    node.innerHTML = icon(node.dataset.icon);
  });
}

function escapeHtml(text) {
  return String(text)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function searchTerms() {
  return el.search.value.trim().split(/\s+/).filter(Boolean);
}

function termsRegExp(terms) {
  if (!terms.length) {
    return null;
  }
  const sorted = [...terms].sort((a, b) => b.length - a.length).map(escapeRegExp);
  return new RegExp(`(${sorted.join("|")})`, "gi");
}

function highlight(text, terms) {
  const regex = termsRegExp(terms);
  if (!regex) {
    return escapeHtml(text);
  }
  return String(text)
    .split(regex)
    .map((part, index) => (index % 2 ? `<mark>${escapeHtml(part)}</mark>` : escapeHtml(part)))
    .join("");
}

function relativeTime(iso) {
  const time = Date.parse(iso || "");
  if (!time) {
    return "";
  }
  const seconds = Math.max(0, (Date.now() - time) / 1000);
  const units = [
    [60, "min", 60],
    [3600, "hr", 24],
    [86400, "day", 7],
    [604800, "wk", 5],
    [2629800, "mo", 12],
    [31557600, "yr", Infinity]
  ];
  if (seconds < 60) {
    return "just now";
  }
  for (const [size, label, limit] of units) {
    const value = Math.floor(seconds / size);
    if (value < limit) {
      return `${value} ${label}${value === 1 ? "" : "s"} ago`;
    }
  }
  return "";
}

function formatDate(iso) {
  const time = Date.parse(iso || "");
  return time
    ? new Date(time).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })
    : "";
}

function doneFilteredHits() {
  return state.showDone ? state.hits : state.hits.filter((hit) => !hit.done);
}

function visibleHits() {
  const hits = doneFilteredHits();
  return state.project ? hits.filter((hit) => hit.project.key === state.project) : hits;
}

function setProject(key) {
  state.project = key;
  const match = state.hits.find((hit) => hit.project.key === key);
  state.projectName = key ? match?.project.name || state.projectName : "";
  localStorage.setItem("project", state.project);
  localStorage.setItem("projectName", state.projectName);
}

function renderProjectOptions() {
  const base = doneFilteredHits();
  const groups = new Map();
  for (const { project } of base) {
    const group = groups.get(project.key) || { ...project, count: 0 };
    group.count++;
    groups.set(project.key, group);
  }
  if (state.project && !groups.has(state.project)) {
    groups.set(state.project, { key: state.project, name: state.projectName || state.project, detail: "", count: 0 });
  }
  const projects = [...groups.values()].sort(
    (a, b) =>
      a.name.localeCompare(b.name, undefined, { sensitivity: "base" }) || a.detail.localeCompare(b.detail)
  );
  const nameCounts = new Map();
  projects.forEach(({ name }) => nameCounts.set(name.toLowerCase(), (nameCounts.get(name.toLowerCase()) || 0) + 1));

  el.projectFilter.innerHTML = [
    `<option value="">All repositories (${base.length})</option>`,
    ...projects.map(({ key, name, detail, count, missing, merged }) => {
      const label = nameCounts.get(name.toLowerCase()) > 1 && detail ? `${name} — ${detail}` : name;
      const notes = [missing ? "folder missing" : "", merged?.length ? `+${merged.length} merged` : ""].filter(Boolean);
      const suffix = notes.length ? ` · ${notes.join(" · ")}` : "";
      return `<option value="${escapeHtml(key)}" title="${escapeHtml(projectTooltip({ detail, missing, merged }))}">${escapeHtml(label)} (${count})${escapeHtml(suffix)}</option>`;
    })
  ].join("");
  el.projectFilter.value = state.project;
  el.projectFilter.classList.toggle("active", Boolean(state.project));
  const selected = groups.get(state.project);
  el.projectFilter.title = selected ? projectTooltip(selected) || selected.name : "Filter by repository or folder";
  el.mergeButton.disabled = !isMergeable(state.project);
  el.mergeButton.title = isMergeable(state.project)
    ? "Merge this project into another (renamed or moved repository)"
    : "Select a repository or folder to merge it into another";
}

function isMergeable(key) {
  return key.startsWith("repo:") || key.startsWith("path:");
}

function projectTooltip({ detail, missing, merged }) {
  return [
    detail,
    missing ? "Folder no longer exists (renamed or moved?). Use the merge button to group it with the new project." : "",
    merged?.length ? `Includes: ${merged.map((project) => project.name).join(", ")}` : ""
  ]
    .filter(Boolean)
    .join("\n");
}

function resumeCommand(session) {
  return session.resumeCommand || "";
}

function resumeHint(session) {
  if (!session) {
    return "";
  }
  if (session.source === "vscode") {
    return session.resumeCommand
      ? "Opens the chat's folder in VS Code; the chat is listed in VS Code's Chat view"
      : "This chat has no folder to open";
  }
  return session.cwd ? `Run from ${session.cwd}` : "";
}

function renderList() {
  const terms = searchTerms();
  if (state.project && !terms.length && state.hits.length && !state.hits.some((hit) => hit.project.key === state.project)) {
    setProject("");
  }
  renderProjectOptions();
  const hits = visibleHits();
  const total = state.hits.length;
  el.count.textContent = hits.length === total ? `${total}` : `${hits.length} of ${total}`;

  if (!hits.some((hit) => hit.id === state.selectedId)) {
    state.selectedId = null;
  }

  el.list.innerHTML = hits
    .map((hit) => {
      const meta = [];
      if (!state.project) {
        meta.push(`<span class="repo" title="${escapeHtml(hit.project.detail)}">${escapeHtml(hit.project.name)}</span>`);
      }
      if (hit.additions || hit.deletions) {
        meta.push(
          `<span><span class="added">+${hit.additions ?? 0}</span> <span class="deleted">-${hit.deletions ?? 0}</span></span>`
        );
      }
      meta.push(`<span>${escapeHtml(relativeTime(hit.updatedAt))}</span>`);
      const badges = [
        hit.inUse ? '<span class="badge live">In use</span>' : "",
        hit.source === "vscode"
          ? `<span class="badge vscode" title="${escapeHtml(hit.protection ?? "")}">VS Code chat</span>`
          : hit.protection
            ? `<span class="badge protected" title="${escapeHtml(hit.protection)}">Protected</span>`
            : "",
        hit.done ? '<span class="badge">Done</span>' : ""
      ].join("");
      const snippet = hit.snippet
        ? `<div class="item-snippet"><span class="role">${hit.snippet.role === "user" ? "You" : "Copilot"}:</span> ${highlight(hit.snippet.text, terms)}</div>`
        : "";
      const classes = [
        "session-item",
        hit.id === state.selectedId ? "selected" : "",
        hit.done ? "done" : "",
        hit.inUse ? "in-use" : ""
      ].join(" ");
      return `<li class="${classes}" role="option" data-id="${hit.id}" aria-selected="${hit.id === state.selectedId}" title="${escapeHtml(hit.title)}">
        <span class="bullet"></span>
        <div>
          <div class="item-title">${highlight(hit.title, terms)}</div>
          ${snippet}
          <div class="item-meta">${meta.join("<span>·</span>")}${badges}</div>
        </div>
      </li>`;
    })
    .join("");

  el.listEmpty.hidden = hits.length > 0;
  el.listEmpty.textContent = terms.length || state.project ? "No sessions match your filter." : "No sessions found.";
  if (!state.selectedId) {
    showNoSelection();
  }
}

function showNoSelection() {
  state.selectedSession = null;
  state.chatSessionId = null;
  el.chatHeader.hidden = true;
  el.chat.innerHTML = '<div class="empty">Select a session to view the conversation.</div>';
  updateFooter();
}

function updateFooter() {
  const session = state.selectedSession;
  el.resumeCommand.textContent = session ? resumeCommand(session) || "No command available" : "No session selected";
  el.resumeCommand.title = resumeHint(session);
  el.copyResume.disabled = !session?.resumeCommand;
  el.delete.disabled = !session || session.inUse || Boolean(session.protection);
  el.delete.title = session?.protection
    ? `Protected: ${session.protection}`
    : session?.inUse
      ? "This session is currently in use"
      : "Delete session";
  el.setDone.classList.toggle("active", Boolean(session?.done));
  el.setDone.title = session?.protection
    ? `Protected: ${session.protection}`
    : session?.done
      ? "Done (set in VS Code)"
      : "Set as done (only available in VS Code for now)";
}

async function runSearch({ keepSelection = true, rehighlight = true } = {}) {
  const seq = ++state.searchSeq;
  try {
    const hits = await invoke("search_sessions", { query: el.search.value });
    if (seq !== state.searchSeq) {
      return;
    }
    state.hits = hits;
    const previous = state.selectedId;
    if (!keepSelection) {
      state.selectedId = null;
    }
    renderList();
    if (rehighlight && state.selectedId && state.selectedId === previous) {
      highlightChat();
    }
  } catch (error) {
    showToast(`Search failed: ${error}`, true);
  }
}

function scheduleSearch() {
  el.clearSearch.hidden = !el.search.value;
  clearTimeout(state.searchTimer);
  state.searchTimer = setTimeout(() => runSearch(), 90);
}

async function selectSession(id, { focusList = false, silent = false } = {}) {
  if (!id) {
    return;
  }
  state.selectedId = id;
  el.list.querySelectorAll(".session-item").forEach((item) => {
    const selected = item.dataset.id === id;
    item.classList.toggle("selected", selected);
    item.setAttribute("aria-selected", String(selected));
    if (selected && !silent) {
      item.scrollIntoView({ block: "nearest" });
    }
  });
  if (focusList) {
    el.list.focus();
  }

  const seq = ++state.detailSeq;
  try {
    const detail = await invoke("get_session", { id });
    if (seq !== state.detailSeq) {
      return;
    }
    state.selectedSession = detail.session;
    if (silent && state.chatSessionId === id && state.chatMessageCount === detail.messages.length) {
      renderChatHeader(detail);
    } else {
      renderChat(detail, { preserveScroll: silent && state.chatSessionId === id });
    }
    updateFooter();
  } catch (error) {
    if (!silent) {
      showToast(`Unable to open session: ${error}`, true);
    }
  }
}

function renderChatHeader(detail) {
  const { session, messages } = detail;
  el.chatHeader.hidden = false;
  el.chatTitle.textContent = session.title;
  const meta = [session.repository || session.cwd, session.branch, formatDate(session.createdAt), `${messages.length} messages`]
    .filter(Boolean)
    .map(escapeHtml);
  el.chatMeta.innerHTML = meta.join(" · ");
  el.chatMeta.title = `${session.id}${session.cwd ? `\n${session.cwd}` : ""}`;
}

// With preserveScroll the reader keeps their position, or stays pinned to the newest message.
function renderChat(detail, { preserveScroll = false } = {}) {
  const { session, messages } = detail;
  const previousTop = el.chat.scrollTop;
  const wasAtBottom = el.chat.scrollHeight - previousTop - el.chat.clientHeight < 40;
  renderChatHeader(detail);
  state.chatSessionId = session.id;
  state.chatMessageCount = messages.length;
  const fragment = document.createDocumentFragment();
  messages.forEach((message, index) => {
    const article = document.createElement("article");
    article.className = `message ${message.role}`;
    article.dataset.index = String(index);
    const role = message.role === "user" ? "You" : "Copilot";
    article.innerHTML = `
      <div class="message-header">
        <span class="role">${icon(message.role === "user" ? "user" : "copilot")}${role}</span>
        <span title="${escapeHtml(formatDate(message.timestamp))}">${escapeHtml(relativeTime(message.timestamp))}</span>
      </div>
      <button class="icon-button copy-message" type="button" title="Copy message">${icon("copy")}</button>
      <div class="message-body markdown">${renderMarkdown(message.content)}</div>`;
    article.querySelector(".copy-message").addEventListener("click", (event) => {
      copyText(message.content, event.currentTarget);
    });
    fragment.appendChild(article);
  });
  el.chat.replaceChildren(fragment);
  if (preserveScroll) {
    highlightChat({ scroll: false });
    el.chat.scrollTop = wasAtBottom ? el.chat.scrollHeight : previousTop;
  } else {
    el.chat.scrollTop = 0;
    highlightChat();
  }
}

function highlightChat({ scroll = true } = {}) {
  el.chat.querySelectorAll("mark").forEach((mark) => {
    mark.replaceWith(document.createTextNode(mark.textContent));
  });
  el.chat.querySelectorAll(".message-body").forEach((body) => body.normalize());
  const regex = termsRegExp(searchTerms());
  if (!regex) {
    return;
  }
  const tester = new RegExp(regex.source, "i");

  const walker = document.createTreeWalker(el.chat, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      return node.parentElement?.closest(".message-body") && tester.test(node.nodeValue)
        ? NodeFilter.FILTER_ACCEPT
        : NodeFilter.FILTER_REJECT;
    }
  });
  const nodes = [];
  while (walker.nextNode()) {
    nodes.push(walker.currentNode);
  }
  nodes.forEach((node) => {
    regex.lastIndex = 0;
    const parts = node.nodeValue.split(regex);
    const fragment = document.createDocumentFragment();
    parts.forEach((part, index) => {
      if (!part) {
        return;
      }
      if (index % 2) {
        const mark = document.createElement("mark");
        mark.textContent = part;
        fragment.appendChild(mark);
      } else {
        fragment.appendChild(document.createTextNode(part));
      }
    });
    node.replaceWith(fragment);
  });
  if (scroll) {
    el.chat.querySelector("mark")?.scrollIntoView({ block: "center" });
  }
}

function inlineMarkdown(text) {
  return text
    .split(/(`+[^`]*?`+)/g)
    .map((part, index) => {
      if (index % 2) {
        return `<code>${escapeHtml(part.replace(/^`+|`+$/g, ""))}</code>`;
      }
      return escapeHtml(part)
        .replace(/\[([^\]]+)\]\(([^)\s]+)(?:\s+&quot;[^&]*&quot;)?\)/g, '<span class="link" title="$2">$1</span>')
        .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
        .replace(/__([^_]+)__/g, "<strong>$1</strong>")
        .replace(/(^|[^*])\*([^*\s][^*]*?)\*(?!\*)/g, "$1<em>$2</em>")
        .replace(/~~([^~]+)~~/g, "<del>$1</del>");
    })
    .join("");
}

function splitTableRow(line) {
  return line
    .trim()
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((cell) => cell.trim());
}

function renderMarkdown(source) {
  const lines = String(source).replace(/\r\n?/g, "\n").split("\n");
  const html = [];
  let paragraph = [];
  let listType = null;

  const flushParagraph = () => {
    if (paragraph.length) {
      html.push(`<p>${paragraph.map(inlineMarkdown).join("<br>")}</p>`);
      paragraph = [];
    }
  };
  const closeList = () => {
    if (listType) {
      html.push(`</${listType}>`);
      listType = null;
    }
  };
  const flushAll = () => {
    flushParagraph();
    closeList();
  };

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const fence = line.match(/^\s*(`{3,}|~{3,})/);
    if (fence) {
      flushAll();
      const code = [];
      i++;
      while (i < lines.length && !lines[i].trim().startsWith(fence[1])) {
        code.push(lines[i]);
        i++;
      }
      html.push(`<pre><code>${escapeHtml(code.join("\n"))}</code></pre>`);
      continue;
    }

    if (!line.trim()) {
      flushAll();
      continue;
    }

    const heading = line.match(/^\s{0,3}(#{1,6})\s+(.+?)\s*#*\s*$/);
    if (heading) {
      flushAll();
      const level = heading[1].length;
      html.push(`<h${level}>${inlineMarkdown(heading[2])}</h${level}>`);
      continue;
    }

    if (/^\s{0,3}([-*_])(\s*\1){2,}\s*$/.test(line)) {
      flushAll();
      html.push("<hr>");
      continue;
    }

    if (line.includes("|") && i + 1 < lines.length && /^\s*\|?\s*:?-{3,}:?\s*(\|\s*:?-{3,}:?\s*)*\|?\s*$/.test(lines[i + 1])) {
      flushAll();
      const header = splitTableRow(line);
      const rows = [];
      i += 2;
      while (i < lines.length && lines[i].includes("|") && lines[i].trim()) {
        rows.push(splitTableRow(lines[i]));
        i++;
      }
      i--;
      html.push(
        `<table><thead><tr>${header.map((cell) => `<th>${inlineMarkdown(cell)}</th>`).join("")}</tr></thead><tbody>${rows
          .map((row) => `<tr>${row.map((cell) => `<td>${inlineMarkdown(cell)}</td>`).join("")}</tr>`)
          .join("")}</tbody></table>`
      );
      continue;
    }

    const quote = line.match(/^\s{0,3}>\s?(.*)$/);
    if (quote) {
      flushAll();
      const quoted = [quote[1]];
      while (i + 1 < lines.length && /^\s{0,3}>/.test(lines[i + 1])) {
        quoted.push(lines[++i].replace(/^\s{0,3}>\s?/, ""));
      }
      html.push(`<blockquote>${renderMarkdown(quoted.join("\n"))}</blockquote>`);
      continue;
    }

    const item = line.match(/^(\s*)(?:([-*+])|(\d+)[.)])\s+(.*)$/);
    if (item) {
      flushParagraph();
      const type = item[2] ? "ul" : "ol";
      if (listType !== type) {
        closeList();
        html.push(`<${type}>`);
        listType = type;
      }
      const indent = Math.min(4, Math.floor(item[1].replace(/\t/g, "  ").length / 2));
      const style = indent ? ` style="margin-left:${indent * 18}px"` : "";
      html.push(`<li${style}>${inlineMarkdown(item[4])}</li>`);
      continue;
    }

    if (listType && /^\s{2,}\S/.test(line)) {
      html.push(`<li style="list-style:none">${inlineMarkdown(line.trim())}</li>`);
      continue;
    }

    closeList();
    paragraph.push(line.trim());
  }
  flushAll();
  return html.join("");
}

async function copyText(text, button) {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
    } else {
      const area = document.createElement("textarea");
      area.value = text;
      area.style.position = "fixed";
      area.style.opacity = "0";
      document.body.appendChild(area);
      area.select();
      const copied = document.execCommand("copy");
      area.remove();
      if (!copied) {
        throw new Error("Clipboard is unavailable");
      }
    }
    if (button) {
      button.innerHTML = icon("check");
      button.classList.add("copied");
      setTimeout(() => {
        button.innerHTML = icon("copy");
        button.classList.remove("copied");
      }, 1200);
    }
  } catch (error) {
    showToast(`Copy failed: ${error}`, true);
  }
}

function showToast(message, isError = false) {
  clearTimeout(state.toastTimer);
  el.toast.textContent = message;
  el.toast.classList.toggle("error", isError);
  el.toast.hidden = false;
  state.toastTimer = setTimeout(() => {
    el.toast.hidden = true;
  }, isError ? 5000 : 2200);
}

// Silent refreshes come from the backend watcher and must not disturb the user.
async function refresh({ silent = false } = {}) {
  if (state.refreshing) {
    state.refreshPending = true;
    return;
  }
  state.refreshing = true;
  el.refresh.disabled = !silent;
  if (!state.hits.length && !silent) {
    el.listEmpty.hidden = false;
    el.listEmpty.textContent = "Indexing sessions…";
  }
  try {
    await invoke("refresh_sessions");
    await runSearch({ rehighlight: !silent });
    if (state.selectedId) {
      await selectSession(state.selectedId, { silent });
    }
  } catch (error) {
    if (silent) {
      console.warn("Automatic refresh failed", error);
    } else {
      el.listEmpty.hidden = false;
      el.listEmpty.textContent = "Unable to load sessions.";
      showToast(`Refresh failed: ${error}`, true);
    }
  } finally {
    state.refreshing = false;
    el.refresh.disabled = false;
    if (state.refreshPending) {
      state.refreshPending = false;
      refresh({ silent: true });
    }
  }
}

function openDeleteDialog() {
  const session = state.selectedSession;
  if (!session || session.inUse) {
    return;
  }
  if (session.protection) {
    showToast(`Protected: ${session.protection}`);
    return;
  }
  el.dialogText.innerHTML = `<strong>${escapeHtml(session.title)}</strong><br><code>${escapeHtml(session.id)}</code>`;
  el.dialog.hidden = false;
  el.dialogCancel.focus();
}

function closeDeleteDialog() {
  el.dialog.hidden = true;
}

async function openMergeDialog() {
  if (!isMergeable(state.project)) {
    return;
  }
  let projects;
  try {
    projects = await invoke("list_projects");
  } catch (error) {
    showToast(`Unable to load projects: ${error}`, true);
    return;
  }
  const source = projects.find((project) => project.key === state.project);
  if (!source) {
    showToast("This project has no sessions any more.");
    return;
  }
  el.mergeText.innerHTML = [
    `Group the sessions of <strong>${escapeHtml(source.name)}</strong>`,
    source.detail ? ` <span class="merged-detail">(${escapeHtml(source.detail)})</span>` : "",
    " under another project, for example after the repository was renamed or moved.",
    source.missing ? "<br>Its folder no longer exists." : ""
  ].join("");

  // Projects with an existing folder first, most recently used first: the likely new name.
  const targets = projects
    .filter((project) => project.key !== source.key && isMergeable(project.key))
    .sort((a, b) => Number(a.missing) - Number(b.missing) || (b.updatedAt || "").localeCompare(a.updatedAt || ""));
  el.mergeTarget.innerHTML = [
    '<option value="">Choose a project…</option>',
    ...targets.map((project) => {
      const notes = [`${project.sessions}`, project.missing ? "folder missing" : "", relativeTime(project.updatedAt)].filter(Boolean);
      return `<option value="${escapeHtml(project.key)}" title="${escapeHtml(project.detail)}">${escapeHtml(project.name)} (${escapeHtml(notes.join(" · "))})</option>`;
    })
  ].join("");
  el.mergeTarget.disabled = !targets.length;
  el.mergeConfirm.disabled = true;

  el.mergedSection.hidden = !source.merged.length;
  el.mergedList.innerHTML = source.merged
    .map(
      (project) => `<li>
        <span class="merged-name" title="${escapeHtml(project.detail)}">${escapeHtml(project.name)}
          <span class="merged-detail">${escapeHtml(project.detail)}</span></span>
        <button class="button secondary" type="button" data-key="${escapeHtml(project.key)}">Unmerge</button>
      </li>`
    )
    .join("");

  el.mergeDialog.hidden = false;
  el.mergeTarget.focus();
}

function closeMergeDialog() {
  el.mergeDialog.hidden = true;
}

async function mergeSelectedProject() {
  const into = el.mergeTarget.value;
  if (!into || !state.project) {
    return;
  }
  el.mergeConfirm.disabled = true;
  try {
    const key = await invoke("merge_project", { from: state.project, into });
    setProject(key);
    closeMergeDialog();
    await runSearch();
    showToast("Projects merged");
  } catch (error) {
    showToast(`Merge failed: ${error}`, true);
    el.mergeConfirm.disabled = false;
  }
}

async function unmergeProject(key, button) {
  button.disabled = true;
  try {
    await invoke("unmerge_project", { key });
    await runSearch();
    showToast("Project unmerged");
    await openMergeDialog();
  } catch (error) {
    showToast(`Unmerge failed: ${error}`, true);
    button.disabled = false;
  }
}

async function deleteSelected() {
  const session = state.selectedSession;
  if (!session) {
    return;
  }
  el.dialogConfirm.disabled = true;
  try {
    const hits = visibleHits();
    const index = hits.findIndex((hit) => hit.id === session.id);
    const next = hits[index + 1] || hits[index - 1];
    await invoke("delete_session", { id: session.id });
    closeDeleteDialog();
    state.selectedId = next?.id || null;
    await runSearch();
    if (state.selectedId) {
      await selectSession(state.selectedId);
    }
    showToast("Session deleted");
  } catch (error) {
    showToast(`Delete failed: ${error}`, true);
  } finally {
    el.dialogConfirm.disabled = false;
  }
}

function moveSelection(delta) {
  const hits = visibleHits();
  if (!hits.length) {
    return;
  }
  const current = hits.findIndex((hit) => hit.id === state.selectedId);
  const next = current < 0 ? 0 : Math.max(0, Math.min(hits.length - 1, current + delta));
  selectSession(hits[next].id);
}

function setupSplitter() {
  const saved = Number(localStorage.getItem("listWidth"));
  if (saved) {
    el.columns.style.setProperty("--list-width", `${saved}px`);
  }
  el.splitter.addEventListener("pointerdown", (event) => {
    el.splitter.setPointerCapture(event.pointerId);
    el.splitter.classList.add("dragging");
  });
  el.splitter.addEventListener("pointermove", (event) => {
    if (!el.splitter.hasPointerCapture(event.pointerId)) {
      return;
    }
    const bounds = el.columns.getBoundingClientRect();
    const width = Math.round(Math.max(200, Math.min(bounds.width - 300, event.clientX - bounds.left)));
    el.columns.style.setProperty("--list-width", `${width}px`);
    localStorage.setItem("listWidth", String(width));
  });
  el.splitter.addEventListener("pointerup", () => el.splitter.classList.remove("dragging"));
}

function bindEvents() {
  el.search.addEventListener("input", scheduleSearch);
  el.search.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && el.search.value) {
      event.preventDefault();
      el.search.value = "";
      scheduleSearch();
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      moveSelection(event.key === "ArrowDown" ? 1 : -1);
    } else if (event.key === "Enter") {
      moveSelection(0);
    }
  });
  el.clearSearch.addEventListener("click", () => {
    el.search.value = "";
    scheduleSearch();
    el.search.focus();
  });
  el.list.addEventListener("click", (event) => {
    const item = event.target.closest(".session-item");
    if (item) {
      selectSession(item.dataset.id, { focusList: true });
    }
  });
  el.list.addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      moveSelection(event.key === "ArrowDown" ? 1 : -1);
    } else if (event.key === "Delete") {
      openDeleteDialog();
    }
  });
  el.refresh.addEventListener("click", () => refresh());
  el.projectFilter.addEventListener("change", () => {
    setProject(el.projectFilter.value);
    renderList();
  });
  el.toggleDone.addEventListener("click", () => {
    state.showDone = !state.showDone;
    localStorage.setItem("showDone", String(state.showDone));
    el.toggleDone.setAttribute("aria-pressed", String(state.showDone));
    el.toggleDone.title = state.showDone ? "Hide sessions marked as done" : "Show sessions marked as done";
    renderList();
  });
  el.copyResume.addEventListener("click", (event) => {
    if (state.selectedSession?.resumeCommand) {
      copyText(resumeCommand(state.selectedSession), event.currentTarget);
    }
  });
  el.delete.addEventListener("click", openDeleteDialog);
  el.mergeButton.addEventListener("click", openMergeDialog);
  el.mergeCancel.addEventListener("click", closeMergeDialog);
  el.mergeConfirm.addEventListener("click", mergeSelectedProject);
  el.mergeTarget.addEventListener("change", () => {
    el.mergeConfirm.disabled = !el.mergeTarget.value;
  });
  el.mergedList.addEventListener("click", (event) => {
    const button = event.target.closest("button[data-key]");
    if (button) {
      unmergeProject(button.dataset.key, button);
    }
  });
  el.mergeDialog.addEventListener("click", (event) => {
    if (event.target === el.mergeDialog) {
      closeMergeDialog();
    }
  });
  el.dialogCancel.addEventListener("click", closeDeleteDialog);
  el.dialogConfirm.addEventListener("click", deleteSelected);
  el.dialog.addEventListener("click", (event) => {
    if (event.target === el.dialog) {
      closeDeleteDialog();
    }
  });
  document.addEventListener("keydown", (event) => {
    if (!el.dialog.hidden && event.key === "Escape") {
      closeDeleteDialog();
    } else if (!el.mergeDialog.hidden && event.key === "Escape") {
      closeMergeDialog();
    } else if ((event.ctrlKey || event.metaKey) && ["f", "k"].includes(event.key.toLowerCase())) {
      event.preventDefault();
      el.search.focus();
      el.search.select();
    } else if (event.key === "F5") {
      event.preventDefault();
      refresh();
    }
  });
}

function init() {
  hydrateIcons();
  el.toggleDone.setAttribute("aria-pressed", String(state.showDone));
  el.toggleDone.title = state.showDone ? "Hide sessions marked as done" : "Show sessions marked as done";
  setupSplitter();
  bindEvents();
  if (!invoke) {
    el.listEmpty.hidden = false;
    el.listEmpty.textContent = "Run this page inside the Tauri app.";
    return;
  }
  refresh();
  window.__TAURI__.event?.listen("sessions-changed", () => refresh({ silent: true }));
  el.search.focus();
}

init();
