// Runs in the webview before the shared app (src/main.js) and stands in for the Tauri API:
// commands and events travel over postMessage to the extension, and localStorage is backed by
// the extension's global state so settings survive closing the panel.
(function () {
  const vscode = acquireVsCodeApi();
  const init = JSON.parse(document.getElementById("host-init").textContent);

  const storage = new Map(Object.entries(init.storage || {}));
  const localStorageShim = {
    getItem: (key) => (storage.has(key) ? storage.get(key) : null),
    setItem(key, value) {
      storage.set(key, String(value));
      vscode.postMessage({ type: "storage", key, value: String(value) });
    },
    removeItem(key) {
      storage.delete(key);
      vscode.postMessage({ type: "storage", key, value: null });
    },
    clear() {
      for (const key of [...storage.keys()]) {
        this.removeItem(key);
      }
    },
    key: (index) => [...storage.keys()][index] ?? null,
    get length() {
      return storage.size;
    }
  };
  Object.defineProperty(window, "localStorage", { value: localStorageShim, configurable: true });

  let nextId = 1;
  const pending = new Map();
  const listeners = new Map();
  // Events that arrive before the app subscribes, e.g. a reveal request for a new panel.
  const queued = new Map();

  window.addEventListener("message", ({ data }) => {
    if (data?.type === "response") {
      const request = pending.get(data.id);
      if (request) {
        pending.delete(data.id);
        "error" in data ? request.reject(data.error) : request.resolve(data.result);
      }
    } else if (data?.type === "event") {
      const handlers = listeners.get(data.event);
      if (handlers?.size) {
        handlers.forEach((handler) => handler({ event: data.event, payload: data.payload }));
      } else {
        const list = queued.get(data.event) || [];
        list.push(data.payload);
        queued.set(data.event, list);
      }
    }
  });

  window.__TAURI__ = {
    hostActions: init.hostActions || [],
    workspaceFolders: init.workspaceFolders || [],
    core: {
      invoke(command, args = {}) {
        const id = nextId++;
        return new Promise((resolve, reject) => {
          pending.set(id, { resolve, reject });
          vscode.postMessage({ type: "invoke", id, command, args });
        });
      }
    },
    event: {
      async listen(event, handler) {
        const handlers = listeners.get(event) || new Set();
        handlers.add(handler);
        listeners.set(event, handlers);
        const backlog = queued.get(event);
        if (backlog) {
          queued.delete(event);
          backlog.forEach((payload) => handler({ event, payload }));
        }
        return () => handlers.delete(handler);
      }
    }
  };

  vscode.postMessage({ type: "ready" });
})();
