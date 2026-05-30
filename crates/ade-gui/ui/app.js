// ADE GUI frontend. Uses the global Tauri API (withGlobalTauri) — no bundler.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const transcript = $("transcript");
const modelSel = $("model");
const promptEl = $("prompt");
const sendBtn = $("send");
const statusEl = $("status");

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
promptEl.focus();
