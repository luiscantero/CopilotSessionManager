"use strict";

const invoke = window.__TAURI__?.core?.invoke;
// Extra session actions offered by the host (the VS Code extension), called through invoke.
const HOST_ACTIONS = new Set(window.__TAURI__?.hostActions || []);

const ICONS = {
  chat: '<svg viewBox="0 0 16 16"><path d="M2.5 3.5h11v7.5h-6.5l-3 2.5V11h-1.5z"/></svg>',
  check: '<svg viewBox="0 0 16 16"><path d="M3 8.5l3 3 7-7"/></svg>',
  checkAll: '<svg viewBox="0 0 16 16"><path d="M1.5 8.5l3 3 7-7M7.4 11.1l.9.9 6.2-6.2"/></svg>',
  close: '<svg viewBox="0 0 16 16"><path d="M4 4l8 8M12 4l-8 8"/></svg>',
  copilot: '<svg viewBox="0 0 16 16"><path d="M4 6.2C4 4.4 5.4 3 7.2 3h1.6C10.6 3 12 4.4 12 6.2v3.6c0 1.8-1.4 3.2-3.2 3.2H7.2C5.4 13 4 11.6 4 9.8z"/><path d="M2 8h2M12 8h2M6.5 7.5v1M9.5 7.5v1"/></svg>',
  merge: '<svg viewBox="0 0 16 16"><path d="M4.5 2v3c0 2.5 3.5 3 3.5 5.5V14M11.5 2v3c0 2.5-3.5 3-3.5 5.5M6 12.5l2 2 2-2"/></svg>',
  play: '<svg viewBox="0 0 16 16"><path d="M5 3.5v9l7-4.5z"/></svg>',
  copy: '<svg viewBox="0 0 16 16"><path d="M3.5 10.5V3.2c0-.4.3-.7.7-.7h6.3"/><rect x="5.5" y="4.5" width="7" height="9" rx=".8"/></svg>',
  gear: '<svg viewBox="0 0 16 16"><path d="M12.83 6.71L14.51 6.91L14.51 9.09L12.83 9.29L12.33 10.5L13.37 11.83L11.83 13.37L10.5 12.33L9.29 12.83L9.09 14.51L6.91 14.51L6.71 12.83L5.5 12.33L4.17 13.37L2.63 11.83L3.67 10.5L3.17 9.29L1.49 9.09L1.49 6.91L3.17 6.71L3.67 5.5L2.63 4.17L4.17 2.63L5.5 3.67L6.71 3.17L6.91 1.49L9.09 1.49L9.29 3.17L10.5 3.67L11.83 2.63L13.37 4.17L12.33 5.5z"/><circle cx="8" cy="8" r="2.1"/></svg>',
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
  dialogWarning: document.getElementById("dialog-warning"),
  list: document.getElementById("session-list"),
  listEmpty: document.getElementById("list-empty"),
  mergeButton: document.getElementById("merge-project"),
  mergeCancel: document.getElementById("merge-cancel"),
  mergeConfirm: document.getElementById("merge-confirm"),
  mergeDialog: document.getElementById("merge-backdrop"),
  mergedList: document.getElementById("merged-list"),
  mergedSection: document.getElementById("merged-section"),
  mergeMove: document.getElementById("merge-move"),
  mergeReport: document.getElementById("merge-report"),
  mergeReportList: document.getElementById("merge-report-list"),
  mergeReportText: document.getElementById("merge-report-text"),
  mergeTarget: document.getElementById("merge-target"),
  mergeText: document.getElementById("merge-text"),
  mergeWarning: document.getElementById("merge-warning"),
  openChat: document.getElementById("open-chat"),
  projectFilter: document.getElementById("project-filter"),
  refresh: document.getElementById("refresh"),
  languageSelect: document.getElementById("language-select"),
  resumeCommand: document.getElementById("resume-command"),
  resumeSession: document.getElementById("resume-session"),
  search: document.getElementById("search"),
  settings: document.getElementById("settings"),
  settingsClose: document.getElementById("settings-close"),
  settingsDialog: document.getElementById("settings-backdrop"),
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
  loaded: null,
  workspaceProjectChecked: false,
  chatSessionId: null,
  chatMessageCount: 0,
  chatDetail: null
};

const DEFAULT_LANGUAGE = "en-us";
const I18N_ATTRIBUTES = {
  "data-i18n-aria-label": "aria-label",
  "data-i18n-placeholder": "placeholder",
  "data-i18n-title": "title"
};

const i18n = {
  languages: [],
  code: DEFAULT_LANGUAGE,
  dir: "ltr",
  strings: {},
  fallback: {},
  relativeTime: new Intl.RelativeTimeFormat(DEFAULT_LANGUAGE, { numeric: "always", style: "short" })
};

async function fetchJson(path) {
  const response = await fetch(path);
  if (!response.ok) {
    throw new Error(`${path}: ${response.status} ${response.statusText}`);
  }
  return response.json();
}

function hasTranslation(key) {
  return key in i18n.strings || key in i18n.fallback;
}

function translationTemplate(key) {
  return i18n.strings[key] ?? i18n.fallback[key] ?? key;
}

function fillTemplate(template, params) {
  return template.replace(/\{(\w+)\}/g, (match, name) => (name in params ? String(params[name]) : match));
}

function t(key, params = {}) {
  return fillTemplate(translationTemplate(key), params);
}

// Escapes the translation itself; params are inserted as-is, so they must already be safe HTML.
function tHtml(key, params = {}) {
  return fillTemplate(escapeHtml(translationTemplate(key)), params);
}

// Saved choice first, then the first OS/browser language we have (exact match, then same base language).
function detectLanguage() {
  const codes = i18n.languages.map((language) => language.code);
  const saved = localStorage.getItem("language");
  if (codes.includes(saved)) {
    return saved;
  }
  const preferred = (navigator.languages?.length ? navigator.languages : [navigator.language]).filter(Boolean);
  for (const tag of preferred.map((tag) => tag.toLowerCase())) {
    const base = tag.split("-")[0];
    const match = codes.find((code) => code === tag) || codes.find((code) => code.split("-")[0] === base);
    if (match) {
      return match;
    }
  }
  return DEFAULT_LANGUAGE;
}

async function setLanguage(code) {
  const language = i18n.languages.find((entry) => entry.code === code) || { code: DEFAULT_LANGUAGE };
  const strings = language.code === DEFAULT_LANGUAGE ? i18n.fallback : await fetchJson(`locales/${language.code}.json`);
  i18n.code = language.code;
  i18n.dir = language.dir === "rtl" ? "rtl" : "ltr";
  i18n.strings = strings;
  i18n.relativeTime = new Intl.RelativeTimeFormat(language.code, { numeric: "always", style: "short" });
  document.documentElement.lang = language.code;
  document.documentElement.dir = i18n.dir;
  applyTranslations();
}

async function initI18n() {
  try {
    [i18n.languages, i18n.fallback] = await Promise.all([
      fetchJson("locales/languages.json"),
      fetchJson(`locales/${DEFAULT_LANGUAGE}.json`)
    ]);
  } catch (error) {
    console.warn("Unable to load translations", error);
    return;
  }
  try {
    await setLanguage(detectLanguage());
  } catch (error) {
    console.warn("Unable to load the language, using the default", error);
    await setLanguage(DEFAULT_LANGUAGE);
  }
}

function applyTranslations() {
  document.querySelectorAll("[data-i18n]").forEach((node) => {
    if (hasTranslation(node.dataset.i18n)) {
      node.textContent = t(node.dataset.i18n);
    }
  });
  for (const [attribute, target] of Object.entries(I18N_ATTRIBUTES)) {
    document.querySelectorAll(`[${attribute}]`).forEach((node) => {
      const key = node.getAttribute(attribute);
      if (hasTranslation(key)) {
        node.setAttribute(target, t(key));
      }
    });
  }
}

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
    [60, "minute", 60],
    [3600, "hour", 24],
    [86400, "day", 7],
    [604800, "week", 5],
    [2629800, "month", 12],
    [31557600, "year", Infinity]
  ];
  if (seconds < 60) {
    return t("time.justNow");
  }
  for (const [size, unit, limit] of units) {
    const value = Math.floor(seconds / size);
    if (value < limit) {
      return i18n.relativeTime.format(-value, unit);
    }
  }
  return "";
}

function formatDate(iso) {
  const time = Date.parse(iso || "");
  return time
    ? new Date(time).toLocaleString(i18n.code, { dateStyle: "medium", timeStyle: "short" })
    : "";
}

function doneFilteredHits() {
  return state.showDone ? state.hits : state.hits.filter((hit) => !hit.done);
}

function visibleHits() {
  const hits = doneFilteredHits();
  return state.project ? hits.filter((hit) => hit.project.key === state.project) : hits;
}

function setProject(key, name = "") {
  state.project = key;
  const match = state.hits.find((hit) => hit.project.key === key);
  state.projectName = key ? name || match?.project.name || state.projectName : "";
  localStorage.setItem("project", state.project);
  localStorage.setItem("projectName", state.projectName);
}

// In VS Code, start filtered to the project of the open folder, if it has sessions.
async function selectWorkspaceProject() {
  const folders = window.__TAURI__?.workspaceFolders || [];
  if (!folders.length) {
    return;
  }
  try {
    const project = await invoke("project_for_folders", { folders });
    if (project) {
      setProject(project.key, project.name);
    }
  } catch (error) {
    console.warn("Unable to find the project of the workspace", error);
  }
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
    `<option value="">${escapeHtml(t("projects.all", { count: base.length }))}</option>`,
    ...projects.map(({ key, name, detail, count, missing, merged }) => {
      const label = nameCounts.get(name.toLowerCase()) > 1 && detail ? `${name} — ${detail}` : name;
      const notes = [
        missing ? t("projects.folderMissing") : "",
        merged?.length ? t("projects.mergedCount", { count: merged.length }) : ""
      ].filter(Boolean);
      const suffix = notes.length ? ` · ${notes.join(" · ")}` : "";
      return `<option value="${escapeHtml(key)}" title="${escapeHtml(projectTooltip({ detail, missing, merged }))}">${escapeHtml(label)} (${count})${escapeHtml(suffix)}</option>`;
    })
  ].join("");
  el.projectFilter.value = state.project;
  el.projectFilter.classList.toggle("active", Boolean(state.project));
  const selected = groups.get(state.project);
  el.projectFilter.title = selected ? projectTooltip(selected) || selected.name : t("projects.filterTitle");
  el.mergeButton.disabled = !isMergeable(state.project);
  el.mergeButton.title = isMergeable(state.project) ? t("projects.mergeTitle") : t("projects.mergeDisabledTitle");
}

function isMergeable(key) {
  return key.startsWith("repo:") || key.startsWith("path:");
}

function projectTooltip({ detail, missing, merged }) {
  return [
    detail,
    missing ? t("projects.folderMissingHint") : "",
    merged?.length ? t("projects.includes", { names: merged.map((project) => project.name).join(", ") }) : ""
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
    return session.resumeCommand ? t("footer.openInVsCode") : t("footer.noFolder");
  }
  return session.cwd ? t("footer.runFrom", { folder: session.cwd }) : "";
}

function renderList() {
  const terms = searchTerms();
  if (state.project && !terms.length && state.hits.length && !state.hits.some((hit) => hit.project.key === state.project)) {
    setProject("");
  }
  renderProjectOptions();
  const hits = visibleHits();
  const total = state.hits.length;
  el.count.textContent = hits.length === total ? `${total}` : t("header.countFiltered", { shown: hits.length, total });

  if (!hits.some((hit) => hit.id === state.selectedId)) {
    state.selectedId = null;
  }

  el.list.innerHTML = hits
    .map((hit) => {
      const meta = [];
      if (!state.project) {
        meta.push(`<span class="repo" dir="auto" title="${escapeHtml(hit.project.detail)}">${escapeHtml(hit.project.name)}</span>`);
      }
      if (hit.additions || hit.deletions) {
        meta.push(
          `<span dir="ltr"><span class="added">+${hit.additions ?? 0}</span> <span class="deleted">-${hit.deletions ?? 0}</span></span>`
        );
      }
      meta.push(`<span>${escapeHtml(relativeTime(hit.updatedAt))}</span>`);
      const badges = [
        hit.inUse ? `<span class="badge live">${escapeHtml(t("badge.inUse"))}</span>` : "",
        hit.source === "vscode"
          ? `<span class="badge vscode"${hit.windowOpen ? ` title="${escapeHtml(t("footer.vscodeOpen"))}"` : ""}>${escapeHtml(t("badge.vscodeChat"))}</span>`
          : hit.protection
            ? `<span class="badge protected" title="${escapeHtml(hit.protection)}">${escapeHtml(t("badge.protected"))}</span>`
            : "",
        hit.done ? `<span class="badge">${escapeHtml(t("badge.done"))}</span>` : ""
      ].join("");
      const snippet = hit.snippet
        ? `<div class="item-snippet"><span class="role">${escapeHtml(roleLabel(hit.snippet.role))}:</span> <bdi>${highlight(hit.snippet.text, terms)}</bdi></div>`
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
          <div class="item-title" dir="auto">${highlight(hit.title, terms)}</div>
          ${snippet}
          <div class="item-meta">${meta.join("<span>·</span>")}${badges}</div>
        </div>
      </li>`;
    })
    .join("");

  el.listEmpty.hidden = hits.length > 0;
  el.listEmpty.textContent = terms.length || state.project ? t("list.noMatches") : t("list.empty");
  if (!state.selectedId) {
    showNoSelection();
  }
}

function roleLabel(role) {
  return role === "user" ? t("role.user") : t("role.assistant");
}

function showNoSelection() {
  state.selectedSession = null;
  state.chatSessionId = null;
  state.chatDetail = null;
  el.chatHeader.hidden = true;
  el.chat.innerHTML = `<div class="empty">${escapeHtml(t("chat.selectSession"))}</div>`;
  updateFooter();
}

function updateFooter() {
  const session = state.selectedSession;
  el.resumeCommand.textContent = session ? resumeCommand(session) || t("footer.noCommand") : t("footer.noSession");
  el.resumeCommand.title = resumeHint(session);
  el.copyResume.disabled = !session?.resumeCommand;
  el.resumeSession.hidden = !HOST_ACTIONS.has("resume_session") || session?.source === "vscode";
  el.resumeSession.disabled = !session;
  el.openChat.hidden = !HOST_ACTIONS.has("open_in_chat");
  el.openChat.disabled = !session;
  el.delete.disabled = !session || session.inUse || session.windowOpen || Boolean(session.protection);
  el.delete.title = session?.protection
    ? t("footer.protected", { reason: session.protection })
    : session?.inUse
      ? t("footer.inUse")
      : session?.windowOpen
        ? t("footer.vscodeOpen")
        : t("footer.delete");
  el.setDone.classList.toggle("active", Boolean(session?.done));
  el.setDone.title = session?.protection
    ? t("footer.protected", { reason: session.protection })
    : session?.done
      ? t("footer.doneInVsCode")
      : t("footer.setDoneHint");
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
    showToast(t("toast.searchFailed", { error }), true);
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
      showToast(t("toast.openFailed", { error }), true);
    }
  }
}

function renderChatHeader(detail) {
  const { session, messages } = detail;
  el.chatHeader.hidden = false;
  el.chatTitle.textContent = session.title;
  const meta = [
    session.repository || session.cwd,
    session.branch,
    formatDate(session.createdAt),
    t("chat.messageCount", { count: messages.length })
  ]
    .filter(Boolean)
    .map((part) => `<bdi>${escapeHtml(part)}</bdi>`);
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
  state.chatDetail = detail;
  const fragment = document.createDocumentFragment();
  messages.forEach((message, index) => {
    const article = document.createElement("article");
    article.className = `message ${message.role}`;
    article.dataset.index = String(index);
    article.innerHTML = `
      <div class="message-header">
        <span class="role">${icon(message.role === "user" ? "user" : "copilot")}${escapeHtml(roleLabel(message.role))}</span>
        <span title="${escapeHtml(formatDate(message.timestamp))}">${escapeHtml(relativeTime(message.timestamp))}</span>
      </div>
      <button class="icon-button copy-message" type="button" title="${escapeHtml(t("chat.copyMessage"))}">${icon("copy")}</button>
      <div class="message-body markdown" dir="auto">${renderMarkdown(message.content)}</div>`;
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
      const style = indent ? ` style="margin-inline-start:${indent * 18}px"` : "";
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
        throw new Error(t("toast.clipboardUnavailable"));
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
    showToast(t("toast.copyFailed", { error }), true);
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
    el.listEmpty.textContent = t("list.indexing");
  }
  try {
    const folders = window.__TAURI__?.workspaceFolders || [];
    await invoke("refresh_sessions", folders.length ? { folders } : {});
    if (!state.workspaceProjectChecked) {
      state.workspaceProjectChecked = true;
      await selectWorkspaceProject();
    }
    await runSearch({ rehighlight: !silent });
    if (state.selectedId) {
      await selectSession(state.selectedId, { silent });
    }
  } catch (error) {
    if (silent) {
      console.warn("Automatic refresh failed", error);
    } else {
      el.listEmpty.hidden = false;
      el.listEmpty.textContent = t("list.loadFailed");
      showToast(t("toast.refreshFailed", { error }), true);
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
    showToast(t("footer.protected", { reason: session.protection }));
    return;
  }
  if (session.windowOpen) {
    showToast(t("footer.vscodeOpen"));
    return;
  }
  el.dialogText.innerHTML = `<strong>${escapeHtml(session.title)}</strong><br><code>${escapeHtml(session.id)}</code>`;
  const vscodeChat = session.source === "vscode";
  el.dialogWarning.dataset.i18n = vscodeChat ? "delete.warningVsCode" : "delete.warning";
  el.dialogWarning.textContent = vscodeChat ? t("delete.warningVsCode") : t("delete.warning");
  el.dialog.hidden = false;
  el.dialogCancel.focus();
}

function closeDeleteDialog() {
  el.dialog.hidden = true;
}

async function openMergeDialog(report = null) {
  if (!isMergeable(state.project)) {
    return;
  }
  let projects;
  try {
    projects = await invoke("list_projects");
  } catch (error) {
    showToast(t("toast.projectsFailed", { error }), true);
    return;
  }
  const source = projects.find((project) => project.key === state.project);
  if (!source) {
    showToast(t("toast.projectGone"));
    return;
  }
  const name = [
    `<strong>${escapeHtml(source.name)}</strong>`,
    source.detail ? ` <span class="merged-detail">(${escapeHtml(source.detail)})</span>` : ""
  ].join("");
  el.mergeText.innerHTML = [
    tHtml("merge.text", { name }),
    source.missing ? `<br>${tHtml("merge.folderGone")}` : ""
  ].join("");

  const targets = projects
    .filter((project) => project.key !== source.key && isMergeable(project.key))
    .sort(
      (a, b) =>
        a.name.localeCompare(b.name, undefined, { sensitivity: "base" }) || (a.detail || "").localeCompare(b.detail || "")
    );
  el.mergeTarget.innerHTML = [
    `<option value="">${escapeHtml(t("merge.choose"))}</option>`,
    ...targets.map((project) => {
      const notes = [`${project.sessions}`, project.missing ? t("projects.folderMissing") : "", relativeTime(project.updatedAt)].filter(Boolean);
      return `<option value="${escapeHtml(project.key)}" title="${escapeHtml(project.detail)}">${escapeHtml(project.name)} (${escapeHtml(notes.join(" · "))})</option>`;
    })
  ].join("");
  el.mergeTarget.disabled = !targets.length;
  el.mergeConfirm.disabled = true;
  el.mergeMove.checked = false;
  updateMergeWarning();

  el.mergedSection.hidden = !source.merged.length;
  el.mergedList.innerHTML = source.merged
    .map((project) => {
      const status = [
        project.moved ? t("merge.movedCount", { count: project.moved }) : "",
        project.moved && project.notMoved ? t("merge.notMovedCount", { count: project.notMoved }) : ""
      ].filter(Boolean);
      const key = escapeHtml(project.key);
      const unmergeTitle = project.moved ? ` title="${escapeHtml(t("merge.unmergeMovedTitle"))}"` : "";
      return `<li>
        <span class="merged-name" title="${escapeHtml(project.detail)}">
          <span class="merged-title">${escapeHtml(project.name)} <span class="merged-detail">${escapeHtml(project.detail)}</span></span>${
            status.length ? `<span class="merged-status">${escapeHtml(status.join(" · "))}</span>` : ""
          }</span>
        ${
          project.notMoved
            ? `<button class="button secondary" type="button" data-action="move" data-key="${key}" title="${escapeHtml(t("merge.moveTitle"))}">${escapeHtml(t("merge.move"))}</button>`
            : ""
        }
        <button class="button secondary" type="button" data-action="unmerge" data-key="${key}"${unmergeTitle}>${escapeHtml(t("merge.unmerge"))}</button>
      </li>`;
    })
    .join("");
  renderMergeReport(report);

  el.mergeDialog.hidden = false;
  el.mergeTarget.focus();
}

function updateMergeWarning() {
  const move = el.mergeMove.checked;
  el.mergeWarning.dataset.i18n = move ? "merge.warningMove" : "merge.warning";
  el.mergeWarning.textContent = move ? t("merge.warningMove") : t("merge.warning");
}

// Shows the result of moving sessions ({ move }) or of unmerging ({ unmerge }) in the merge dialog.
function renderMergeReport(report) {
  const lines = [];
  let skipped = [];
  if (report?.move) {
    const { moved, folder, skipped: notMoved } = report.move;
    if (moved) {
      lines.push(t("merge.reportMoved", { count: moved, folder: folder || "" }));
    }
    if (notMoved.length) {
      lines.push(t("merge.reportSkipped", { count: notMoved.length }));
    }
    if (!moved && !notMoved.length) {
      lines.push(t("merge.reportNothing"));
    }
    skipped = notMoved;
  } else if (report?.unmerge) {
    const { restored, unmerged, skipped: notRestored } = report.unmerge;
    if (restored) {
      lines.push(t("merge.reportRestored", { count: restored }));
    }
    if (!unmerged) {
      lines.push(t("merge.stillMerged"));
    }
    if (notRestored.length) {
      lines.push(t("merge.reportNotRestored", { count: notRestored.length }));
    }
    skipped = notRestored;
  }
  el.mergeReport.hidden = !lines.length;
  el.mergeReportText.textContent = lines.join(" ");
  el.mergeReportList.innerHTML = skipped.map((reason) => `<li>${escapeHtml(reason)}</li>`).join("");
}

function closeMergeDialog() {
  el.mergeDialog.hidden = true;
}

function openSettingsDialog() {
  el.languageSelect.innerHTML = i18n.languages
    .map(
      ({ code, name, dir }) =>
        `<option value="${escapeHtml(code)}" lang="${escapeHtml(code)}" dir="${dir === "rtl" ? "rtl" : "ltr"}">${escapeHtml(name)}</option>`
    )
    .join("");
  el.languageSelect.value = i18n.code;
  el.languageSelect.disabled = !i18n.languages.length;
  el.settingsDialog.hidden = false;
  el.languageSelect.focus();
}

function closeSettingsDialog() {
  el.settingsDialog.hidden = true;
}

async function changeLanguage(code) {
  try {
    await setLanguage(code);
    localStorage.setItem("language", code);
  } catch (error) {
    el.languageSelect.value = i18n.code;
    showToast(t("toast.languageFailed", { error }), true);
    return;
  }
  updateToggleDoneTitle();
  renderList();
  updateFooter();
  if (state.chatDetail) {
    renderChat(state.chatDetail, { preserveScroll: true });
  }
}

function updateToggleDoneTitle() {
  el.toggleDone.setAttribute("aria-pressed", String(state.showDone));
  el.toggleDone.title = state.showDone ? t("header.hideDone") : t("header.showDone");
}

async function mergeSelectedProject() {
  const into = el.mergeTarget.value;
  if (!into || !state.project) {
    return;
  }
  const moveSessions = el.mergeMove.checked;
  el.mergeConfirm.disabled = true;
  try {
    const { key, report } = await invoke("merge_project", { from: state.project, into, moveSessions });
    setProject(key);
    await runSearch();
    if (report?.skipped.length) {
      await openMergeDialog({ move: report });
      return;
    }
    closeMergeDialog();
    showToast(report?.moved ? t("toast.mergedMoved", { count: report.moved }) : t("toast.merged"));
  } catch (error) {
    showToast(t("toast.mergeFailed", { error }), true);
    el.mergeConfirm.disabled = false;
  }
}

async function moveMergedProject(key, button) {
  button.disabled = true;
  try {
    const report = await invoke("move_merged_project", { key });
    await runSearch();
    await openMergeDialog({ move: report });
  } catch (error) {
    showToast(t("toast.moveFailed", { error }), true);
    button.disabled = false;
  }
}

async function unmergeProject(key, button) {
  button.disabled = true;
  try {
    const report = await invoke("unmerge_project", { key });
    await runSearch();
    if (report.unmerged && !report.skipped.length) {
      showToast(t("toast.unmerged"));
    }
    await openMergeDialog(report.restored || report.skipped.length ? { unmerge: report } : null);
  } catch (error) {
    showToast(t("toast.unmergeFailed", { error }), true);
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
    showToast(t("toast.deleted"));
  } catch (error) {
    showToast(t("toast.deleteFailed", { error }), true);
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
    // In right-to-left layouts the list pane sits on the right.
    const offset = i18n.dir === "rtl" ? bounds.right - event.clientX : event.clientX - bounds.left;
    const width = Math.round(Math.max(200, Math.min(bounds.width - 300, offset)));
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
    updateToggleDoneTitle();
    renderList();
  });
  el.copyResume.addEventListener("click", (event) => {
    if (state.selectedSession?.resumeCommand) {
      copyText(resumeCommand(state.selectedSession), event.currentTarget);
    }
  });
  el.resumeSession.addEventListener("click", () => runHostAction("resume_session"));
  el.openChat.addEventListener("click", () => runHostAction("open_in_chat"));
  el.delete.addEventListener("click", openDeleteDialog);
  el.mergeButton.addEventListener("click", openMergeDialog);
  el.mergeCancel.addEventListener("click", closeMergeDialog);
  el.mergeConfirm.addEventListener("click", mergeSelectedProject);
  el.mergeTarget.addEventListener("change", () => {
    el.mergeConfirm.disabled = !el.mergeTarget.value;
  });
  el.mergeMove.addEventListener("change", updateMergeWarning);
  el.mergedList.addEventListener("click", (event) => {
    const button = event.target.closest("button[data-key]");
    if (button?.dataset.action === "move") {
      moveMergedProject(button.dataset.key, button);
    } else if (button) {
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
  el.settings.addEventListener("click", openSettingsDialog);
  el.settingsClose.addEventListener("click", closeSettingsDialog);
  el.languageSelect.addEventListener("change", () => changeLanguage(el.languageSelect.value));
  el.settingsDialog.addEventListener("click", (event) => {
    if (event.target === el.settingsDialog) {
      closeSettingsDialog();
    }
  });
  document.addEventListener("keydown", (event) => {
    if (!el.dialog.hidden && event.key === "Escape") {
      closeDeleteDialog();
    } else if (!el.mergeDialog.hidden && event.key === "Escape") {
      closeMergeDialog();
    } else if (!el.settingsDialog.hidden && event.key === "Escape") {
      closeSettingsDialog();
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

async function runHostAction(action) {
  const id = state.selectedSession?.id;
  if (!id) {
    return;
  }
  try {
    await invoke(action, { id });
  } catch (error) {
    showToast(String(error), true);
  }
}

// The host asks to show a session, e.g. one picked in VS Code's quick search.
async function revealSession({ id, query = "" } = {}) {
  if (!id) {
    return;
  }
  await state.loaded;
  el.search.value = query;
  el.clearSearch.hidden = !query;
  clearTimeout(state.searchTimer);
  await runSearch({ keepSelection: false });
  const hit = state.hits.find((candidate) => candidate.id === id);
  if (hit?.done && !state.showDone) {
    state.showDone = true;
    localStorage.setItem("showDone", "true");
    updateToggleDoneTitle();
  }
  if (hit && state.project && hit.project.key !== state.project) {
    setProject("");
  }
  renderList();
  await selectSession(id, { focusList: true });
}

async function init() {
  hydrateIcons();
  await initI18n();
  updateToggleDoneTitle();
  setupSplitter();
  bindEvents();
  if (!invoke) {
    el.listEmpty.hidden = false;
    // Locale files cannot be fetched from file:// pages, so keep an English fallback here.
    el.listEmpty.textContent = hasTranslation("app.notInTauri") ? t("app.notInTauri") : "Run this page inside the Tauri app.";
    return;
  }
  state.loaded = refresh();
  window.__TAURI__.event?.listen("sessions-changed", () => refresh({ silent: true }));
  window.__TAURI__.event?.listen("reveal-session", (event) => revealSession(event.payload));
  el.search.focus();
}

init();
