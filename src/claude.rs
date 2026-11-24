//! Moteur : vibelearn pilote directement le binaire natif `claude` (celui de
//! Claude Code, qui porte la connexion à l'abonnement) avec le protocole
//! qu'utilise le Claude Agent SDK : JSON ligne à ligne sur stdin/stdout, plus
//! des « requêtes de contrôle » pour les permissions et pour le serveur
//! d'outils que vibelearn tient en mémoire. Aucun Node, aucune clé API.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use crate::i18n::tr;
use crate::tools;

// ---------------------------------------------------------------------------
// Localisation du binaire
// ---------------------------------------------------------------------------

const EXE: &str = if cfg!(windows) { "claude.exe" } else { "claude" };

/// Trouve le binaire `claude` : chemin explicite, PATH, installeur natif,
/// puis celui que l'app desktop Claude embarque.
pub fn locate(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
        bail!("{} : {}", tr("binaire claude introuvable", "claude binary not found"), p.display());
    }
    if let Some(dirs) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&dirs) {
            let candidate = dir.join(EXE);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        let candidate = home.join(".local").join("bin").join(EXE);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    if let Some(found) = desktop_bundle() {
        return Ok(found);
    }
    bail!(
        "{}",
        tr(
            "Le binaire `claude` est introuvable. Installe Claude Code (binaire natif, sans Node) :\n  winget install Anthropic.ClaudeCode\nou indique son chemin avec --claude-bin / VIBELEARN_CLAUDE.",
            "The `claude` binary was not found. Install Claude Code (native binary, no Node):\n  winget install Anthropic.ClaudeCode\nor point to it with --claude-bin / VIBELEARN_CLAUDE.",
        )
    )
}

/// L'app desktop range ses binaires sous `…/claude-code/<version>/<build>/`.
fn desktop_bundle() -> Option<PathBuf> {
    let mut roots = Vec::new();
    if let Some(local) = dirs::data_local_dir() {
        if let Ok(entries) = std::fs::read_dir(local.join("Claude-Data")) {
            roots.extend(entries.flatten().map(|e| e.path().join("claude-code")));
        }
        roots.push(local.join("Claude").join("claude-code"));
    }
    if let Some(data) = dirs::data_dir() {
        roots.push(data.join("Claude").join("claude-code"));
    }
    let mut best: Option<(Vec<u32>, PathBuf)> = None;
    for root in roots {
        let Ok(versions) = std::fs::read_dir(&root) else { continue };
        for version in versions.flatten() {
            let key: Vec<u32> = version
                .file_name()
                .to_string_lossy()
                .split('.')
                .filter_map(|p| p.parse().ok())
                .collect();
            if key.is_empty() {
                continue;
            }
            let mut candidates = vec![version.path().join(EXE)];
            if let Ok(builds) = std::fs::read_dir(version.path()) {
                candidates.extend(builds.flatten().map(|b| b.path().join(EXE)));
            }
            for c in candidates {
                if c.is_file() && best.as_ref().is_none_or(|(k, _)| key > *k) {
                    best = Some((key.clone(), c));
                }
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Variables posées par une session Claude hôte (app desktop, Claude Code).
/// Héritées telles quelles, elles feraient croire au sous-processus qu'il est
/// piloté par cet hôte (authentification déléguée, sockets de messagerie…).
const HOST_SESSION_VARS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "CLAUDE_AGENT_SDK_VERSION",
    "CLAUDE_AGENT_SDK_MCP_NO_PREFIX",
    "CLAUDE_PREVIEW_CLASSIFIER_FLOOR",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_HOST_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SDK_HAS_HOST_AUTH_REFRESH",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_OAUTH_SCOPES",
    "CLAUDE_CODE_ACCOUNT_UUID",
    "CLAUDE_CODE_USER_EMAIL",
    "CLAUDE_CODE_ORGANIZATION_UUID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_DESKTOP_APP_VERSION",
    "CLAUDE_CODE_HOST_SHORT_TEMP_DIRS",
    "CLAUDE_CODE_EAGER_FLUSH",
    "CLAUDE_CODE_SIMPLE",
    "CLAUDE_CODE_DISABLE_CRON",
    "CLAUDE_CODE_DISABLE_TERMINAL_TITLE",
    "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING",
    "CLAUDE_CODE_ENABLE_ASK_USER_QUESTION_TOOL",
    "CLAUDE_CODE_REPORT_FINDINGS",
    "CLAUDE_CODE_TERMINAL_MCP_TOOLS",
    "CLAUDE_CODE_EMIT_TOOL_USE_SUMMARIES",
    "CLAUDE_CODE_EMIT_STARTUP_TIMING",
    "CLAUDE_CODE_STARTUP_FAILURE_RESULTS",
    "MCP_SERVER_CONNECTION_BATCH_SIZE",
    "MCP_CONNECTION_NONBLOCKING",
];

/// Prépare l'environnement du sous-processus `claude`.
fn scrub_env(cmd: &mut std::process::Command, keep_api_key: bool) {
    for var in HOST_SESSION_VARS {
        cmd.env_remove(var);
    }
    if !keep_api_key {
        // Le but est d'utiliser l'abonnement : une clé API dans l'environnement
        // prendrait le dessus et facturerait l'API.
        cmd.env_remove("ANTHROPIC_API_KEY");
        cmd.env_remove("ANTHROPIC_AUTH_TOKEN");
    }
    if std::env::var("ANTHROPIC_BASE_URL").is_ok_and(|u| u == "https://api.anthropic.com") {
        cmd.env_remove("ANTHROPIC_BASE_URL");
    }
    // Les outils du tuteur attendent une réponse humaine : pas de délai d'inactivité.
    cmd.env("CLAUDE_CODE_MCP_TOOL_IDLE_TIMEOUT", "0");
}

#[derive(Debug)]
pub struct AuthStatus {
    pub logged_in: bool,
    pub method: String,
}

pub fn version(bin: &Path) -> Option<String> {
    let out = std::process::Command::new(bin).arg("--version").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn auth_status(bin: &Path, keep_api_key: bool) -> Result<AuthStatus> {
    let mut cmd = std::process::Command::new(bin);
    cmd.args(["auth", "status"]);
    scrub_env(&mut cmd, keep_api_key);
    let out = cmd.output().context("claude auth status")?;
    let json: Value = serde_json::from_slice(&out.stdout).context("réponse inattendue de `claude auth status`")?;
    Ok(AuthStatus {
        logged_in: json["loggedIn"].as_bool().unwrap_or(false),
        method: json["authMethod"].as_str().unwrap_or("none").to_string(),
    })
}

/// Lance `claude <args>` dans le terminal courant (connexion, déconnexion).
pub fn passthrough(bin: &Path, args: &[&str]) -> Result<i32> {
    let mut cmd = std::process::Command::new(bin);
    cmd.args(args);
    scrub_env(&mut cmd, false);
    Ok(cmd.status()?.code().unwrap_or(1))
}

// ---------------------------------------------------------------------------
// Événements et commandes
// ---------------------------------------------------------------------------

/// Ce que le moteur rapporte à l'application.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// Le moteur est prêt à recevoir le premier message.
    Ready,
    Session { session_id: String, model: Option<String> },
    BlockStart { kind: String, name: Option<String> },
    Delta { text: String },
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
    ToolDone { id: String, is_error: bool },
    /// Appel d'un outil du tuteur : l'application répond par `tool_reply`.
    ToolCall { id: u64, tool: String, args: Value },
    /// Demande de permission : l'application répond par `permission_reply`.
    Permission { id: u64, tool: String, input: Value, reason: Option<String>, tool_use_id: Option<String> },
    /// Un appel en attente a été abandonné (interruption).
    Cancel { id: u64 },
    Result {
        is_error: bool,
        duration_ms: u64,
        cost_usd: f64,
        input_tokens: u64,
        output_tokens: u64,
        errors: Vec<String>,
    },
    Status { text: String },
    /// Le contexte a été résumé : le modèle a perdu le contenu des fichiers qu'il avait vus.
    Compacted,
    AuthRequired,
    Fatal { message: String },
    Exited { code: Option<i32>, stderr: String },
}

#[derive(Debug)]
pub(crate) enum Cmd {
    User(String),
    Interrupt,
    ToolReply { id: u64, text: String, is_error: bool },
    PermissionReply { id: u64, allow: bool, message: Option<String> },
    Shutdown,
}

pub(crate) type Emit = Arc<dyn Fn(AgentEvent) + Send + Sync>;

pub struct EngineConfig {
    pub claude_bin: PathBuf,
    pub cwd: PathBuf,
    pub system_prompt: String,
    pub title: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub resume: Option<String>,
    pub keep_api_key: bool,
}

/// Poignée vers le moteur (réel ou factice).
pub struct Engine {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl Engine {
    pub fn spawn(cfg: EngineConfig, on_event: impl Fn(AgentEvent) + Send + Sync + 'static) -> Result<Self> {
        let mut std_cmd = std::process::Command::new(&cfg.claude_bin);
        std_cmd.args([
            "--output-format",
            "stream-json",
            "--verbose",
            "--input-format",
            "stream-json",
            "--permission-prompt-tool=stdio",
            "--allowedTools=Read,Glob,Grep,mcp__tutor",
            // PowerShell n'existe que sous Windows.
            if cfg!(windows) {
                "--tools=Read,Glob,Grep,Write,Edit,Bash,PowerShell,WebSearch,WebFetch"
            } else {
                "--tools=Read,Glob,Grep,Write,Edit,Bash,WebSearch,WebFetch"
            },
            // Session isolée : ni réglages, ni plugins, ni serveurs MCP de l'utilisateur.
            "--setting-sources=",
            "--strict-mcp-config",
            "--permission-mode=default",
            "--include-partial-messages",
        ]);
        if let Some(model) = &cfg.model {
            std_cmd.arg(format!("--model={model}"));
        }
        if let Some(effort) = &cfg.effort {
            std_cmd.arg(format!("--effort={effort}"));
        }
        if let Some(resume) = &cfg.resume {
            std_cmd.arg(format!("--resume={resume}"));
        }
        scrub_env(&mut std_cmd, cfg.keep_api_key);
        std_cmd.current_dir(&cfg.cwd);

        let mut cmd = Command::from(std_cmd);
        let child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("{} {}", tr("impossible de lancer", "could not start"), cfg.claude_bin.display()))?;

        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run_real(cfg, child, rx, Arc::new(on_event)));
        Ok(Self { tx })
    }

    /// Moteur sans binaire : vibelearn parle lui-même à une API OpenAI.
    pub fn api(cfg: crate::api::ApiConfig, on_event: impl Fn(AgentEvent) + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(crate::api::run(cfg, rx, Arc::new(on_event)));
        Self { tx }
    }

    /// Moteur factice qui déroule un scénario complet sans appeler de modèle
    /// (tests et démonstration hors connexion).
    pub fn mock(on_event: impl Fn(AgentEvent) + Send + Sync + 'static) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(crate::mock::run(rx_adapter(rx), Arc::new(on_event)));
        Self { tx }
    }

    pub fn send_user(&self, text: &str) {
        let _ = self.tx.send(Cmd::User(text.to_string()));
    }

    pub fn interrupt(&self) {
        let _ = self.tx.send(Cmd::Interrupt);
    }

    pub fn tool_reply(&self, id: u64, text: &str, is_error: bool) {
        let _ = self.tx.send(Cmd::ToolReply { id, text: text.to_string(), is_error });
    }

    pub fn permission_reply(&self, id: u64, allow: bool, message: Option<&str>) {
        let _ = self.tx.send(Cmd::PermissionReply { id, allow, message: message.map(str::to_string) });
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(Cmd::Shutdown);
    }
}

/// Vue simplifiée des commandes pour le moteur factice.
#[derive(Debug)]
pub enum MockCmd {
    User(String),
    ToolReply { id: u64, text: String },
    PermissionReply { id: u64, allow: bool, message: Option<String> },
    Interrupt,
    Shutdown,
}

fn rx_adapter(mut rx: mpsc::UnboundedReceiver<Cmd>) -> mpsc::UnboundedReceiver<MockCmd> {
    let (tx, out) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            let mapped = match cmd {
                Cmd::User(t) => MockCmd::User(t),
                Cmd::ToolReply { id, text, .. } => MockCmd::ToolReply { id, text },
                Cmd::PermissionReply { id, allow, message } => MockCmd::PermissionReply { id, allow, message },
                Cmd::Interrupt => MockCmd::Interrupt,
                Cmd::Shutdown => MockCmd::Shutdown,
            };
            if tx.send(mapped).is_err() {
                break;
            }
        }
    });
    out
}

// ---------------------------------------------------------------------------
// Boucle du moteur réel
// ---------------------------------------------------------------------------

async fn write_line(stdin: &mut Option<ChildStdin>, line: &str) {
    if let Some(s) = stdin {
        let ok = s.write_all(line.as_bytes()).await.is_ok()
            && s.write_all(b"\n").await.is_ok()
            && s.flush().await.is_ok();
        if !ok {
            *stdin = None;
        }
    }
}

async fn run_real(cfg: EngineConfig, mut child: Child, mut cmds: mpsc::UnboundedReceiver<Cmd>, emit: Emit) {
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    let tail_writer = tail.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut t = tail_writer.lock().unwrap();
            if t.len() >= 30 {
                t.pop_front();
            }
            t.push_back(line);
        }
    });

    let mut wire = Wire::new();
    write_line(&mut stdin, &wire.initialize_line(&cfg.system_prompt, &cfg.title)).await;

    let mut lines = BufReader::new(stdout).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { break };
                let step = wire.on_line(&line);
                for out in step.out {
                    write_line(&mut stdin, &out).await;
                }
                for event in step.events {
                    emit(event);
                }
            }
            cmd = cmds.recv() => match cmd {
                None | Some(Cmd::Shutdown) => break,
                Some(cmd) => {
                    if let Some(out) = wire.on_cmd(cmd) {
                        write_line(&mut stdin, &out).await;
                    }
                }
            }
        }
    }

    // Fermer stdin termine proprement la session côté claude.
    drop(stdin);
    let code = match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
        Ok(Ok(status)) => status.code(),
        _ => {
            let _ = child.kill().await;
            None
        }
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let stderr = tail.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n");
    emit(AgentEvent::Exited { code, stderr });
}

// ---------------------------------------------------------------------------
// Protocole (sans E/S : testable)
// ---------------------------------------------------------------------------

const INIT_ID: &str = "vibelearn-init";

struct PendingTool {
    request_id: String,
    rpc_id: Value,
}

struct PendingPermission {
    request_id: String,
    tool_use_id: Value,
    input: Value,
}

#[derive(Default)]
struct Step {
    out: Vec<String>,
    events: Vec<AgentEvent>,
}

struct Wire {
    seq: u64,
    tools: HashMap<u64, PendingTool>,
    permissions: HashMap<u64, PendingPermission>,
}

fn control_success(request_id: &str, response: Value) -> String {
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": response },
    })
    .to_string()
}

impl Wire {
    fn new() -> Self {
        Self { seq: 0, tools: HashMap::new(), permissions: HashMap::new() }
    }

    fn next(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// Première ligne envoyée : déclare le serveur d'outils en mémoire et
    /// remplace le prompt système (hors ligne de commande : pas de limite de taille).
    fn initialize_line(&self, system_prompt: &str, title: &str) -> String {
        json!({
            "type": "control_request",
            "request_id": INIT_ID,
            "request": {
                "subtype": "initialize",
                "sdkMcpServers": [tools::SERVER],
                "systemPrompt": [system_prompt],
                // Les consignes sont relues à chaque lancement : une session
                // reprise suit celles de la version installée, pas celles du
                // jour où la conversation a commencé.
                "systemPromptSnapshot": false,
                "title": title,
            },
        })
        .to_string()
    }

    fn on_cmd(&mut self, cmd: Cmd) -> Option<String> {
        match cmd {
            Cmd::User(text) => Some(
                json!({
                    "type": "user",
                    "message": { "role": "user", "content": text },
                    "parent_tool_use_id": null,
                })
                .to_string(),
            ),
            Cmd::Interrupt => {
                let id = self.next();
                Some(
                    json!({
                        "type": "control_request",
                        "request_id": format!("vibelearn-int-{id}"),
                        "request": { "subtype": "interrupt" },
                    })
                    .to_string(),
                )
            }
            Cmd::ToolReply { id, text, is_error } => {
                let pending = self.tools.remove(&id)?;
                let rpc = json!({
                    "jsonrpc": "2.0",
                    "id": pending.rpc_id,
                    "result": { "content": [{ "type": "text", "text": text }], "isError": is_error },
                });
                Some(control_success(&pending.request_id, json!({ "mcp_response": rpc })))
            }
            Cmd::PermissionReply { id, allow, message } => {
                let pending = self.permissions.remove(&id)?;
                let response = if allow {
                    json!({ "behavior": "allow", "updatedInput": pending.input, "toolUseID": pending.tool_use_id })
                } else {
                    json!({
                        "behavior": "deny",
                        "message": message.unwrap_or_else(|| "Denied by the learner.".to_string()),
                        "toolUseID": pending.tool_use_id,
                    })
                };
                Some(control_success(&pending.request_id, response))
            }
            Cmd::Shutdown => None,
        }
    }

    fn on_line(&mut self, line: &str) -> Step {
        let mut step = Step::default();
        let Ok(v) = serde_json::from_str::<Value>(line) else { return step };
        match v["type"].as_str().unwrap_or("") {
            "control_request" => self.on_control_request(&v, &mut step),
            "control_cancel_request" => {
                let rid = v["request_id"].as_str().unwrap_or("");
                let tool = self.tools.iter().find(|(_, p)| p.request_id == rid).map(|(id, _)| *id);
                let perm = self.permissions.iter().find(|(_, p)| p.request_id == rid).map(|(id, _)| *id);
                if let Some(id) = tool {
                    self.tools.remove(&id);
                    step.events.push(AgentEvent::Cancel { id });
                }
                if let Some(id) = perm {
                    self.permissions.remove(&id);
                    step.events.push(AgentEvent::Cancel { id });
                }
            }
            "control_response" => {
                let r = &v["response"];
                if r["request_id"] == INIT_ID {
                    if r["subtype"] == "error" {
                        step.events.push(AgentEvent::Fatal {
                            message: r["error"].as_str().unwrap_or("initialize failed").to_string(),
                        });
                    } else if r["response"]["account"]["tokenSource"] == "none" {
                        step.events.push(AgentEvent::AuthRequired);
                    } else {
                        step.events.push(AgentEvent::Ready);
                    }
                }
            }
            "system" => match v["subtype"].as_str().unwrap_or("") {
                "init" => step.events.push(AgentEvent::Session {
                    session_id: v["session_id"].as_str().unwrap_or("").to_string(),
                    model: v["model"].as_str().map(str::to_string),
                }),
                "api_retry" => step.events.push(AgentEvent::Status {
                    text: tr("nouvelle tentative de connexion à l'API…", "retrying the API…").to_string(),
                }),
                "compact_boundary" => step.events.push(AgentEvent::Compacted),
                _ => {}
            },
            "stream_event" if v["parent_tool_use_id"].is_null() => {
                let e = &v["event"];
                match e["type"].as_str().unwrap_or("") {
                    "content_block_start" => step.events.push(AgentEvent::BlockStart {
                        kind: e["content_block"]["type"].as_str().unwrap_or("").to_string(),
                        name: e["content_block"]["name"].as_str().map(str::to_string),
                    }),
                    "content_block_delta" if e["delta"]["type"] == "text_delta" => {
                        if let Some(text) = e["delta"]["text"].as_str() {
                            step.events.push(AgentEvent::Delta { text: text.to_string() });
                        }
                    }
                    _ => {}
                }
            }
            "assistant" if v["parent_tool_use_id"].is_null() => {
                if v["error"] == "authentication_failed" {
                    step.events.push(AgentEvent::AuthRequired);
                    return step;
                }
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str().unwrap_or("") {
                        "text" => {
                            if let Some(text) = block["text"].as_str().filter(|t| !t.is_empty()) {
                                step.events.push(AgentEvent::Text { text: text.to_string() });
                            }
                        }
                        "tool_use" => step.events.push(AgentEvent::ToolUse {
                            id: block["id"].as_str().unwrap_or("").to_string(),
                            name: block["name"].as_str().unwrap_or("").to_string(),
                            input: block["input"].clone(),
                        }),
                        _ => {}
                    }
                }
                if let Some(err) = v["error"].as_str() {
                    step.events.push(AgentEvent::Status { text: format!("{} : {err}", tr("erreur du modèle", "model error")) });
                }
            }
            "user" if v["parent_tool_use_id"].is_null() => {
                for block in v["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        step.events.push(AgentEvent::ToolDone {
                            id: block["tool_use_id"].as_str().unwrap_or("").to_string(),
                            is_error: block["is_error"].as_bool().unwrap_or(false),
                        });
                    }
                }
            }
            "result" => {
                let u = &v["usage"];
                let n = |k: &str| u[k].as_u64().unwrap_or(0);
                step.events.push(AgentEvent::Result {
                    is_error: v["is_error"].as_bool().unwrap_or(false),
                    duration_ms: v["duration_ms"].as_u64().unwrap_or(0),
                    cost_usd: v["total_cost_usd"].as_f64().unwrap_or(0.0),
                    input_tokens: n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens"),
                    output_tokens: n("output_tokens"),
                    errors: v["errors"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|e| e.as_str().map(str::to_string)).collect())
                        .unwrap_or_default(),
                });
            }
            _ => {}
        }
        step
    }

    fn on_control_request(&mut self, v: &Value, step: &mut Step) {
        let request_id = v["request_id"].as_str().unwrap_or("").to_string();
        let req = &v["request"];
        match req["subtype"].as_str().unwrap_or("") {
            "mcp_message" => {
                let msg = &req["message"];
                let rpc_id = msg["id"].clone();
                let method = msg["method"].as_str().unwrap_or("");
                if rpc_id.is_null() {
                    // Notification JSON-RPC : rien à répondre au serveur, juste accuser réception.
                    step.out.push(control_success(
                        &request_id,
                        json!({ "mcp_response": { "jsonrpc": "2.0", "result": {}, "id": 0 } }),
                    ));
                    return;
                }
                let result = match method {
                    "initialize" => json!({
                        "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": tools::SERVER, "version": env!("CARGO_PKG_VERSION") },
                    }),
                    "tools/list" => json!({ "tools": tools::definitions() }),
                    "ping" => json!({}),
                    "tools/call" => {
                        let id = self.next();
                        self.tools.insert(id, PendingTool { request_id, rpc_id });
                        step.events.push(AgentEvent::ToolCall {
                            id,
                            tool: msg["params"]["name"].as_str().unwrap_or("").to_string(),
                            args: msg["params"]["arguments"].clone(),
                        });
                        return;
                    }
                    _ => {
                        let rpc = json!({
                            "jsonrpc": "2.0", "id": rpc_id,
                            "error": { "code": -32601, "message": format!("method not found: {method}") },
                        });
                        step.out.push(control_success(&request_id, json!({ "mcp_response": rpc })));
                        return;
                    }
                };
                let rpc = json!({ "jsonrpc": "2.0", "id": rpc_id, "result": result });
                step.out.push(control_success(&request_id, json!({ "mcp_response": rpc })));
            }
            "can_use_tool" => {
                let id = self.next();
                self.permissions.insert(
                    id,
                    PendingPermission {
                        request_id,
                        tool_use_id: req["tool_use_id"].clone(),
                        input: req["input"].clone(),
                    },
                );
                step.events.push(AgentEvent::Permission {
                    id,
                    tool: req["tool_name"].as_str().unwrap_or("").to_string(),
                    input: req["input"].clone(),
                    reason: req["decision_reason"].as_str().map(str::to_string),
                    tool_use_id: req["tool_use_id"].as_str().map(str::to_string),
                });
            }
            other => {
                step.out.push(
                    json!({
                        "type": "control_response",
                        "response": {
                            "subtype": "error", "request_id": request_id,
                            "error": format!("vibelearn does not handle control request `{other}`"),
                        },
                    })
                    .to_string(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Value {
        serde_json::from_str(line).unwrap()
    }

    #[test]
    fn serves_the_in_memory_tool_server_handshake() {
        let mut w = Wire::new();
        // Lignes telles qu'émises par claude 2.1.326.
        let step = w.on_line(r#"{"type":"control_request","request_id":"r1","request":{"subtype":"mcp_message","server_name":"tutor","message":{"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{}},"jsonrpc":"2.0","id":"d36d9a7d:0"}}}"#);
        let resp = parse(&step.out[0]);
        assert_eq!(resp["response"]["request_id"], "r1");
        let rpc = &resp["response"]["response"]["mcp_response"];
        assert_eq!(rpc["id"], "d36d9a7d:0");
        assert_eq!(rpc["result"]["protocolVersion"], "2025-11-25");

        let step = w.on_line(r#"{"type":"control_request","request_id":"r2","request":{"subtype":"mcp_message","server_name":"tutor","message":{"jsonrpc":"2.0","method":"notifications/initialized"}}}"#);
        assert_eq!(parse(&step.out[0])["response"]["response"]["mcp_response"]["id"], 0);

        let step = w.on_line(r#"{"type":"control_request","request_id":"r3","request":{"subtype":"mcp_message","server_name":"tutor","message":{"method":"tools/list","jsonrpc":"2.0","id":"d36d9a7d:1"}}}"#);
        let tools = &parse(&step.out[0])["response"]["response"]["mcp_response"]["result"]["tools"];
        assert_eq!(tools.as_array().unwrap().len(), 11);
    }

    #[test]
    fn tool_calls_wait_for_the_app_then_answer_the_right_request() {
        let mut w = Wire::new();
        let step = w.on_line(r#"{"type":"control_request","request_id":"r9","request":{"subtype":"mcp_message","server_name":"tutor","message":{"method":"tools/call","params":{"name":"ask","arguments":{"kind":"free","question":"Pourquoi ?"}},"jsonrpc":"2.0","id":"x:7"}}}"#);
        assert!(step.out.is_empty(), "la réponse attend l'apprenant");
        let AgentEvent::ToolCall { id, tool, args } = &step.events[0] else { panic!() };
        assert_eq!((tool.as_str(), args["question"].as_str()), ("ask", Some("Pourquoi ?")));

        let out = w.on_cmd(Cmd::ToolReply { id: *id, text: "{\"answer\":\"parce que\"}".into(), is_error: false }).unwrap();
        let resp = parse(&out);
        assert_eq!(resp["response"]["request_id"], "r9");
        let rpc = &resp["response"]["response"]["mcp_response"];
        assert_eq!(rpc["id"], "x:7");
        assert_eq!(rpc["result"]["content"][0]["text"], "{\"answer\":\"parce que\"}");
        // Une seconde réponse au même appel est ignorée.
        assert!(w.on_cmd(Cmd::ToolReply { id: *id, text: String::new(), is_error: false }).is_none());
    }

    #[test]
    fn permission_requests_round_trip() {
        let mut w = Wire::new();
        let line = r#"{"type":"control_request","request_id":"p1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"cargo test"},"tool_use_id":"toolu_1","decision_reason":"needs approval"}}"#;
        let step = w.on_line(line);
        let AgentEvent::Permission { id, tool, .. } = &step.events[0] else { panic!() };
        assert_eq!(tool, "Bash");
        let allow = parse(&w.on_cmd(Cmd::PermissionReply { id: *id, allow: true, message: None }).unwrap());
        assert_eq!(allow["response"]["response"]["behavior"], "allow");
        assert_eq!(allow["response"]["response"]["updatedInput"]["command"], "cargo test");
        assert_eq!(allow["response"]["response"]["toolUseID"], "toolu_1");

        let step = w.on_line(line);
        let AgentEvent::Permission { id, .. } = &step.events[0] else { panic!() };
        let deny = parse(&w.on_cmd(Cmd::PermissionReply { id: *id, allow: false, message: Some("non".into()) }).unwrap());
        assert_eq!(deny["response"]["response"]["behavior"], "deny");
        assert_eq!(deny["response"]["response"]["message"], "non");
    }

    #[test]
    fn detects_a_missing_login_from_the_initialize_response() {
        let mut w = Wire::new();
        let step = w.on_line(r#"{"type":"control_response","response":{"subtype":"success","request_id":"vibelearn-init","response":{"account":{"tokenSource":"none","apiProvider":"firstParty"}}}}"#);
        assert_eq!(step.events, vec![AgentEvent::AuthRequired]);
        let step = w.on_line(r#"{"type":"control_response","response":{"subtype":"success","request_id":"vibelearn-init","response":{"account":{"tokenSource":"claude.ai"}}}}"#);
        assert_eq!(step.events, vec![AgentEvent::Ready]);
    }

    #[test]
    fn translates_streaming_and_result_messages() {
        let mut w = Wire::new();
        let step = w.on_line(r#"{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Sal"}}}"#);
        assert_eq!(step.events, vec![AgentEvent::Delta { text: "Sal".into() }]);
        // Le début d'un appel d'outil porte son nom : l'interface peut dire ce qui se prépare.
        let step = w.on_line(r#"{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"mcp__tutor__knowledge_checklist","input":{}}}}"#);
        assert_eq!(
            step.events,
            vec![AgentEvent::BlockStart { kind: "tool_use".into(), name: Some("mcp__tutor__knowledge_checklist".into()) }]
        );
        // Les sous-agents ne polluent pas le fil principal.
        let step = w.on_line(r#"{"type":"stream_event","parent_tool_use_id":"t1","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"x"}}}"#);
        assert!(step.events.is_empty());

        let step = w.on_line(r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1200,"total_cost_usd":0.02,"usage":{"input_tokens":10,"cache_read_input_tokens":90,"output_tokens":7}}"#);
        let AgentEvent::Result { input_tokens, output_tokens, is_error, .. } = &step.events[0] else { panic!() };
        assert_eq!((*input_tokens, *output_tokens, *is_error), (100, 7, false));
    }

    #[test]
    fn user_messages_use_the_streaming_input_shape() {
        let mut w = Wire::new();
        let line = parse(&w.on_cmd(Cmd::User("salut".into())).unwrap());
        assert_eq!(line["type"], "user");
        assert_eq!(line["message"]["content"], "salut");
        assert!(line["parent_tool_use_id"].is_null());
    }
}
