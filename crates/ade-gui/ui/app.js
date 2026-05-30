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
const editorPathEl = $("editor-path");
const saveBtn = $("save");
const sbModel = $("sb-model");
const sbRoot = $("sb-root");

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

function isDiff(text) {
  return /\n@@ |\n--- |^@@ |^--- /.test(text) || text.includes("\n+") || text.includes("\n-");
}
function renderDiff(text) {
  const pre = document.createElement("pre");
  pre.className = "diff";
  for (const line of text.split("\n")) {
    const span = document.createElement("span");
    const c = line[0];
    span.className =
      c === "+" ? "d-add" : c === "-" ? "d-del" : c === "@" ? "d-hunk" : "d-ctx";
    span.textContent = line + "\n";
    pre.appendChild(span);
  }
  return pre;
}

listen("tool-result", (e) => {
  const text = String(e.payload.result || "");
  const name = e.payload.name || "";

  // Colorized diff for file edits.
  if ((name === "edit_file" || name === "write_file") && e.payload.ok && isDiff(text)) {
    const tools = transcript.querySelectorAll(".tool");
    (tools[tools.length - 1] || transcript).appendChild(renderDiff(text));
    scrollDown();
    return;
  }

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

// --- editor (CodeMirror tabs) -----------------------------------------------

const editors = new Map(); // rel -> { cm, dirty, tabEl, wrap }
let activeEditor = null;
const cmHost = $("cm-host");
const editorTabs = $("editor-tabs");

const MODES = {
  js: { name: "javascript" }, jsx: { name: "javascript" }, mjs: { name: "javascript" },
  ts: { name: "javascript", typescript: true }, tsx: { name: "javascript", typescript: true },
  json: { name: "javascript", json: true },
  rs: "rust", py: "python", css: "css", scss: "css",
  html: "htmlmixed", htm: "htmlmixed", xml: "xml", svg: "xml",
  md: "markdown", markdown: "markdown",
  sh: "shell", bash: "shell", zsh: "shell",
  toml: "toml", yaml: "yaml", yml: "yaml",
  c: "text/x-csrc", h: "text/x-csrc", cpp: "text/x-c++src", hpp: "text/x-c++src",
};
function modeFor(rel) {
  const ext = rel.split(".").pop().toLowerCase();
  return MODES[ext] || null;
}
function cmTheme() {
  const t = document.body.dataset.theme;
  return t === "light" ? "default" : t === "monokai" ? "monokai" : "material-darker";
}
function setEditorPath() {
  const e = editors.get(activeEditor);
  editorPathEl.innerHTML =
    (e && e.dirty ? '<span class="dot">●</span>' : "") + (activeEditor || "no file open");
  saveBtn.disabled = !(e && e.dirty);
}

async function openFile(rel) {
  if (editors.has(rel)) return selectEditor(rel);
  let text;
  try {
    text = await invoke("read_file_text", { rel });
  } catch (e) {
    editorPathEl.textContent = rel + " — " + e;
    return;
  }
  const emptyMsg = cmHost.querySelector(".cm-empty");
  if (emptyMsg) emptyMsg.remove();

  const wrap = document.createElement("div");
  wrap.className = "cm-wrap";
  cmHost.appendChild(wrap);
  const cm = CodeMirror(wrap, {
    value: text,
    mode: modeFor(rel),
    theme: cmTheme(),
    lineNumbers: true,
    lineWrapping: false,
  });
  cm.on("change", () => markDirty(rel));

  const tabEl = document.createElement("div");
  tabEl.className = "etab";
  const name = document.createElement("span");
  name.textContent = rel.split("/").pop();
  const x = document.createElement("span");
  x.className = "x";
  x.textContent = "✕";
  x.addEventListener("click", (ev) => {
    ev.stopPropagation();
    closeEditor(rel);
  });
  tabEl.append(name, x);
  tabEl.addEventListener("click", () => selectEditor(rel));
  editorTabs.appendChild(tabEl);

  editors.set(rel, { cm, dirty: false, tabEl, wrap });
  selectEditor(rel);
}

function selectEditor(rel) {
  activeEditor = rel;
  for (const [r, ed] of editors) {
    const on = r === rel;
    ed.wrap.classList.toggle("hidden", !on);
    ed.tabEl.classList.toggle("active", on);
    if (on) {
      ed.cm.refresh();
      ed.cm.focus();
    }
  }
  setEditorPath();
}

function markDirty(rel) {
  const ed = editors.get(rel);
  if (!ed || ed.dirty) return;
  ed.dirty = true;
  ed.tabEl.classList.add("dirty");
  if (rel === activeEditor) setEditorPath();
}

function closeEditor(rel) {
  const ed = editors.get(rel);
  if (!ed) return;
  ed.wrap.remove();
  ed.tabEl.remove();
  editors.delete(rel);
  if (activeEditor === rel) {
    activeEditor = null;
    const next = editors.keys().next();
    if (!next.done) selectEditor(next.value);
    else {
      cmHost.innerHTML = '<div class="cm-empty">Select a file from the explorer.</div>';
      setEditorPath();
    }
  }
}

async function saveActive() {
  const ed = editors.get(activeEditor);
  if (!ed || !ed.dirty) return;
  try {
    await invoke("save_file_text", { rel: activeEditor, content: ed.cm.getValue() });
    ed.dirty = false;
    ed.tabEl.classList.remove("dirty");
    setEditorPath();
  } catch (e) {
    editorPathEl.textContent = activeEditor + " — save failed: " + e;
  }
}

// Reload open, unmodified files after the agent may have changed them.
async function reloadOpenFile() {
  for (const [rel, ed] of editors) {
    if (ed.dirty) continue;
    try {
      const text = await invoke("read_file_text", { rel });
      if (text !== ed.cm.getValue()) {
        const pos = ed.cm.getCursor();
        ed.cm.setValue(text);
        ed.cm.setCursor(pos);
        ed.dirty = false;
        ed.tabEl.classList.remove("dirty");
      }
    } catch {}
  }
}

saveBtn.addEventListener("click", saveActive);
window.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === "s") {
    e.preventDefault();
    saveActive();
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
    const target = sp.dataset.target; // sidebar | chat | panel
    const horizontal = sp.classList.contains("h");
    const startPos = horizontal ? e.clientY : e.clientX;
    const varName =
      target === "sidebar" ? "--sidebar-w" : target === "chat" ? "--chat-w" : "--panel-h";
    const start = parseInt(getComputedStyle(document.documentElement).getPropertyValue(varName));
    const move = (ev) => {
      const delta = (horizontal ? ev.clientY : ev.clientX) - startPos;
      // sidebar grows right; chat & panel grow toward the start (left/up).
      let next = target === "sidebar" ? start + delta : start - delta;
      next = Math.max(horizontal ? 80 : 140, Math.min(horizontal ? 600 : 640, next));
      document.documentElement.style.setProperty(varName, next + "px");
      if (target === "panel") fitActiveTerminal();
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

// --- theme ------------------------------------------------------------------

const themeSel = $("theme");
function applyTheme(name) {
  document.body.dataset.theme = name;
  themeSel.value = name;
  localStorage.setItem("ade-theme", name);
  // Recolor open terminals to match.
  const css = getComputedStyle(document.body);
  const theme = {
    background: css.getPropertyValue("--bg").trim(),
    foreground: css.getPropertyValue("--text").trim(),
  };
  for (const t of terminals.values()) t.term.options.theme = theme;
  for (const ed of editors.values()) ed.cm.setOption("theme", cmTheme());
}
themeSel.addEventListener("change", () => applyTheme(themeSel.value));

// --- bottom panel tabs ------------------------------------------------------

function showPanelTab(tab) {
  document.querySelectorAll(".ptab").forEach((b) => b.classList.toggle("active", b.dataset.tab === tab));
  $("terminal-view").classList.toggle("hidden", tab !== "terminal");
  $("preview-view").classList.toggle("hidden", tab !== "preview");
  $("term-actions").classList.toggle("hidden", tab !== "terminal");
  $("preview-actions").classList.toggle("hidden", tab !== "preview");
  if (tab === "terminal") {
    if (terminals.size === 0) newTerminal();
    fitActiveTerminal();
  }
}
document.querySelectorAll(".ptab").forEach((b) =>
  b.addEventListener("click", () => showPanelTab(b.dataset.tab))
);

// --- terminals (xterm + PTY) ------------------------------------------------

const terminals = new Map(); // id -> { term, fit, el, tabEl }
let activeTerm = null;
const termHost = $("term-host");
const termTabs = $("term-tabs");

function termTheme() {
  const css = getComputedStyle(document.body);
  return {
    background: css.getPropertyValue("--bg").trim(),
    foreground: css.getPropertyValue("--text").trim(),
  };
}

async function newTerminal() {
  const el = document.createElement("div");
  el.className = "term-inst";
  termHost.appendChild(el);

  const term = new Terminal({
    fontFamily: getComputedStyle(document.body).getPropertyValue("--mono"),
    fontSize: 13,
    cursorBlink: true,
    theme: termTheme(),
  });
  const fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open(el);
  fit.fit();

  let id;
  try {
    id = await invoke("term_open", { rows: term.rows, cols: term.cols });
  } catch (e) {
    term.write("\r\n\x1b[31mfailed to open terminal: " + e + "\x1b[0m\r\n");
    return;
  }

  const tabEl = document.createElement("div");
  tabEl.className = "term-tab";
  const label = document.createElement("span");
  label.textContent = "sh " + id;
  const x = document.createElement("span");
  x.className = "x";
  x.textContent = "✕";
  x.addEventListener("click", (ev) => {
    ev.stopPropagation();
    closeTerminal(id);
  });
  tabEl.append(label, x);
  tabEl.addEventListener("click", () => selectTerminal(id));
  termTabs.appendChild(tabEl);

  terminals.set(id, { term, fit, el, tabEl });
  term.onData((d) => invoke("term_input", { id, data: d }));
  new ResizeObserver(() => {
    if (activeTerm === id) {
      fit.fit();
      invoke("term_resize", { id, rows: term.rows, cols: term.cols });
    }
  }).observe(el);

  selectTerminal(id);
}

function selectTerminal(id) {
  activeTerm = id;
  for (const [tid, t] of terminals) {
    const on = tid === id;
    t.el.classList.toggle("hidden", !on);
    t.tabEl.classList.toggle("active", on);
    if (on) {
      t.fit.fit();
      t.term.focus();
      invoke("term_resize", { id, rows: t.term.rows, cols: t.term.cols });
    }
  }
}

function closeTerminal(id) {
  const t = terminals.get(id);
  if (!t) return;
  invoke("term_close", { id });
  t.term.dispose();
  t.el.remove();
  t.tabEl.remove();
  terminals.delete(id);
  if (activeTerm === id) {
    const next = terminals.keys().next();
    activeTerm = null;
    if (!next.done) selectTerminal(next.value);
  }
}

function fitActiveTerminal() {
  const t = terminals.get(activeTerm);
  if (t) {
    t.fit.fit();
    invoke("term_resize", { id: activeTerm, rows: t.term.rows, cols: t.term.cols });
  }
}

$("term-new").addEventListener("click", newTerminal);

listen("term-output", (e) => {
  const t = terminals.get(e.payload.id);
  if (t) t.term.write(e.payload.data);
});
listen("term-exit", (e) => {
  const t = terminals.get(e.payload.id);
  if (t) t.term.write("\r\n\x1b[90m[process exited]\x1b[0m\r\n");
});

// --- preview ----------------------------------------------------------------

const previewFrame = $("preview-frame");
const previewUrl = $("preview-url");
function loadPreview() {
  let url = previewUrl.value.trim();
  if (url && !/^https?:\/\//.test(url)) url = "http://" + url;
  previewFrame.src = url || "about:blank";
}
$("preview-go").addEventListener("click", loadPreview);
$("preview-reload").addEventListener("click", () => {
  // Force reload even if the URL is unchanged.
  const u = previewFrame.src;
  previewFrame.src = "about:blank";
  setTimeout(() => (previewFrame.src = u), 30);
});
previewUrl.addEventListener("keydown", (e) => {
  if (e.key === "Enter") loadPreview();
});

// --- init -------------------------------------------------------------------

applyTheme(localStorage.getItem("ade-theme") || "dark");
loadModels();
loadTree();
loadRoot();
showPanelTab("terminal");
promptEl.focus();
