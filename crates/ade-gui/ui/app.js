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
const sbModel = $("sb-model");
const sbRoot = $("sb-root");

let openPath = null;
let dirty = false;
let liveAssistant = null; // element currently being streamed into

// --- helpers ----------------------------------------------------------------

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
function escapeHtml(s) {
  return s.replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" }[c]));
}

// Minimal markdown: fenced code blocks, inline code, bold, paragraphs.
function renderMarkdown(text) {
  const parts = text.split(/```/);
  let html = "";
  parts.forEach((part, i) => {
    if (i % 2 === 1) {
      const body = part.replace(/^[^\n]*\n/, (m) => (part.indexOf("\n") === m.length - 1 ? "" : m));
      // Strip an optional language token on the first line.
      const nl = part.indexOf("\n");
      const code = nl >= 0 && !part.slice(0, nl).includes(" ") ? part.slice(nl + 1) : part;
      html += `<pre><code>${escapeHtml(code.replace(/\n$/, ""))}</code></pre>`;
    } else {
      let seg = escapeHtml(part)
        .replace(/`([^`]+)`/g, "<code>$1</code>")
        .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
      seg = seg
        .split(/\n{2,}/)
        .map((p) => `<p>${p.replace(/\n/g, "<br>")}</p>`)
        .join("");
      html += seg;
    }
  });
  return html;
}

function addMessage(role, text) {
  clearEmpty();
  const el = document.createElement("div");
  el.className = "msg " + role;
  if (role === "assistant") el.innerHTML = renderMarkdown(text);
  else el.textContent = text;
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

// --- streaming events -------------------------------------------------------

listen("assistant-delta", (e) => {
  if (!liveAssistant) {
    liveAssistant = addMessage("assistant", "");
    liveAssistant._raw = "";
    liveAssistant.classList.add("streaming");
  }
  liveAssistant._raw += e.payload;
  liveAssistant.innerHTML = renderMarkdown(liveAssistant._raw);
  scrollDown();
});

listen("assistant-end", () => {
  if (liveAssistant) liveAssistant.classList.remove("streaming");
  liveAssistant = null;
});

listen("tool-call", (e) => {
  if (liveAssistant) liveAssistant.classList.remove("streaming");
  liveAssistant = null;
  addTool(e.payload.name, e.payload.summary);
});

listen("tool-result", (e) => {
  const text = String(e.payload.result || "");
  const r = document.createElement("div");
  r.className = "result " + (e.payload.ok ? "ok" : "bad");
  const LIMIT = 500;
  if (text.length > LIMIT) {
    r.textContent = text.slice(0, LIMIT);
    const more = document.createElement("span");
    more.className = "more";
    more.textContent = "  ▸ show all (" + text.length + " chars)";
    let expanded = false;
    more.addEventListener("click", () => {
      expanded = !expanded;
      r.textContent = expanded ? text : text.slice(0, LIMIT);
      more.textContent = expanded ? "  ▾ show less" : "  ▸ show all (" + text.length + " chars)";
      r.appendChild(more);
    });
    r.appendChild(more);
  } else {
    r.textContent = text;
  }
  const tools = transcript.querySelectorAll(".tool");
  (tools[tools.length - 1] || transcript).appendChild(r);
  scrollDown();
});

listen("permission-request", (e) => {
  if (liveAssistant) liveAssistant.classList.remove("streaming");
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
  row.append(mk("Allow", 1, "ok"), mk("Always", 2, "ok"), mk("Deny", 0, "no"));
  card.append(q, s, row);
  transcript.appendChild(card);
  scrollDown();
});

// --- model list + status bar ------------------------------------------------

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
      setStatus("no config", "error");
      sbModel.textContent = "no models — edit ~/.config/ade/config.toml";
      return;
    }
    for (const m of models) {
      const o = document.createElement("option");
      o.value = m.name;
      o.textContent = `${m.name} (${m.kind})`;
      if (m.default) o.selected = true;
      modelSel.appendChild(o);
    }
    updateSbModel();
  } catch (e) {
    setStatus("config error", "error");
  }
}
function updateSbModel() {
  sbModel.textContent = "◆ " + (modelSel.value || "—");
}
modelSel.addEventListener("change", updateSbModel);

async function loadRoot() {
  try {
    sbRoot.textContent = await invoke("project_root");
  } catch {}
}

// --- file tree --------------------------------------------------------------

async function loadChildren(rel, container) {
  let entries;
  try {
    entries = await invoke("list_tree", { rel });
  } catch {
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

// --- editor -----------------------------------------------------------------

function setEditorPath(rel, isDirty) {
  editorPathEl.innerHTML = (isDirty ? '<span class="dot">●</span>' : "") + (rel || "no file open");
}
async function openFile(rel) {
  try {
    const text = await invoke("read_file_text", { rel });
    codeEl.value = text;
    openPath = rel;
    dirty = false;
    saveBtn.disabled = true;
    setEditorPath(rel, false);
  } catch (e) {
    setEditorPath(rel + " — " + e, false);
    codeEl.value = "";
    openPath = null;
  }
}
async function reloadOpenFile() {
  if (openPath && !dirty) await openFile(openPath);
}
async function saveFile() {
  if (!openPath || !dirty) return;
  try {
    await invoke("save_file_text", { rel: openPath, content: codeEl.value });
    dirty = false;
    saveBtn.disabled = true;
    setEditorPath(openPath, false);
  } catch (e) {
    setEditorPath(openPath + " — save failed: " + e, true);
  }
}
codeEl.addEventListener("input", () => {
  if (openPath && !dirty) {
    dirty = true;
    saveBtn.disabled = false;
    setEditorPath(openPath, true);
  }
});
saveBtn.addEventListener("click", saveFile);
window.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === "s") {
    e.preventDefault();
    saveFile();
  }
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

// --- resizable splitters ----------------------------------------------------

document.querySelectorAll(".splitter").forEach((sp) => {
  sp.addEventListener("mousedown", (e) => {
    e.preventDefault();
    sp.classList.add("dragging");
    const target = sp.dataset.target; // "sidebar" | "chat"
    const startX = e.clientX;
    const varName = target === "sidebar" ? "--sidebar-w" : "--chat-w";
    const start = parseInt(getComputedStyle(document.documentElement).getPropertyValue(varName));
    const move = (ev) => {
      const delta = ev.clientX - startX;
      // sidebar grows to the right; chat grows to the left.
      let next = target === "sidebar" ? start + delta : start - delta;
      next = Math.max(140, Math.min(640, next));
      document.documentElement.style.setProperty(varName, next + "px");
    };
    const up = () => {
      sp.classList.remove("dragging");
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", up);
    };
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", up);
  });
});

// --- init -------------------------------------------------------------------

loadModels();
loadTree();
loadRoot();
promptEl.focus();
