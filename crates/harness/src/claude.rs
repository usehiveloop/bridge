//! Claude Code adapter — wraps `@agentclientprotocol/claude-agent-acp`.
//!
//! Spawns the Node binary as a child process, drives it over ACP stdio,
//! and translates `SessionNotification`s into `BridgeEvent`s on the per-
//! conversation SSE channels.

use crate::events;
use crate::settings;
use crate::skills;
use agent_client_protocol::schema::{
    CancelNotification, ContentBlock, InitializeRequest, NewSessionRequest, PromptRequest,
    ProtocolVersion, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId, SessionNotification, TextContent,
};
use agent_client_protocol::{ByteStreams, Client, ConnectionTo, Responder};
use bridge_core::event::{BridgeEvent, BridgeEventType};
use bridge_core::mcp::McpServerDefinition;
use bridge_core::{AgentDefinition, BridgeError};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio::task::JoinHandle;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::{error, info, warn};
use webhooks::{EventBus, PermissionManager};

/// Per-conversation context returned to the supervisor.
pub struct ConversationContext {
    pub agent_id: String,
    pub conversation_id: String,
    pub events: mpsc::Receiver<BridgeEvent>,
}

/// Options used to launch the Claude ACP agent process.
pub struct ClaudeHarnessOptions {
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    pub config_dir: PathBuf,
    pub extra_env: Vec<(String, String)>,
}

impl ClaudeHarnessOptions {
    pub fn from_env() -> Self {
        Self {
            command: std::env::var("BRIDGE_CLAUDE_ACP_COMMAND")
                .unwrap_or_else(|_| "claude-agent-acp".to_string()),
            args: std::env::var("BRIDGE_CLAUDE_ACP_ARGS")
                .ok()
                .map(|s| s.split_whitespace().map(String::from).collect())
                .unwrap_or_default(),
            working_dir: std::env::var("BRIDGE_WORKING_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))),
            config_dir: std::env::var("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/tmp/claude-state")),
            extra_env: Vec::new(),
        }
    }
}

/// Spawn the Claude ACP harness, run init, and return a handle the
/// supervisor can dispatch into.
pub async fn spawn_claude_harness(
    agent: AgentDefinition,
    opts: ClaudeHarnessOptions,
    event_bus: Arc<EventBus>,
    permission_manager: Arc<PermissionManager>,
) -> Result<Arc<ClaudeHarness>, BridgeError> {
    settings::write_settings(&opts.config_dir, &agent);
    if !agent.skills.is_empty() {
        skills::write_skills(&opts.config_dir, &agent.skills);
    }

    let mut cmd = Command::new(&opts.command);
    cmd.args(&opts.args);
    cmd.current_dir(&opts.working_dir);
    for (k, v) in &opts.extra_env {
        cmd.env(k, v);
    }
    // claude-agent-acp downgrades bypassPermissions to default when running
    // as root unless IS_SANDBOX=1 is set. Bridge processes typically run as
    // root inside containers, so we opt the agent process into the sandbox
    // bypass when (and only when) the agent's config asks for it.
    if agent.config.permission_mode.as_deref() == Some("bypassPermissions") {
        cmd.env("IS_SANDBOX", "1");
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    info!(
        command = %opts.command,
        args = ?opts.args,
        cwd = %opts.working_dir.display(),
        "spawning claude-agent-acp"
    );

    let mut child = cmd
        .spawn()
        .map_err(|e| BridgeError::HarnessError(format!("failed to spawn claude-agent-acp: {e}")))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| BridgeError::HarnessError("claude-agent-acp stdin missing".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| BridgeError::HarnessError("claude-agent-acp stdout missing".into()))?;
    let stderr = child.stderr.take();

    if let Some(stderr) = stderr {
        tokio::spawn(pipe_stderr(stderr));
    }

    let inner = Arc::new(
        ClaudeHarness::start(
            agent,
            opts.working_dir.clone(),
            stdin,
            stdout,
            child,
            event_bus,
            permission_manager,
        )
        .await?,
    );

    Ok(inner)
}

async fn pipe_stderr(stderr: tokio::process::ChildStderr) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        info!(target: "claude_acp", "{}", line);
    }
}

struct SessionState {
    session_id: SessionId,
    sse_tx: mpsc::Sender<BridgeEvent>,
}

enum Cmd {
    NewSession {
        api_key_override: Option<String>,
        provider_override: Option<bridge_core::ProviderConfig>,
        per_conversation_mcp: Option<Vec<McpServerDefinition>>,
        reply: oneshot::Sender<Result<SessionId, String>>,
    },
    Prompt {
        session_id: SessionId,
        text: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Cancel {
        session_id: SessionId,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

/// Thread-safe mutable view of the active agent definition.
type AgentDefStore = Arc<RwLock<AgentDefinition>>;

pub struct ClaudeHarness {
    agent_id: String,
    agent_def: AgentDefStore,
    cmd_tx: mpsc::Sender<Cmd>,
    sessions: Arc<DashMap<String, SessionState>>,
    cwd: PathBuf,
    _driver: JoinHandle<()>,
    _child: Arc<tokio::sync::Mutex<tokio::process::Child>>,
}

impl ClaudeHarness {
    async fn start(
        agent: AgentDefinition,
        cwd: PathBuf,
        stdin: ChildStdin,
        stdout: ChildStdout,
        child: tokio::process::Child,
        event_bus: Arc<EventBus>,
        permission_manager: Arc<PermissionManager>,
    ) -> Result<Self, BridgeError> {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<Cmd>(64);
        let sessions: Arc<DashMap<String, SessionState>> = Arc::new(DashMap::new());
        let agent_id = agent.id.clone();
        let agent_def: AgentDefStore = Arc::new(RwLock::new(agent));

        let agent_id_for_notif = agent_id.clone();
        let agent_id_for_perm = agent_id.clone();
        let agent_id_for_prompt = agent_id.clone();
        let sessions_for_notif = sessions.clone();
        let sessions_for_perm = sessions.clone();
        let sessions_for_prompt = sessions.clone();
        let event_bus_for_notif = event_bus.clone();
        let event_bus_for_perm = event_bus.clone();
        let event_bus_for_prompt = event_bus.clone();
        let agent_def_for_driver = agent_def.clone();
        let cwd_for_driver = cwd.clone();

        let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());

        let driver = tokio::spawn(async move {
            let result = Client
                .builder()
                .name("bridge")
                .on_receive_notification(
                    move |notification: SessionNotification, _cx| {
                        let agent_id = agent_id_for_notif.clone();
                        let sessions = sessions_for_notif.clone();
                        let event_bus = event_bus_for_notif.clone();
                        async move {
                            handle_notification(&agent_id, &sessions, &event_bus, notification)
                                .await;
                            Ok(())
                        }
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .on_receive_request(
                    move |req: RequestPermissionRequest,
                          responder: Responder<RequestPermissionResponse>,
                          _cx| {
                        let perm = permission_manager.clone();
                        let agent_id = agent_id_for_perm.clone();
                        let event_bus = event_bus_for_perm.clone();
                        let sessions = sessions_for_perm.clone();
                        async move {
                            handle_permission(perm, event_bus, &sessions, &agent_id, req, responder)
                                .await
                        }
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_with(
                    transport,
                    move |cx: ConnectionTo<agent_client_protocol::Agent>| async move {
                        cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
                            .block_task()
                            .await?;
                        info!("ACP initialized");

                        while let Some(cmd) = cmd_rx.recv().await {
                            match cmd {
                                Cmd::NewSession {
                                    api_key_override,
                                    provider_override,
                                    per_conversation_mcp,
                                    reply,
                                } => {
                                    let agent_def = agent_def_for_driver.read().await.clone();
                                    let mut req = NewSessionRequest::new(cwd_for_driver.clone());
                                    let mcp_servers = build_mcp_servers(
                                        &agent_def.mcp_servers,
                                        per_conversation_mcp.as_deref(),
                                    );
                                    if !mcp_servers.is_empty() {
                                        req = req.mcp_servers(mcp_servers);
                                    }
                                    if let Some(meta) = build_new_session_meta(
                                        &agent_def,
                                        api_key_override,
                                        provider_override,
                                    ) {
                                        req = req.meta(meta);
                                    }
                                    match cx.send_request(req).block_task().await {
                                        Ok(resp) => {
                                            let _ = reply.send(Ok(resp.session_id));
                                        }
                                        Err(e) => {
                                            let _ =
                                                reply.send(Err(format!("session/new failed: {e}")));
                                        }
                                    }
                                }
                                Cmd::Prompt {
                                    session_id,
                                    text,
                                    reply,
                                } => {
                                    let req = PromptRequest::new(
                                        session_id.clone(),
                                        vec![ContentBlock::Text(TextContent::new(text))],
                                    );
                                    let send = cx.send_request(req);
                                    let agent_id = agent_id_for_prompt.clone();
                                    let event_bus = event_bus_for_prompt.clone();
                                    let sessions = sessions_for_prompt.clone();
                                    let conv_id = session_id.0.to_string();
                                    tokio::spawn(async move {
                                        match send.block_task().await {
                                            Ok(resp) => {
                                                let stop = format!("{:?}", resp.stop_reason)
                                                    .to_ascii_lowercase();
                                                let ev = BridgeEvent::new(
                                                    BridgeEventType::TurnCompleted,
                                                    &agent_id,
                                                    &conv_id,
                                                    json!({ "stop_reason": stop }),
                                                );
                                                event_bus.emit(ev.clone());
                                                if let Some(state) = sessions.get(&conv_id) {
                                                    let _ = state.sse_tx.send(ev).await;
                                                }
                                            }
                                            Err(e) => {
                                                warn!(error = %e, "prompt failed");
                                                let ev = BridgeEvent::new(
                                                    BridgeEventType::AgentError,
                                                    &agent_id,
                                                    &conv_id,
                                                    json!({ "error": e.to_string() }),
                                                );
                                                event_bus.emit(ev.clone());
                                                if let Some(state) = sessions.get(&conv_id) {
                                                    let _ = state.sse_tx.send(ev).await;
                                                }
                                            }
                                        }
                                    });
                                    let _ = reply.send(Ok(()));
                                }
                                Cmd::Cancel { session_id, reply } => {
                                    let _ =
                                        cx.send_notification(CancelNotification::new(session_id));
                                    let _ = reply.send(Ok(()));
                                }
                            }
                        }
                        Ok(())
                    },
                )
                .await;
            if let Err(e) = result {
                error!(error = %e, "ACP connection driver exited");
            }
        });

        Ok(Self {
            agent_id,
            agent_def,
            cmd_tx,
            sessions,
            cwd,
            _driver: driver,
            _child: Arc::new(tokio::sync::Mutex::new(child)),
        })
    }

    /// Update the active agent definition. Picked up on the next session creation.
    pub async fn set_definition(&self, def: AgentDefinition) {
        *self.agent_def.write().await = def;
    }

    pub async fn create_conversation(
        &self,
        api_key_override: Option<String>,
        provider_override: Option<bridge_core::ProviderConfig>,
        per_conversation_mcp: Option<Vec<McpServerDefinition>>,
    ) -> Result<ConversationContext, BridgeError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::NewSession {
                api_key_override,
                provider_override,
                per_conversation_mcp,
                reply: reply_tx,
            })
            .await
            .map_err(|_| BridgeError::HarnessError("harness driver dropped".into()))?;
        let session_id = reply_rx
            .await
            .map_err(|_| BridgeError::HarnessError("session creation cancelled".into()))?
            .map_err(BridgeError::HarnessError)?;

        let (sse_tx, sse_rx) = mpsc::channel(256);
        self.sessions.insert(
            session_id.0.to_string(),
            SessionState {
                session_id: session_id.clone(),
                sse_tx,
            },
        );

        Ok(ConversationContext {
            agent_id: self.agent_id.clone(),
            conversation_id: session_id.0.to_string(),
            events: sse_rx,
        })
    }

    pub async fn send_message(
        &self,
        conversation_id: &str,
        content: String,
        _system_reminder: Option<String>,
    ) -> Result<(), BridgeError> {
        let session_id = self
            .sessions
            .get(conversation_id)
            .map(|s| s.session_id.clone())
            .ok_or_else(|| BridgeError::ConversationNotFound(conversation_id.into()))?;

        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Prompt {
                session_id,
                text: content,
                reply: reply_tx,
            })
            .await
            .map_err(|_| BridgeError::HarnessError("harness driver dropped".into()))?;
        reply_rx
            .await
            .map_err(|_| BridgeError::HarnessError("prompt cancelled".into()))?
            .map_err(BridgeError::HarnessError)
    }

    pub async fn abort(&self, conversation_id: &str) -> Result<(), BridgeError> {
        let session_id = self
            .sessions
            .get(conversation_id)
            .map(|s| s.session_id.clone())
            .ok_or_else(|| BridgeError::ConversationNotFound(conversation_id.into()))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(Cmd::Cancel {
                session_id,
                reply: reply_tx,
            })
            .await
            .map_err(|_| BridgeError::HarnessError("harness driver dropped".into()))?;
        reply_rx
            .await
            .map_err(|_| BridgeError::HarnessError("cancel cancelled".into()))?
            .map_err(BridgeError::HarnessError)
    }

    pub async fn end(&self, conversation_id: &str) {
        self.sessions.remove(conversation_id);
    }

    pub async fn shutdown(&self) {
        self.sessions.clear();
    }
}

async fn handle_notification(
    agent_id: &str,
    sessions: &DashMap<String, SessionState>,
    event_bus: &EventBus,
    notification: SessionNotification,
) {
    let conv_id = notification.session_id.0.to_string();
    let events = events::map_update(agent_id, &conv_id, &notification.update);
    for ev in events {
        event_bus.emit(ev.clone());
        if let Some(state) = sessions.get(&conv_id) {
            let _ = state.sse_tx.send(ev).await;
        }
    }
}

async fn handle_permission(
    perm: Arc<PermissionManager>,
    event_bus: Arc<EventBus>,
    sessions: &DashMap<String, SessionState>,
    agent_id: &str,
    req: RequestPermissionRequest,
    responder: Responder<RequestPermissionResponse>,
) -> Result<(), agent_client_protocol::Error> {
    let conv_id = req.session_id.0.to_string();

    let allow_id = req
        .options
        .iter()
        .find(|o| o.option_id.0.as_ref() == "allow")
        .map(|o| o.option_id.clone())
        .or_else(|| req.options.first().map(|o| o.option_id.clone()));
    let reject_id = req
        .options
        .iter()
        .find(|o| o.option_id.0.as_ref() == "reject")
        .map(|o| o.option_id.clone());

    let tool_name = req
        .tool_call
        .fields
        .title
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let arguments = req
        .tool_call
        .fields
        .raw_input
        .clone()
        .unwrap_or(Value::Null);
    let tool_call_id = req.tool_call.tool_call_id.0.to_string();

    if let Some(state) = sessions.get(&conv_id) {
        let _ = state
            .sse_tx
            .send(BridgeEvent::new(
                BridgeEventType::ToolApprovalRequired,
                agent_id,
                &conv_id,
                json!({
                    "tool_call_id": tool_call_id,
                    "tool_name": tool_name,
                    "arguments": arguments.clone(),
                    "options": req.options.iter().map(|o| {
                        json!({
                            "option_id": o.option_id.0.as_ref(),
                            "name": o.name,
                            "kind": format!("{:?}", o.kind).to_ascii_lowercase(),
                        })
                    }).collect::<Vec<_>>(),
                }),
            ))
            .await;
    }

    let result = perm
        .request_approval(
            agent_id,
            &conv_id,
            &tool_name,
            &tool_call_id,
            &arguments,
            &event_bus,
            None,
            None,
        )
        .await;

    let (outcome, decision_str) = match &result {
        Ok((bridge_core::ApprovalDecision::Approve, _)) => match &allow_id {
            Some(id) => (
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id.clone())),
                "approve",
            ),
            None => (RequestPermissionOutcome::Cancelled, "cancelled"),
        },
        Ok((bridge_core::ApprovalDecision::Deny, _)) => match &reject_id {
            Some(id) => (
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id.clone())),
                "deny",
            ),
            None => (RequestPermissionOutcome::Cancelled, "cancelled"),
        },
        Err(_) => (RequestPermissionOutcome::Cancelled, "cancelled"),
    };

    // Emit ToolApprovalResolved onto the per-session SSE channel.
    // PermissionManager.resolve already emitted to the global event bus
    // (webhooks, polling); the SSE stream needs its own copy.
    if let Some(state) = sessions.get(&conv_id) {
        let _ = state
            .sse_tx
            .send(BridgeEvent::new(
                BridgeEventType::ToolApprovalResolved,
                agent_id,
                &conv_id,
                json!({
                    "tool_call_id": tool_call_id,
                    "decision": decision_str,
                }),
            ))
            .await;
    }

    responder.respond(RequestPermissionResponse::new(outcome))
}

fn build_mcp_servers(
    agent: &[McpServerDefinition],
    per_conv: Option<&[McpServerDefinition]>,
) -> Vec<agent_client_protocol::schema::McpServer> {
    let mut out = Vec::new();
    for s in agent.iter().chain(per_conv.unwrap_or(&[]).iter()) {
        out.push(translate_mcp(s));
    }
    out
}

fn translate_mcp(def: &McpServerDefinition) -> agent_client_protocol::schema::McpServer {
    use agent_client_protocol::schema::{
        EnvVariable, HttpHeader, McpServer, McpServerHttp, McpServerStdio,
    };
    use bridge_core::mcp::McpTransport;
    match &def.transport {
        McpTransport::Stdio { command, args, env } => {
            let env_vec: Vec<EnvVariable> = env
                .iter()
                .map(|(k, v)| EnvVariable::new(k.clone(), v.clone()))
                .collect();
            McpServer::Stdio(
                McpServerStdio::new(def.name.clone(), PathBuf::from(command))
                    .args(args.clone())
                    .env(env_vec),
            )
        }
        McpTransport::StreamableHttp { url, headers } => {
            let header_vec: Vec<HttpHeader> = headers
                .iter()
                .map(|(k, v)| HttpHeader::new(k.clone(), v.clone()))
                .collect();
            McpServer::Http(McpServerHttp::new(def.name.clone(), url.clone()).headers(header_vec))
        }
    }
}

fn build_new_session_meta(
    agent: &AgentDefinition,
    api_key_override: Option<String>,
    provider_override: Option<bridge_core::ProviderConfig>,
) -> Option<serde_json::Map<String, Value>> {
    let mut options = serde_json::Map::new();

    if !agent.system_prompt.trim().is_empty() {
        options.insert(
            "systemPrompt".to_string(),
            json!({ "append": agent.system_prompt }),
        );
    }

    if !agent.config.allowed_tools.is_empty() {
        options.insert(
            "allowedTools".to_string(),
            Value::Array(
                agent
                    .config
                    .allowed_tools
                    .iter()
                    .map(|t| Value::String(t.clone()))
                    .collect(),
            ),
        );
    }
    if !agent.config.disabled_tools.is_empty() {
        options.insert(
            "disallowedTools".to_string(),
            Value::Array(
                agent
                    .config
                    .disabled_tools
                    .iter()
                    .map(|t| Value::String(t.clone()))
                    .collect(),
            ),
        );
    }

    if let Some(mode) = &agent.config.permission_mode {
        options.insert("permissionMode".to_string(), json!(mode));
    }
    if let Some(model) = provider_override.as_ref().map(|p| p.model.clone()) {
        options.insert("model".to_string(), json!(model));
    }
    if let Some(extra) = api_key_override {
        let mut env_obj = options
            .get("env")
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        env_obj.insert("ANTHROPIC_API_KEY".to_string(), json!(extra));
        options.insert("env".to_string(), Value::Object(env_obj));
    }

    if options.is_empty() {
        None
    } else {
        let mut meta = serde_json::Map::new();
        meta.insert(
            "claudeCode".to_string(),
            json!({ "options": Value::Object(options) }),
        );
        Some(meta)
    }
}
