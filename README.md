# ADE — Open Source Agent Development Environment

Run multiple AI coding agents in one place, with full code visibility. Bring
your own API key, switch models freely, and watch every file edit and shell
command before it runs. No black box.

**Any agent, any API. Open source. Lightweight. Yours.**

- **Multi-model, BYO key** — Claude, Gemini, Deepseek, Mimo, local models. A
  model is a config entry, not code.
- **Agentic** — agents read, edit, and run code through gated tools.
- **Shared MCP + skills** — configure an MCP server or drop a skill folder once;
  every model uses them. No per-agent installs.
- **Tiny** — single Rust binary, ~2.6 MB. WSL2-native.
- **CLI first** — Tauri GUI is planned for phase 2.

## Install

```sh
cargo build --release
# binary at target/release/ade
```

## Configure

Copy `examples/config.toml` to `~/.config/ade/config.toml` (or `./.ade/config.toml`
in a project) and set your keys via env vars. The three adapter kinds —
`anthropic`, `openai`, `gemini` — cover everything; `openai` handles Deepseek,
Mimo, and any OpenAI-compatible or local endpoint.

```sh
export ANTHROPIC_API_KEY=sk-...
ade models                       # list configured models
ade run "add a --version flag"   # one-shot agentic task
ade chat                         # interactive session
```

## Commands

| Command | Description |
| --- | --- |
| `ade run "<task>"` | One-shot agentic task in the current dir |
| `ade chat` | Interactive REPL (`/model`, `/tools`, `/clear`, `/save`, `/quit`) |
| `ade models` | List configured providers/models |
| `ade mcp list` | List MCP servers and their tools |
| `ade skills list` | List skills (global + project) |
| `ade config path` | Show config file locations |

Flags: `-m/--model <name>` per-call override, `--verbose` (raw tool output),
`--yes` (auto-approve mutating actions).

## Tools & permissions

Built-in tools: `read_file`, `list_dir`, `search`, `write_file`, `edit_file`,
`run_shell`. Read-only tools run freely; mutating ones prompt unless they match
an `allow` prefix in config (or you pass `--yes`). MCP and skill tools merge
into the same list, so every model sees them identically.

## MCP & skills

- **MCP** — add a `[[mcp]]` block (stdio transport). Servers start once and are
  shared across all agents; tools appear as `mcp__<server>__<tool>`.
- **Skills** — create `~/.config/ade/skills/<name>/SKILL.md` (or under
  `./.ade/skills/`). Only name + description load up front; the agent calls
  `use_skill` to pull full instructions on demand.

## GUI (preview)

A Tauri desktop shell lives in `crates/ade-gui` — a model dropdown plus a
streaming chat panel, reusing the exact same core (providers, tools, streaming,
skills). It is **not** part of the default build because it needs system webkit
libraries.

On Debian/Ubuntu/WSL2, install the prerequisites once:

```sh
sudo apt update && sudo apt install -y \
  libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

Then run it (WSLg shows the window automatically):

```sh
cargo run -p ade-gui
```

Features: three-pane VSCode-style layout (resizable file tree | editor | streaming
chat), markdown rendering, in-app permission prompts (Allow / Always / Deny),
Ctrl/Cmd+S save, status bar, built-in tools, skills, and shared MCP servers — the
same core as the CLI. Planned: syntax highlighting, diff view, multi-window.

### Platforms

The CLI and core are pure Rust and run on **Linux, macOS, and Windows** (the
shell tool uses `cmd.exe` on Windows, `/bin/sh` elsewhere). The GUI uses Tauri,
so it runs on all three using the system webview — no per-OS code, only per-OS
build deps:

- **Linux/WSL2:** `webkit2gtk-4.1` + `librsvg` (see above).
- **macOS:** Xcode command-line tools (`xcode-select --install`). WKWebView ships
  with the OS.
- **Windows:** WebView2 runtime (preinstalled on Windows 11; otherwise a small
  Microsoft download) and the MSVC build tools.

## Architecture

```text
crates/ade-core   provider adapters, tool registry, MCP host, skills, agent loop
crates/ade-cli    the `ade` binary
crates/ade-gui    Tauri desktop shell (preview) over ade-core
```

Workspace, lib-first so a future GUI shares the core. Built for a tiny binary
(`opt-level = "z"`, LTO, stripped, `panic = "abort"`).

## Status

MVP: multi-model agent loop with **streaming** responses, built-in tools,
permission gate, shared MCP (stdio) and skills, sessions. Planned: MCP SSE
transport, Tauri GUI.

## License

MIT OR Apache-2.0.
