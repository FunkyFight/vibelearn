//! Moteur sans binaire : vibelearn tient lui-même la conversation avec une API :
//! Anthropic Messages (l'officielle ou un service compatible), OpenAI Chat
//! Completions (compatible avec la plupart des services) ou OpenAI Responses
//! (celle des modèles Codex). Il expose au modèle les outils du
//! tuteur et ceux de `localtools`, et rapporte les mêmes `AgentEvent` que le
//! moteur `claude` : l'application ne fait pas la différence.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

use crate::claude::{AgentEvent, Cmd, Emit};
use crate::i18n::tr;
use crate::localtools::{self, Access};
use crate::{paths, tools};

/// Tours modèle → outils → modèle au plus, dans un même tour de l'apprenant.
const MAX_ROUNDS: usize = 80;
const RETRIES: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dialect {
    /// `POST /chat/completions`
    Chat,
    /// `POST /responses`
    Responses,
    /// `POST /v1/messages` (API Anthropic ou compatible)
    Anthropic,
}

/// Version d'API envoyée à Anthropic ; les services compatibles ne connaissent souvent que la première.
const ANTHROPIC_VERSION: &str = "2026-10-21";
const ANTHROPIC_VERSION_COMPAT: &str = "2023-06-01";
/// Plafond de sortie par réponse (en flux, pas de délai HTTP à craindre) ; repli si le service le refuse.
const ANTHROPIC_MAX_TOKENS: u64 = 64_000;
const ANTHROPIC_MAX_TOKENS_FALLBACK: u64 = 8_192;

/// Adresse d'un point d'accès Anthropic : `https://api.anthropic.com` + `/v1/messages`,
/// sans doubler `/v1` si l'URL donnée le contient déjà.
pub fn anthropic_endpoint(url: &str, path: &str) -> String {
    let url = crate::login::trim_url(url);
    if url.ends_with("/v1") { format!("{url}/{path}") } else { format!("{url}/v1/{path}") }
}

pub struct ApiConfig {
    pub dialect: Dialect,
    pub url: String,
    pub key: String,
    pub model: String,
    pub effort: Option<String>,
    pub cwd: PathBuf,
    pub system_prompt: String,
    pub resume: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    dialect: Dialect,
    model: String,
    items: Vec<Value>,
}

/// Message d'erreur lisible tiré d'une réponse d'API.
pub fn error_message(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let msg = v["error"]["message"].as_str().or_else(|| v["error"].as_str()).or_else(|| v["message"].as_str()).or_else(|| v["detail"].as_str());
    let text = msg.map(str::to_string).unwrap_or_else(|| body.trim().to_string());
    if text.chars().count() > 400 { text.chars().take(400).collect::<String>() + "…" } else { text }
}

/// Nom de l'outil tel que l'application le connaît (`mcp__tutor__ask`, `Read`…).
fn display_name(name: &str) -> String {
    if localtools::access(name).is_some() { name.to_string() } else { format!("mcp__{}__{name}", tools::SERVER) }
}

fn is_tutor_tool(name: &str) -> bool {
    tools::definitions().iter().any(|t| t["name"] == name)
}

/// Les blocs de raisonnement d'une conversation reprise : le prompt système a pu
/// changer depuis (date du jour, version installée), et un bloc rejoué après un
/// tel changement est refusé. On les retire une fois, au chargement.
fn strip_thinking(items: &mut [Value]) {
    for item in items.iter_mut().filter(|i| i["role"] == "assistant") {
        if let Some(blocks) = item["content"].as_array_mut() {
            blocks.retain(|b| !matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking")));
        }
    }
}

/// Effort de vibelearn → effort des API OpenAI (low, medium, high).
fn openai_effort(effort: &str) -> &'static str {
    match effort {
        "low" => "low",
        "high" | "xhigh" | "max" => "high",
        _ => "medium",
    }
}

fn tool_specs(dialect: Dialect) -> Vec<Value> {
    let mut specs: Vec<(String, String, Value)> = tools::definitions()
        .into_iter()
        .map(|t| (t["name"].as_str().unwrap_or("").to_string(), t["description"].as_str().unwrap_or("").to_string(), t["inputSchema"].clone()))
        .collect();
    specs.extend(localtools::definitions().into_iter().map(|(n, d, s)| (n.to_string(), d, s)));
    specs
        .into_iter()
        .map(|(name, description, parameters)| match dialect {
            Dialect::Chat => json!({ "type": "function", "function": { "name": name, "description": description, "parameters": parameters } }),
            // L'API Responses rend les schémas stricts par défaut : les nôtres ont des champs facultatifs.
            Dialect::Responses => json!({ "type": "function", "name": name, "description": description, "parameters": parameters, "strict": false }),
            Dialect::Anthropic => json!({ "name": name, "description": description, "input_schema": parameters }),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Lecture des flux (sans E/S : testable)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
struct Call {
    id: String,
    name: String,
    arguments: String,
}

/// Ce qu'un appel au modèle a produit.
#[derive(Debug, Default)]
struct Streamed {
    text: String,
    calls: Vec<Call>,
    /// Éléments à rajouter tels quels à l'historique (API Responses).
    output: Vec<Value>,
    input_tokens: u64,
    output_tokens: u64,
    error: Option<String>,
}

#[derive(Default)]
struct Reader {
    dialect: Option<Dialect>,
    out: Streamed,
    text_started: bool,
    thinking_started: bool,
    /// Chat : (index du flux, appel en cours de réception).
    partial: Vec<(u64, Call)>,
    /// Anthropic : les blocs du message, et le JSON des appels d'outils reçu par morceaux.
    blocks: Vec<(u64, Value, String)>,
}

impl Reader {
    fn new(dialect: Dialect) -> Self {
        Self { dialect: Some(dialect), ..Self::default() }
    }

    /// Une ligne `data: …` du flux SSE.
    fn on_data(&mut self, data: &str, events: &mut Vec<AgentEvent>) {
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else { return };
        match self.dialect {
            Some(Dialect::Chat) => self.on_chat(&v, events),
            Some(Dialect::Responses) => self.on_responses(&v, events),
            Some(Dialect::Anthropic) => self.on_anthropic(&v, events),
            None => {}
        }
    }

    fn start_text(&mut self, events: &mut Vec<AgentEvent>) {
        if !self.text_started {
            self.text_started = true;
            events.push(AgentEvent::BlockStart { kind: "text".into(), name: None });
        }
    }

    fn start_thinking(&mut self, events: &mut Vec<AgentEvent>) {
        if !self.thinking_started {
            self.thinking_started = true;
            events.push(AgentEvent::BlockStart { kind: "thinking".into(), name: None });
        }
    }

    fn on_chat(&mut self, v: &Value, events: &mut Vec<AgentEvent>) {
        if let Some(msg) = v["error"]["message"].as_str().or_else(|| v["error"].as_str()) {
            self.out.error = Some(msg.to_string());
            return;
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.out.input_tokens = u["prompt_tokens"].as_u64().unwrap_or(self.out.input_tokens);
            self.out.output_tokens = u["completion_tokens"].as_u64().unwrap_or(self.out.output_tokens);
        }
        let Some(choice) = v["choices"].get(0) else { return };
        let delta = &choice["delta"];
        let reasoning = delta["reasoning_content"].as_str().or_else(|| delta["reasoning"].as_str());
        if reasoning.is_some_and(|r| !r.is_empty()) {
            self.start_thinking(events);
        }
        if let Some(text) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            self.start_text(events);
            self.out.text.push_str(text);
            events.push(AgentEvent::Delta { text: text.to_string() });
        }
        for (pos, tc) in delta["tool_calls"].as_array().into_iter().flatten().enumerate() {
            let index = tc["index"].as_u64().unwrap_or(pos as u64);
            let slot = match self.partial.iter().position(|(i, _)| *i == index) {
                Some(s) => s,
                None => {
                    self.partial.push((index, Call { id: String::new(), name: String::new(), arguments: String::new() }));
                    self.partial.len() - 1
                }
            };
            let call = &mut self.partial[slot].1;
            if let Some(id) = tc["id"].as_str().filter(|s| !s.is_empty()) {
                call.id = id.to_string();
            }
            if let Some(name) = tc["function"]["name"].as_str().filter(|s| !s.is_empty()) {
                let first = call.name.is_empty();
                call.name.push_str(name);
                if first {
                    events.push(AgentEvent::BlockStart { kind: "tool_use".into(), name: Some(display_name(name)) });
                }
            }
            if let Some(args) = tc["function"]["arguments"].as_str() {
                call.arguments.push_str(args);
            }
        }
    }

    fn on_responses(&mut self, v: &Value, events: &mut Vec<AgentEvent>) {
        match v["type"].as_str().unwrap_or("") {
            "response.output_item.added" => match v["item"]["type"].as_str().unwrap_or("") {
                "message" => self.start_text(events),
                "reasoning" => self.start_thinking(events),
                "function_call" => events.push(AgentEvent::BlockStart {
                    kind: "tool_use".into(),
                    name: v["item"]["name"].as_str().map(display_name),
                }),
                _ => {}
            },
            "response.output_text.delta" => {
                if let Some(text) = v["delta"].as_str().filter(|t| !t.is_empty()) {
                    self.start_text(events);
                    self.out.text.push_str(text);
                    events.push(AgentEvent::Delta { text: text.to_string() });
                }
            }
            "response.completed" | "response.incomplete" => {
                let r = &v["response"];
                self.out.input_tokens = r["usage"]["input_tokens"].as_u64().unwrap_or(0);
                self.out.output_tokens = r["usage"]["output_tokens"].as_u64().unwrap_or(0);
                let output = r["output"].as_array().cloned().unwrap_or_default();
                let mut text = String::new();
                for item in &output {
                    match item["type"].as_str().unwrap_or("") {
                        "function_call" => self.out.calls.push(Call {
                            id: item["call_id"].as_str().unwrap_or("").to_string(),
                            name: item["name"].as_str().unwrap_or("").to_string(),
                            arguments: item["arguments"].as_str().unwrap_or("").to_string(),
                        }),
                        "message" => {
                            for part in item["content"].as_array().into_iter().flatten() {
                                if let Some(t) = part["text"].as_str() {
                                    text.push_str(t);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if !text.is_empty() {
                    self.out.text = text;
                }
                self.out.output = output.into_iter().filter_map(history_item).collect();
                if v["type"] == "response.incomplete" && self.out.calls.is_empty() && self.out.text.is_empty() {
                    let why = r["incomplete_details"]["reason"].as_str().unwrap_or("incomplete");
                    self.out.error = Some(format!("response incomplete: {why}"));
                }
            }
            "response.failed" => {
                let msg = v["response"]["error"]["message"].as_str().unwrap_or("the response failed");
                self.out.error = Some(msg.to_string());
            }
            "error" => {
                let msg = v["message"].as_str().or_else(|| v["error"]["message"].as_str()).unwrap_or("stream error");
                self.out.error = Some(msg.to_string());
            }
            _ => {}
        }
    }

    fn on_anthropic(&mut self, v: &Value, events: &mut Vec<AgentEvent>) {
        let index = v["index"].as_u64().unwrap_or(0);
        match v["type"].as_str().unwrap_or("") {
            "message_start" => {
                let u = &v["message"]["usage"];
                let n = |k: &str| u[k].as_u64().unwrap_or(0);
                self.out.input_tokens = n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens");
                self.out.output_tokens = n("output_tokens");
            }
            "content_block_start" => {
                let block = v["content_block"].clone();
                match block["type"].as_str().unwrap_or("") {
                    "text" => self.start_text(events),
                    "thinking" | "redacted_thinking" => self.start_thinking(events),
                    "tool_use" => events.push(AgentEvent::BlockStart {
                        kind: "tool_use".into(),
                        name: block["name"].as_str().map(display_name),
                    }),
                    _ => {}
                }
                self.blocks.push((index, block, String::new()));
            }
            "content_block_delta" => {
                let Some((_, block, json)) = self.blocks.iter_mut().find(|(i, ..)| *i == index) else { return };
                let d = &v["delta"];
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let text = d["text"].as_str().unwrap_or("");
                        let full = format!("{}{text}", block["text"].as_str().unwrap_or(""));
                        block["text"] = json!(full);
                        if !text.is_empty() {
                            self.out.text.push_str(text);
                            events.push(AgentEvent::Delta { text: text.to_string() });
                        }
                    }
                    "input_json_delta" => json.push_str(d["partial_json"].as_str().unwrap_or("")),
                    "thinking_delta" => {
                        let full = format!("{}{}", block["thinking"].as_str().unwrap_or(""), d["thinking"].as_str().unwrap_or(""));
                        block["thinking"] = json!(full);
                    }
                    "signature_delta" => {
                        let full = format!("{}{}", block["signature"].as_str().unwrap_or(""), d["signature"].as_str().unwrap_or(""));
                        block["signature"] = json!(full);
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(n) = v["usage"]["output_tokens"].as_u64() {
                    self.out.output_tokens = n;
                }
                if v["delta"]["stop_reason"] == "refusal" {
                    let why = v["delta"]["stop_details"]["explanation"].as_str().unwrap_or("");
                    self.out.error = Some(format!("the model declined to answer (refusal) {why}").trim().to_string());
                }
            }
            "error" => {
                let msg = v["error"]["message"].as_str().unwrap_or("stream error");
                self.out.error = Some(msg.to_string());
            }
            _ => {}
        }
    }

    fn finish(mut self) -> Streamed {
        if self.dialect == Some(Dialect::Chat) {
            for (n, (_, mut call)) in std::mem::take(&mut self.partial).into_iter().enumerate() {
                if call.id.is_empty() {
                    call.id = format!("call_{n}_{}", now_ms());
                }
                self.out.calls.push(call);
            }
        }
        if self.dialect == Some(Dialect::Anthropic) {
            for (_, mut block, json) in std::mem::take(&mut self.blocks) {
                match block["type"].as_str().unwrap_or("") {
                    "tool_use" => {
                        // L'historique garde un objet valide ; l'appel, lui, reçoit le texte brut (et son erreur s'il est invalide).
                        block["input"] = if json.trim().is_empty() {
                            block["input"].clone()
                        } else {
                            serde_json::from_str(&json).unwrap_or_else(|_| json!({}))
                        };
                        let arguments = if json.trim().is_empty() { block["input"].to_string() } else { json };
                        self.out.calls.push(Call {
                            id: block["id"].as_str().unwrap_or("").to_string(),
                            name: block["name"].as_str().unwrap_or("").to_string(),
                            arguments,
                        });
                    }
                    // Un bloc de texte vide est refusé en entrée.
                    "text" if block["text"].as_str().unwrap_or("").is_empty() => continue,
                    _ => {}
                }
                if let Some(obj) = block.as_object_mut() {
                    obj.remove("citations");
                }
                self.out.output.push(block);
            }
        }
        self.out
    }
}

/// Un élément de sortie de l'API Responses, prêt à être renvoyé en entrée.
/// Sans stockage côté serveur, les identifiants ne renvoient à rien : on les
/// retire, sauf pour un raisonnement chiffré, qui se suffit à lui-même.
fn history_item(mut item: Value) -> Option<Value> {
    let obj = item.as_object_mut()?;
    obj.remove("status");
    if obj.get("type").and_then(Value::as_str) == Some("reasoning") {
        if obj.get("encrypted_content").is_none_or(Value::is_null) {
            return None;
        }
    } else {
        obj.remove("id");
    }
    Some(item)
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

enum Failure {
    /// 401/403 : la clé ne passe plus.
    Auth,
    Http(u16, String),
    Network(String),
}

enum Reply {
    Done(Streamed),
    Failed(Failure),
    Interrupted,
    Shutdown,
}

enum Waited {
    Tool(String, bool),
    Permission(bool, Option<String>),
    Interrupted,
    Shutdown,
}

#[derive(PartialEq)]
enum Flow {
    Continue,
    Exit,
}

struct Session {
    cfg: ApiConfig,
    http: reqwest::Client,
    emit: Emit,
    rx: mpsc::UnboundedReceiver<Cmd>,
    items: Vec<Value>,
    id: String,
    queued: VecDeque<String>,
    seq: u64,
    /// Paramètres facultatifs refusés par le service (on ne les renvoie plus).
    dropped: Vec<&'static str>,
    /// La conversation à reprendre est introuvable : le premier tour échoue
    /// pour que l'application reparte de l'avancement enregistré.
    missing_resume: bool,
    tools: Vec<Value>,
}

pub async fn run(cfg: ApiConfig, rx: mpsc::UnboundedReceiver<Cmd>, emit: Emit) {
    let http = reqwest::Client::builder().connect_timeout(Duration::from_secs(30)).build().unwrap_or_default();
    let mut s = Session {
        tools: tool_specs(cfg.dialect),
        http,
        emit,
        rx,
        items: Vec::new(),
        id: format!("api-{}", now_ms()),
        queued: VecDeque::new(),
        seq: 0,
        dropped: Vec::new(),
        missing_resume: false,
        cfg,
    };
    if let Some(id) = s.cfg.resume.clone() {
        match s.load(&id) {
            Some(items) => {
                s.items = items;
                s.id = id;
            }
            None => s.missing_resume = true,
        }
    }
    (s.emit)(AgentEvent::Session { session_id: s.id.clone(), model: Some(s.cfg.model.clone()) });
    (s.emit)(AgentEvent::Ready);
    loop {
        let text = match s.queued.pop_front() {
            Some(t) => t,
            None => match s.rx.recv().await {
                Some(Cmd::User(t)) => t,
                Some(Cmd::Shutdown) | None => break,
                Some(_) => continue,
            },
        };
        if s.turn(text).await == Flow::Exit {
            break;
        }
    }
    (s.emit)(AgentEvent::Exited { code: Some(0), stderr: String::new() });
}

impl Session {
    fn file(&self, id: &str) -> PathBuf {
        paths::conversations_dir(&self.cfg.cwd).join(format!("{}.json", paths::slug(id)))
    }

    fn load(&self, id: &str) -> Option<Vec<Value>> {
        let saved: Saved = serde_json::from_str(&std::fs::read_to_string(self.file(id)).ok()?).ok()?;
        let mut items = (saved.dialect == self.cfg.dialect).then_some(saved.items)?;
        strip_thinking(&mut items);
        Some(items)
    }

    fn save(&self) {
        let path = self.file(&self.id);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let saved = Saved { dialect: self.cfg.dialect, model: self.cfg.model.clone(), items: self.items.clone() };
        if let Ok(text) = serde_json::to_string(&saved) {
            let _ = std::fs::write(path, text);
        }
    }

    fn next(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn push_user(&mut self, text: &str) {
        self.items.push(json!({ "role": "user", "content": text }));
    }

    fn push_assistant(&mut self, streamed: &Streamed) {
        match self.cfg.dialect {
            Dialect::Chat => {
                let mut msg = json!({ "role": "assistant", "content": streamed.text });
                if !streamed.calls.is_empty() {
                    msg["tool_calls"] = streamed
                        .calls
                        .iter()
                        .map(|c| json!({ "id": c.id, "type": "function", "function": { "name": c.name, "arguments": c.arguments } }))
                        .collect();
                }
                self.items.push(msg);
            }
            Dialect::Responses => {
                if streamed.output.is_empty() && !streamed.text.is_empty() {
                    self.items.push(json!({ "role": "assistant", "content": streamed.text }));
                } else {
                    self.items.extend(streamed.output.iter().cloned());
                }
            }
            // Les blocs tels quels, raisonnement compris : la conversation ne fait que s'allonger.
            Dialect::Anthropic => {
                if !streamed.output.is_empty() {
                    self.items.push(json!({ "role": "assistant", "content": streamed.output }));
                }
            }
        }
    }

    fn push_tool_result(&mut self, call_id: &str, output: &str, is_error: bool) {
        let item = match self.cfg.dialect {
            Dialect::Chat => json!({ "role": "tool", "tool_call_id": call_id, "content": output }),
            Dialect::Responses => json!({ "type": "function_call_output", "call_id": call_id, "output": output }),
            Dialect::Anthropic => {
                let result = json!({ "type": "tool_result", "tool_use_id": call_id, "content": output, "is_error": is_error });
                // Tous les résultats d'un même tour vont dans un seul message.
                if let Some(last) = self.items.last_mut()
                    && last["role"] == "user"
                    && let Some(content) = last["content"].as_array_mut()
                    && content.iter().all(|b| b["type"] == "tool_result")
                {
                    content.push(result);
                    return;
                }
                json!({ "role": "user", "content": [result] })
            }
        };
        self.items.push(item);
    }

    fn body(&self) -> (String, Value) {
        let url = crate::login::trim_url(&self.cfg.url);
        let keep = |p: &str| !self.dropped.contains(&p);
        let effort = self.cfg.effort.as_deref().map(openai_effort);
        match self.cfg.dialect {
            Dialect::Chat => {
                let mut messages = vec![json!({ "role": "system", "content": self.cfg.system_prompt })];
                messages.extend(self.items.iter().cloned());
                let mut body = json!({ "model": self.cfg.model, "messages": messages, "tools": self.tools, "stream": true });
                if keep("stream_options") {
                    body["stream_options"] = json!({ "include_usage": true });
                }
                if let (Some(e), true) = (effort, keep("reasoning_effort")) {
                    body["reasoning_effort"] = json!(e);
                }
                (format!("{url}/chat/completions"), body)
            }
            Dialect::Responses => {
                let mut body = json!({
                    "model": self.cfg.model,
                    "instructions": self.cfg.system_prompt,
                    "input": self.items,
                    "tools": self.tools,
                    "stream": true,
                    "store": false,
                });
                // Sans stockage, le raisonnement ne se transmet d'un appel à l'autre que chiffré.
                if let (Some(e), true) = (effort, keep("reasoning")) {
                    body["reasoning"] = json!({ "effort": e });
                    if keep("include") {
                        body["include"] = json!(["reasoning.encrypted_content"]);
                    }
                }
                (format!("{url}/responses"), body)
            }
            Dialect::Anthropic => {
                let max_tokens = if keep("max_tokens") { ANTHROPIC_MAX_TOKENS } else { ANTHROPIC_MAX_TOKENS_FALLBACK };
                let mut body = json!({
                    "model": self.cfg.model,
                    "max_tokens": max_tokens,
                    "system": self.cfg.system_prompt,
                    "messages": self.items,
                    "tools": self.tools,
                    "stream": true,
                });
                if keep("thinking") {
                    body["thinking"] = json!({ "type": "adaptive" });
                }
                if let (Some(e), true) = (self.cfg.effort.as_deref(), keep("output_config")) {
                    body["output_config"] = json!({ "effort": e });
                }
                // Cache automatique sur le dernier bloc : système, outils et historique ne changent pas d'un tour à l'autre.
                if keep("cache_control") {
                    body["cache_control"] = json!({ "type": "ephemeral" });
                }
                (anthropic_endpoint(&url, "messages"), body)
            }
        }
    }

    fn post(&self, url: &str, body: &Value) -> reqwest::RequestBuilder {
        let req = self.http.post(url).json(body);
        match self.cfg.dialect {
            Dialect::Anthropic => {
                if crate::login::is_official_anthropic(&self.cfg.url) {
                    req.header("x-api-key", &self.cfg.key).header("anthropic-version", ANTHROPIC_VERSION)
                } else {
                    // Les services compatibles prennent la clé en `x-api-key` ou en jeton Bearer selon les cas.
                    req.header("x-api-key", &self.cfg.key).bearer_auth(&self.cfg.key).header("anthropic-version", ANTHROPIC_VERSION_COMPAT)
                }
            }
            _ => req.bearer_auth(&self.cfg.key),
        }
    }

    /// Paramètre facultatif que le service vient de refuser, s'il y en a un.
    fn refused_param(&self, body: &Value, error: &str) -> Option<&'static str> {
        let error = error.to_lowercase();
        // (paramètre, mot qui le désigne dans le message d'erreur)
        [
            ("reasoning_effort", "reasoning"),
            ("stream_options", "stream_options"),
            ("reasoning", "reasoning"),
            ("include", "include"),
            ("output_config", "effort"),
            ("output_config", "output_config"),
            ("thinking", "thinking"),
            ("cache_control", "cache_control"),
            ("max_tokens", "max_tokens"),
        ]
        .into_iter()
        .find(|(p, word)| body.get(*p).is_some() && !self.dropped.contains(p) && error.contains(word))
        .map(|(p, _)| p)
    }

    /// Un appel au modèle, interrompable.
    async fn request(&mut self) -> Reply {
        loop {
            let (url, body) = self.body();
            let req = self.post(&url, &body);
            let fut = stream(req, self.cfg.dialect, self.emit.clone());
            tokio::pin!(fut);
            let result = loop {
                tokio::select! {
                    r = &mut fut => break r,
                    cmd = self.rx.recv() => match cmd {
                        Some(Cmd::Interrupt) => return Reply::Interrupted,
                        Some(Cmd::Shutdown) | None => return Reply::Shutdown,
                        Some(Cmd::User(t)) => self.queued.push_back(t),
                        Some(_) => {}
                    },
                }
            };
            match result {
                Err(Failure::Http(400, error)) => match self.refused_param(&body, &error) {
                    Some(p) => self.dropped.push(p),
                    None => return Reply::Failed(Failure::Http(400, error)),
                },
                Err(f) => return Reply::Failed(f),
                Ok(s) => return Reply::Done(s),
            }
        }
    }

    async fn wait(&mut self, id: u64) -> Waited {
        loop {
            match self.rx.recv().await {
                Some(Cmd::ToolReply { id: i, text, is_error }) if i == id => return Waited::Tool(text, is_error),
                Some(Cmd::PermissionReply { id: i, allow, message }) if i == id => return Waited::Permission(allow, message),
                Some(Cmd::Interrupt) => return Waited::Interrupted,
                Some(Cmd::Shutdown) | None => return Waited::Shutdown,
                Some(Cmd::User(t)) => self.queued.push_back(t),
                Some(_) => {}
            }
        }
    }

    /// Lance un outil de fichiers, interrompable. `Err` : le tour s'arrête (interruption ou arrêt).
    async fn run_local(&mut self, name: &str, input: &Value) -> Result<(String, bool), Flow> {
        let root = self.cfg.cwd.clone();
        let fut = localtools::run(&root, name, input);
        tokio::pin!(fut);
        loop {
            tokio::select! {
                r = &mut fut => return Ok(match r { Ok(out) => (out, false), Err(e) => (e, true) }),
                cmd = self.rx.recv() => match cmd {
                    Some(Cmd::Interrupt) => return Err(Flow::Continue),
                    Some(Cmd::Shutdown) | None => return Err(Flow::Exit),
                    Some(Cmd::User(t)) => self.queued.push_back(t),
                    Some(_) => {}
                },
            }
        }
    }

    /// Exécute un appel d'outil du modèle. `Err` : le tour s'arrête là.
    async fn call(&mut self, call: &Call) -> Result<(String, bool), Flow> {
        let input: Value = if call.arguments.trim().is_empty() {
            json!({})
        } else {
            match serde_json::from_str(&call.arguments) {
                Ok(v) => v,
                Err(e) => return Ok((format!("The tool arguments were not valid JSON ({e}). Call the tool again with a JSON object."), true)),
            }
        };
        (self.emit)(AgentEvent::ToolUse { id: call.id.clone(), name: display_name(&call.name), input: input.clone() });
        match localtools::access(&call.name) {
            Some(Access::Free) => self.run_local(&call.name, &input).await,
            Some(Access::Ask) => {
                let id = self.next();
                (self.emit)(AgentEvent::Permission {
                    id,
                    tool: call.name.clone(),
                    input: input.clone(),
                    reason: None,
                    tool_use_id: Some(call.id.clone()),
                });
                match self.wait(id).await {
                    Waited::Permission(true, _) => self.run_local(&call.name, &input).await,
                    Waited::Permission(false, message) => Ok((message.unwrap_or_else(|| "Denied by the learner.".into()), true)),
                    Waited::Interrupted => {
                        (self.emit)(AgentEvent::Cancel { id });
                        Err(Flow::Continue)
                    }
                    Waited::Shutdown => Err(Flow::Exit),
                    Waited::Tool(..) => unreachable!("wait ne rend que la réponse attendue"),
                }
            }
            None if is_tutor_tool(&call.name) => {
                let id = self.next();
                (self.emit)(AgentEvent::ToolCall { id, tool: call.name.clone(), args: input });
                match self.wait(id).await {
                    Waited::Tool(text, is_error) => Ok((text, is_error)),
                    Waited::Interrupted => {
                        (self.emit)(AgentEvent::Cancel { id });
                        Err(Flow::Continue)
                    }
                    Waited::Shutdown => Err(Flow::Exit),
                    Waited::Permission(..) => unreachable!("wait ne rend que la réponse attendue"),
                }
            }
            None => Ok((format!("There is no tool named `{}`.", call.name), true)),
        }
    }

    fn result(&self, start: Instant, tokens: (u64, u64), error: Option<String>) {
        (self.emit)(AgentEvent::Result {
            is_error: error.is_some(),
            duration_ms: start.elapsed().as_millis() as u64,
            cost_usd: 0.0,
            input_tokens: tokens.0,
            output_tokens: tokens.1,
            errors: error.into_iter().collect(),
        });
    }

    async fn turn(&mut self, text: String) -> Flow {
        let start = Instant::now();
        if std::mem::take(&mut self.missing_resume) {
            self.result(start, (0, 0), Some("the previous conversation was not found".into()));
            return Flow::Continue;
        }
        self.push_user(&text);
        let mut tokens = (0u64, 0u64);
        for _ in 0..MAX_ROUNDS {
            let streamed = match self.request().await {
                Reply::Done(s) => s,
                Reply::Interrupted => {
                    self.save();
                    self.result(start, tokens, None);
                    return Flow::Continue;
                }
                Reply::Shutdown => {
                    self.save();
                    return Flow::Exit;
                }
                Reply::Failed(Failure::Auth) => {
                    self.save();
                    (self.emit)(AgentEvent::AuthRequired);
                    return Flow::Continue;
                }
                Reply::Failed(Failure::Http(status, error)) => {
                    self.save();
                    self.result(start, tokens, Some(format!("{status} : {error}")));
                    return Flow::Continue;
                }
                Reply::Failed(Failure::Network(error)) => {
                    self.save();
                    self.result(start, tokens, Some(error));
                    return Flow::Continue;
                }
            };
            tokens.0 += streamed.input_tokens;
            tokens.1 += streamed.output_tokens;
            if let Some(error) = streamed.error.clone() {
                self.save();
                self.result(start, tokens, Some(error));
                return Flow::Continue;
            }
            self.push_assistant(&streamed);
            if !streamed.text.trim().is_empty() {
                (self.emit)(AgentEvent::Text { text: streamed.text.clone() });
            }
            if streamed.calls.is_empty() {
                self.save();
                self.result(start, tokens, None);
                return Flow::Continue;
            }
            let calls = streamed.calls;
            for (n, call) in calls.iter().enumerate() {
                match self.call(call).await {
                    Ok((output, is_error)) => {
                        let output = if is_error && !output.starts_with("Error") { format!("Error: {output}") } else { output };
                        self.push_tool_result(&call.id, &output, is_error);
                        (self.emit)(AgentEvent::ToolDone { id: call.id.clone(), is_error });
                    }
                    Err(flow) => {
                        // L'historique doit répondre à chaque appel, même interrompu.
                        for rest in &calls[n..] {
                            self.push_tool_result(&rest.id, "Interrupted: the developer stopped this turn.", true);
                        }
                        self.save();
                        if flow == Flow::Continue {
                            self.result(start, tokens, None);
                        }
                        return flow;
                    }
                }
            }
            self.save();
        }
        self.result(start, tokens, Some(format!("stopped after {MAX_ROUNDS} tool rounds in one turn")));
        Flow::Continue
    }
}

/// Envoie la requête (avec quelques nouvelles tentatives) et lit le flux.
async fn stream(req: reqwest::RequestBuilder, dialect: Dialect, emit: Emit) -> Result<Streamed, Failure> {
    let mut attempt = 0;
    let mut response = loop {
        let Some(r) = req.try_clone() else { return Err(Failure::Network("request could not be built".into())) };
        match r.send().await {
            Ok(resp) if resp.status().is_success() => break resp,
            Ok(resp) => {
                let status = resp.status().as_u16();
                let body = resp.text().await.unwrap_or_default();
                if status == 401 || status == 403 {
                    return Err(Failure::Auth);
                }
                if (status == 429 || status >= 500) && attempt < RETRIES {
                    attempt += 1;
                    emit(AgentEvent::Status { text: tr("nouvelle tentative de connexion à l'API…", "retrying the API…").to_string() });
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                    continue;
                }
                return Err(Failure::Http(status, error_message(&body)));
            }
            Err(e) if attempt < RETRIES && (e.is_connect() || e.is_timeout()) => {
                attempt += 1;
                emit(AgentEvent::Status { text: tr("nouvelle tentative de connexion à l'API…", "retrying the API…").to_string() });
                tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
            }
            Err(e) => return Err(Failure::Network(format!("{} : {e}", tr("API injoignable", "API unreachable")))),
        }
    };

    let mut reader = Reader::new(dialect);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(c)) => c,
            Ok(None) => break,
            Err(e) => return Err(Failure::Network(format!("{} : {e}", tr("flux interrompu", "stream interrupted")))),
        };
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            if let Some(data) = line.trim_end_matches(['\r', '\n']).strip_prefix("data:") {
                let mut events = Vec::new();
                reader.on_data(data, &mut events);
                for e in events {
                    emit(e);
                }
            }
        }
    }
    if let Some(data) = String::from_utf8_lossy(&buf).trim().strip_prefix("data:") {
        let mut events = Vec::new();
        reader.on_data(data, &mut events);
        for e in events {
            emit(e);
        }
    }
    Ok(reader.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(dialect: Dialect, lines: &[&str]) -> (Streamed, Vec<AgentEvent>) {
        let mut r = Reader::new(dialect);
        let mut events = Vec::new();
        for l in lines {
            r.on_data(l, &mut events);
        }
        (r.finish(), events)
    }

    #[test]
    fn chat_streams_text_and_assembles_tool_calls() {
        let (out, events) = feed(
            Dialect::Chat,
            &[
                r#"{"choices":[{"delta":{"role":"assistant","content":"Je "}}]}"#,
                r#"{"choices":[{"delta":{"content":"regarde."}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"Read","arguments":""}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"file_path\":"}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"src/main.rs\"}"}}]}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"ask","arguments":"{}"}}]}}]}"#,
                r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
                r#"{"choices":[],"usage":{"prompt_tokens":120,"completion_tokens":30}}"#,
                "[DONE]",
            ],
        );
        assert_eq!(out.text, "Je regarde.");
        assert_eq!(out.calls[0], Call { id: "call_a".into(), name: "Read".into(), arguments: r#"{"file_path":"src/main.rs"}"#.into() });
        assert_eq!(out.calls[1].name, "ask");
        assert_eq!((out.input_tokens, out.output_tokens), (120, 30));
        assert_eq!(events[0], AgentEvent::BlockStart { kind: "text".into(), name: None });
        assert_eq!(events[1], AgentEvent::Delta { text: "Je ".into() });
        assert!(events.contains(&AgentEvent::BlockStart { kind: "tool_use".into(), name: Some("mcp__tutor__ask".into()) }));
        assert!(events.contains(&AgentEvent::BlockStart { kind: "tool_use".into(), name: Some("Read".into()) }));
    }

    #[test]
    fn responses_streams_text_and_keeps_output_items_without_ids() {
        let (out, events) = feed(
            Dialect::Responses,
            &[
                r#"{"type":"response.output_item.added","item":{"type":"reasoning","id":"rs_1"}}"#,
                r#"{"type":"response.output_item.added","item":{"type":"message","id":"msg_1"}}"#,
                r#"{"type":"response.output_text.delta","delta":"Salut"}"#,
                r#"{"type":"response.output_item.added","item":{"type":"function_call","name":"comment"}}"#,
                r#"{"type":"response.completed","response":{"usage":{"input_tokens":50,"output_tokens":9},"output":[
                    {"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"gAAA"},
                    {"type":"reasoning","id":"rs_2","summary":[]},
                    {"type":"message","id":"msg_1","status":"completed","role":"assistant","content":[{"type":"output_text","text":"Salut","annotations":[]}]},
                    {"type":"function_call","id":"fc_1","call_id":"call_9","name":"comment","arguments":"{\"line\":3}","status":"completed"}]}}"#,
            ],
        );
        assert_eq!(out.text, "Salut");
        assert_eq!(out.calls, vec![Call { id: "call_9".into(), name: "comment".into(), arguments: "{\"line\":3}".into() }]);
        assert_eq!(out.output.len(), 3, "le raisonnement sans contenu chiffré est écarté");
        assert_eq!(out.output[0]["id"], "rs_1");
        assert!(out.output[1].get("id").is_none() && out.output[2].get("id").is_none() && out.output[2].get("status").is_none());
        assert_eq!(events[0], AgentEvent::BlockStart { kind: "thinking".into(), name: None });
        assert!(events.contains(&AgentEvent::BlockStart { kind: "tool_use".into(), name: Some("mcp__tutor__comment".into()) }));
    }

    #[test]
    fn anthropic_streams_text_tools_and_keeps_thinking_blocks() {
        let (out, events) = feed(
            Dialect::Anthropic,
            &[
                r#"{"type":"message_start","message":{"usage":{"input_tokens":10,"cache_read_input_tokens":90,"output_tokens":1}}}"#,
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQB"}}"#,
                r#"{"type":"content_block_stop","index":0}"#,
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Je "}}"#,
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"lis."}}"#,
                r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_1","name":"Read","input":{}}}"#,
                r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"file_path\":"}}"#,
                r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"src/main.rs\"}"}}"#,
                r#"{"type":"content_block_start","index":3,"content_block":{"type":"text","text":""}}"#,
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":42}}"#,
                r#"{"type":"message_stop"}"#,
            ],
        );
        assert_eq!(out.text, "Je lis.");
        assert_eq!(out.calls, vec![Call { id: "toolu_1".into(), name: "Read".into(), arguments: r#"{"file_path":"src/main.rs"}"#.into() }]);
        assert_eq!((out.input_tokens, out.output_tokens), (100, 42));
        assert_eq!(out.output.len(), 3, "le bloc de texte vide est écarté");
        assert_eq!(out.output[0], json!({"type":"thinking","thinking":"","signature":"EqQB"}));
        assert_eq!(out.output[2]["input"]["file_path"], "src/main.rs");
        assert_eq!(events[0], AgentEvent::BlockStart { kind: "thinking".into(), name: None });
        assert!(events.contains(&AgentEvent::BlockStart { kind: "tool_use".into(), name: Some("Read".into()) }));

        let (out, _) = feed(Dialect::Anthropic, &[r#"{"type":"message_delta","delta":{"stop_reason":"refusal"}}"#]);
        assert!(out.error.unwrap().contains("refusal"));
        assert_eq!(anthropic_endpoint("https://api.anthropic.com/", "messages"), "https://api.anthropic.com/v1/messages");
        assert_eq!(anthropic_endpoint("http://localhost:4000/v1", "models"), "http://localhost:4000/v1/models");

        let mut items = vec![json!({"role":"assistant","content":[{"type":"thinking","thinking":"","signature":"x"},{"type":"text","text":"ok"}]})];
        strip_thinking(&mut items);
        assert_eq!(items[0]["content"], json!([{"type":"text","text":"ok"}]));
    }

    #[test]
    fn stream_errors_are_reported() {
        let (out, _) = feed(Dialect::Responses, &[r#"{"type":"response.failed","response":{"error":{"message":"quota exceeded"}}}"#]);
        assert_eq!(out.error.as_deref(), Some("quota exceeded"));
        let (out, _) = feed(Dialect::Chat, &[r#"{"error":{"message":"model not found"}}"#]);
        assert_eq!(out.error.as_deref(), Some("model not found"));
        assert_eq!(error_message(r#"{"error":{"message":"Incorrect API key"}}"#), "Incorrect API key");
    }

    #[test]
    fn every_tool_is_offered_with_its_schema() {
        let chat = tool_specs(Dialect::Chat);
        assert_eq!(chat.len(), tools::definitions().len() + localtools::definitions().len());
        assert!(chat.iter().any(|t| t["function"]["name"] == "start_exercise" && t["function"]["parameters"]["type"] == "object"));
        let responses = tool_specs(Dialect::Responses);
        assert!(responses.iter().all(|t| t["strict"] == false && t["name"].is_string()));
        assert!(tool_specs(Dialect::Anthropic).iter().all(|t| t["input_schema"]["type"] == "object" && t.get("type").is_none()));
        assert_eq!(openai_effort("max"), "high");
    }
}
