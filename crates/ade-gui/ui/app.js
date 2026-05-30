// ADE GUI frontend. Uses the global Tauri API (withGlobalTauri) — no bundler.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const transcript = $("transcript");
const modelSel = $("model");
const promptEl = $("prompt");
const sendBtn = $("send");
const statusEl = $("status");
const treeEl = $("tree");
const codeEl = $("code");
const editorPathEl = $("editor-path");
const saveBtn = $("save");

let openPath = null;
let dirty = false;

// The assistant element currently being streamed into (null = start a new one).
let liveAssistant = null;

function setStatus(text, cls) {
  statusEl.textContent = text;
  statusEl.className = "status " + cls;
}

function clearEmpty() {
  const e = $("empty");
  if (e) e.remove();
}

function scrollDown() {
  transcript.scrollTop = transcript.scrollHeight;
}

function addMessage(role, text) {
  clearEmpty();
  const el = document.createElement("div");
  el.className = "msg " + role;
  el.textContent = text;
  transcript.appendChild(el);
  scrollDown();
  return el;
}

function addTool(name, summary) {
  clearEmpty();
  const el = document.createElement("div");
  el.className = "tool";
  const n = document.createElement("div");
  n.className = "name";
  n.textContent = "▸ " + name + (summary ? ": " + summary : "");
  el.appendChild(n);
  transcript.appendChild(el);
  scrollDown();
  return el;
}

// --- file tree --------------------------------------------------------------

async function loadChildren(rel, container) {
  let entries;
  try {
    entries = await invoke("list_tree", { rel });
  } catch (e) {
    return;
  }
  container.innerHTML = "";
  for (const ent of entries) {
    const childRel = rel ? rel + "/" + ent.name : ent.name;
    const node = document.createElement("div");
    node.className = "node";
    const twisty = document.createElement("span");
    twisty.className = "twisty";
    twisty.textContent = ent.dir ? "▸" : "";
    const ic = document.createElement("span");
    ic.className = "ic";
    ic.textContent = ent.dir ? "📁" : "📄";
    const label = document.createElement("span");
    label.textContent = ent.name;
    node.append(twisty, ic, label);
    container.appendChild(node);

    if (ent.dir) {
      const kids = document.createElement("div");
      kids.className = "children";
      kids.style.display = "none";
      container.appendChild(kids);
      let loaded = false;
      node.addEventListener("click", async () => {
        const open = kids.style.display !== "none";
        kids.style.display = open ? "none" : "block";
        twisty.textContent = open ? "▸" : "▾";
        ic.textContent = open ? "📁" : "📂";
        if (!loaded && !open) {
          loaded = true;
          await loadChildren(childRel, kids);
        }
      });
    } else {
      node.addEventListener("click", () => {
        document.querySelectorAll(".tree .node.active").forEach((n) => n.classList.remove("active"));
        node.classList.add("active");
        openFile(childRel);
      });
    }
  }
}

function loadTree() {
  loadChildren("", treeEl);
}

async function openFile(rel) {
  try {
    const text = await invoke("read_file_text", { rel });
    codeEl.value = text;
    editorPathEl.textContent = rel;
    openPath = rel;
    dirty = false;
    saveBtn.disabled = true;
  } catch (e) {
    editorPathEl.textContent = rel + " — " + e;
    codeEl.value = "";
    openPath = null;
  }
}

async function reloadOpenFile() {
  if (openPath && !dirty) {
    const keep = openPath;
    await openFile(keep);
  }
}

codeEl.addEventListener("input", () => {
  if (openPath) {
    dirty = true;
    saveBtn.disabled = false;
  }
});

saveBtn.addEventListener("click", async () => {
  if (!openPath) return;
  try {
    await invoke("save_file_text", { rel: openPath, content: codeEl.value });
    dirty = false;
    saveBtn.disabled = true;
  } catch (e) {
    editorPathEl.textContent = openPath + " — save failed: " + e;
  }
});

// --- model list -------------------------------------------------------------

async function loadModels() {
  try {
    const models = await invoke("list_models");
    modelSel.innerHTML = "";
    if (!models.length) {
      const o = document.createElement("option");
      o.textContent = "no models configured";
      o.disabled = true;
      modelSel.appendChild(o);
      sendBtn.disabled = true;
      setStatus("configure ~/.config/ade/config.toml", "error");
      return;
    }
    for (const m of models) {
      const o = document.createElement("option");
      o.value = m.name;
      o.textContent = `${m.name} (${m.kind})`;
      if (m.default) o.selected = true;
      modelSel.appendChild(o);
    }
  } catch (e) {
    setStatus("config error", "error");
  }
}

// --- streaming events -------------------------------------------------------

listen("assistant-delta", (e) => {
  if (!liveAssistant) liveAssistant = addMessage("assistant", "");
  liveAssistant.textContent += e.payload;
  scrollDown();
});

listen("assistant-end", () => {
  liveAssistant = null; // next delta begins a fresh bubble
});

listen("tool-call", (e) => {
  liveAssistant = null;
  const el = addTool(e.payload.name, e.payload.summary);
  el.dataset.pending = "1";
});

listen("permission-request", (e) => {
  liveAssistant = null;
  clearEmpty();
  const { id, tool, summary } = e.payload;
  const card = document.createElement("div");
  card.className = "perm";
  const q = document.createElement("div");
  q.className = "perm-q";
  q.textContent = `Allow ${tool}?`;
  const s = document.createElement("div");
  s.className = "perm-summary";
  s.textContent = summary;
  const row = document.createElement("div");
  row.className = "perm-row";

  const mk = (label, choice, cls) => {
    const b = document.createElement("button");
    b.className = "perm-btn " + cls;
    b.textContent = label;
    b.addEventListener("click", () => {
      invoke("respond_permission", { id, choice });
      row.remove();
      card.classList.add(choice === 0 ? "denied" : "allowed");
      q.textContent =
        (choice === 0 ? "Denied " : "Allowed ") + tool + (choice === 2 ? " (always)" : "");
    });
    return b;
  };
  row.appendChild(mk("Allow", 1, "ok"));
  row.appendChild(mk("Always", 2, "ok"));
  row.appendChild(mk("Deny", 0, "no"));

  card.appendChild(q);
  card.appendChild(s);
  card.appendChild(row);
  transcript.appendChild(card);
  scrollDown();
});

listen("tool-result", (e) => {
  const r = document.createElement("div");
  r.className = "result " + (e.payload.ok ? "ok" : "bad");
  const text = String(e.payload.result || "");
  r.textContent = text.length > 600 ? text.slice(0, 600) + "…" : text;
  // attach to the most recent tool block
  const tools = transcript.querySelectorAll(".tool");
  (tools[tools.length - 1] || transcript).appendChild(r);
  scrollDown();
});

// --- send -------------------------------------------------------------------

async function send() {
  const prompt = promptEl.value.trim();
  if (!prompt) return;
  promptEl.value = "";
  autoGrow();
  addMessage("user", prompt);
  liveAssistant = null;

  sendBtn.disabled = true;
  promptEl.disabled = true;
  setStatus("thinking…", "busy");
  try {
    await invoke("send_prompt", { prompt, model: modelSel.value || null });
    setStatus("ready", "idle");
    // The agent may have changed files: refresh tree + reload the open file.
    loadTree();
    reloadOpenFile();
  } catch (e) {
    addMessage("assistant", "⚠ " + e);
    setStatus("error", "error");
  } finally {
    sendBtn.disabled = false;
    promptEl.disabled = false;
    promptEl.focus();
  }
}

function autoGrow() {
  promptEl.style.height = "auto";
  promptEl.style.height = Math.min(promptEl.scrollHeight, 160) + "px";
}

sendBtn.addEventListener("click", send);
promptEl.addEventListener("input", autoGrow);
promptEl.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) {
    e.preventDefault();
    send();
  }
});

loadModels();
loadTree();
promptEl.focus();
