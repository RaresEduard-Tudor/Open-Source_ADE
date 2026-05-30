//! `ade` — CLI-first Agent Development Environment.

mod approver;

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use ade_core::agent::{Agent, Reporter};
use ade_core::config::Config;
use ade_core::mcp::McpHost;
use ade_core::permission::{AlwaysAllow, Approver, PermissionGate};
use ade_core::provider::{self, Provider};
use ade_core::session::Session;
use ade_core::skills::SkillRegistry;
use ade_core::tools::{ToolContext, ToolRegistry};

use approver::InteractiveApprover;

#[derive(Parser)]
#[command(name = "ade", version, about = "Open Source Agent Development Environment")]
struct Cli {
    /// Provider/model to use (overrides the configured default).
    #[arg(short, long, global = true)]
    model: Option<String>,
    /// Print raw provider request bodies for full visibility.
    #[arg(long, global = true)]
    verbose: bool,
    /// Auto-approve all mutating actions (use with care).
    #[arg(long, global = true)]
    yes: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start an interactive agent session in the current directory.
    Chat,
    /// Run a single agentic task and exit.
    Run {
        /// The task prompt.
        prompt: Vec<String>,
    },
    /// List configured providers/models.
    Models,
    /// Inspect MCP servers.
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Inspect skills.
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// Inspect configuration.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Subcommand)]
enum McpAction {
    /// List configured servers and their discovered tools.
    List,
}

#[derive(Subcommand)]
enum SkillsAction {
    /// List skills found in the global and project folders.
    List,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print the global and project config paths.
    Path,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir().context("cannot read current directory")?;
    let cfg = Config::load(&cwd)?;

    match cli.command {
        Command::Models => cmd_models(&cfg, cli.model.as_deref()),
        Command::Config { action: ConfigAction::Path } => cmd_config_path(&cwd),
        Command::Mcp { action: McpAction::List } => cmd_mcp_list(&cfg),
        Command::Skills { action: SkillsAction::List } => cmd_skills_list(&cwd),
        Command::Run { ref prompt } => {
            let prompt = prompt.join(" ");
            if prompt.trim().is_empty() {
                anyhow::bail!("run: empty prompt");
            }
            cmd_run(&cfg, &cwd, &cli, &prompt).await
        }
        Command::Chat => cmd_chat(&cfg, &cwd, &cli).await,
    }
}

// ---- inspection commands ---------------------------------------------------

fn cmd_models(cfg: &Config, requested: Option<&str>) -> Result<()> {
    if cfg.providers.is_empty() {
        println!("No providers configured.");
        println!("Add one to {}", Config::global_path()?.display());
        return Ok(());
    }
    let active = cfg.select_provider(requested).ok();
    for p in &cfg.providers {
        let marker = matches!(&active, Some(a) if a.name == p.name)
            .then_some("*")
            .unwrap_or(" ");
        println!(
            "{marker} {:<14} {:<10} {}",
            p.name,
            format!("{:?}", p.kind).to_lowercase(),
            p.model
        );
    }
    Ok(())
}

fn cmd_config_path(cwd: &Path) -> Result<()> {
    println!("global:  {}", Config::global_path()?.display());
    println!("project: {}", Config::project_path(cwd).display());
    Ok(())
}

fn cmd_mcp_list(cfg: &Config) -> Result<()> {
    if cfg.mcp.is_empty() {
        println!("No MCP servers configured.");
        return Ok(());
    }
    let mut reg = ToolRegistry::new();
    let _host = McpHost::start(&cfg.mcp, &mut reg);
    for server in &cfg.mcp {
        println!("{} ({:?})", server.name, server.transport);
    }
    println!("\ntools:");
    for spec in reg.specs() {
        println!("  {}", spec.name);
    }
    Ok(())
}

fn cmd_skills_list(cwd: &Path) -> Result<()> {
    let skills = SkillRegistry::discover(cwd);
    if skills.is_empty() {
        println!("No skills found.");
        println!("Add one at ~/.config/ade/skills/<name>/SKILL.md or ./.ade/skills/<name>/SKILL.md");
        return Ok(());
    }
    for s in skills.list() {
        println!("{:<20} {}", s.name, s.description);
    }
    Ok(())
}

// ---- agent commands --------------------------------------------------------

/// Everything needed to run the agent, assembled once. MCP servers and skills
/// are shared here across every model used in the session.
struct Runtime {
    registry: ToolRegistry,
    gate: PermissionGate,
    ctx: ToolContext,
    system: Option<String>,
    _host: McpHost,
    _skills: Arc<SkillRegistry>,
}

fn build_runtime(cfg: &Config, cwd: &Path, yes: bool) -> Runtime {
    let mut registry = ToolRegistry::with_builtins();

    // Shared MCP host: spawn each server once, fold tools into the registry.
    let host = McpHost::start(&cfg.mcp, &mut registry);

    // Shared skills: advertise in the system prompt, load via use_skill.
    let skills = Arc::new(SkillRegistry::discover(cwd));
    skills.register_tool(&mut registry);

    let approver: Box<dyn Approver> = if yes {
        Box::new(AlwaysAllow)
    } else {
        Box::new(InteractiveApprover::new(false))
    };
    let gate = PermissionGate::new(cfg.permission.allow.clone(), approver);

    let mut system = format!(
        "You are an agent running inside ADE in the project at {}. \
         Use the provided tools to read, edit, and run code. Be concise and act directly.",
        cwd.display()
    );
    if let Some(sk) = skills.system_prompt() {
        system.push_str("\n\n");
        system.push_str(&sk);
    }

    Runtime {
        registry,
        gate,
        ctx: ToolContext { root: cwd.to_path_buf() },
        system: Some(system),
        _host: host,
        _skills: skills,
    }
}

fn build_provider(cfg: &Config, model: Option<&str>) -> Result<Box<dyn Provider>> {
    let pcfg = cfg.select_provider(model)?;
    Ok(provider::build(pcfg)?)
}

async fn cmd_run(cfg: &Config, cwd: &Path, cli: &Cli, prompt: &str) -> Result<()> {
    let rt = build_runtime(cfg, cwd, cli.yes);
    let provider = build_provider(cfg, cli.model.as_deref())?;
    let reporter = CliReporter { verbose: cli.verbose };
    let mut session = Session::new();

    let agent = Agent {
        provider: provider.as_ref(),
        registry: &rt.registry,
        gate: &rt.gate,
        ctx: &rt.ctx,
        system: rt.system.clone(),
        max_iters: 25,
    };
    // Output is surfaced incrementally through the reporter.
    agent.run_turn(&mut session, prompt, &reporter).await?;
    Ok(())
}

async fn cmd_chat(cfg: &Config, cwd: &Path, cli: &Cli) -> Result<()> {
    let rt = build_runtime(cfg, cwd, cli.yes);
    let mut model = cli.model.clone();
    let mut provider = build_provider(cfg, model.as_deref())?;
    let reporter = CliReporter { verbose: cli.verbose };
    let mut session = Session::new();

    println!(
        "ADE chat — model: {}, {} tools. /help for commands.",
        provider.name(),
        rt.registry.len()
    );

    let stdin = io::stdin();
    loop {
        print!("\n> ");
        io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 {
            break; // EOF
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(cmd) = line.strip_prefix('/') {
            match handle_slash(cmd, cfg, &rt, &mut model, &mut provider, &mut session, cwd) {
                SlashResult::Quit => break,
                SlashResult::Handled => continue,
                SlashResult::NotACommand => {}
            }
        }

        let agent = Agent {
            provider: provider.as_ref(),
            registry: &rt.registry,
            gate: &rt.gate,
            ctx: &rt.ctx,
            system: rt.system.clone(),
            max_iters: 25,
        };
        if let Err(e) = agent.run_turn(&mut session, line, &reporter).await {
            eprintln!("error: {e}");
        }
    }
    Ok(())
}

enum SlashResult {
    Quit,
    Handled,
    NotACommand,
}

#[allow(clippy::too_many_arguments)]
fn handle_slash(
    cmd: &str,
    cfg: &Config,
    rt: &Runtime,
    model: &mut Option<String>,
    provider: &mut Box<dyn Provider>,
    session: &mut Session,
    cwd: &Path,
) -> SlashResult {
    let mut parts = cmd.split_whitespace();
    match parts.next() {
        Some("quit") | Some("q") | Some("exit") => return SlashResult::Quit,
        Some("help") => {
            println!("/model <name>  switch model (history kept)");
            println!("/models        list models");
            println!("/tools         list available tools");
            println!("/clear         clear conversation history");
            println!("/save          save session to .ade/sessions/");
            println!("/quit          exit");
        }
        Some("models") => {
            let _ = cmd_models(cfg, model.as_deref());
        }
        Some("tools") => {
            for spec in rt.registry.specs() {
                println!("  {}", spec.name);
            }
        }
        Some("clear") => {
            *session = Session::new();
            println!("(history cleared)");
        }
        Some("save") => {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let path: PathBuf = Session::dir(cwd).join(format!("{ts}.jsonl"));
            match session.save(&path) {
                Ok(()) => println!("saved {}", path.display()),
                Err(e) => eprintln!("save failed: {e}"),
            }
        }
        Some("model") => match parts.next() {
            Some(name) => match build_provider(cfg, Some(name)) {
                Ok(p) => {
                    *provider = p;
                    *model = Some(name.to_string());
                    println!("switched to {name}");
                }
                Err(e) => eprintln!("{e}"),
            },
            None => eprintln!("usage: /model <name>"),
        },
        _ => return SlashResult::NotACommand,
    }
    SlashResult::Handled
}

// ---- reporter --------------------------------------------------------------

struct CliReporter {
    verbose: bool,
}

impl Reporter for CliReporter {
    fn on_assistant(&self, text: &str) {
        println!("{text}");
    }
    fn on_tool_call(&self, name: &str, summary: &str) {
        println!("  ▸ {name}: {summary}");
    }
    fn on_tool_result(&self, _name: &str, result: &str, ok: bool) {
        if self.verbose {
            println!("    {}", result.replace('\n', "\n    "));
        } else {
            let first = result.lines().next().unwrap_or("");
            let extra = result.lines().count().saturating_sub(1);
            let tail = if extra > 0 { format!(" (+{extra} lines)") } else { String::new() };
            let mark = if ok { "✓" } else { "✗" };
            println!("    {mark} {first}{tail}");
        }
    }
    fn on_denied(&self, name: &str, _summary: &str) {
        println!("    ✗ {name} denied");
    }
}
