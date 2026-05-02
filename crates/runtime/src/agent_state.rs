use bridge_core::{AgentDefinition, AgentMetrics};
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Empty entry kept so `AgentState::subagents` can hold per-subagent rows
/// when the harness adapter populates them. For now it carries a name only.
#[derive(Default)]
pub struct SubAgentEntry {
    pub registered_tools: Vec<(String, String)>,
}

/// Handle for an active conversation. Inert — populated only when the
/// harness adapter is wired up.
pub struct ConversationHandle {
    pub id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Per-agent state.
///
/// Inert: holds the stored definition and metrics counters and nothing else.
/// The conversation loop, tool registry, rig agent, MCP wiring, session
/// store, and subagent runners that used to live here are gone with the
/// harness rip.
pub struct AgentState {
    pub definition: RwLock<AgentDefinition>,
    pub metrics: Arc<AgentMetrics>,
    pub subagents: Arc<DashMap<String, SubAgentEntry>>,
    pub conversations: DashMap<String, ConversationHandle>,
}

impl AgentState {
    pub fn new(definition: AgentDefinition) -> Self {
        Self {
            definition: RwLock::new(definition),
            metrics: Arc::new(AgentMetrics::new()),
            subagents: Arc::new(DashMap::new()),
            conversations: DashMap::new(),
        }
    }

    pub async fn id(&self) -> String {
        self.definition.read().await.id.clone()
    }

    pub async fn name(&self) -> String {
        self.definition.read().await.name.clone()
    }

    pub async fn version(&self) -> Option<String> {
        self.definition.read().await.version.clone()
    }

    pub fn has_conversation(&self, conv_id: &str) -> bool {
        self.conversations.contains_key(conv_id)
    }

    pub fn active_conversation_count(&self) -> usize {
        self.conversations.len()
    }
}
