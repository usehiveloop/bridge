//! Stubbed `AgentSupervisor`.
//!
//! The original supervisor drove rig-core agents, the in-house conversation
//! loop, MCP connections, LSP, tool registration, subagents, and immortal
//! handoff. All of that is gone. The methods below preserve the call shape
//! the api/bridge crates depend on, but every method that used to dispatch
//! to the model now returns [`BridgeError::HarnessUnavailable`]. State
//! mutations (push/sync, definition storage) still work so the control
//! plane sync cycle behaves correctly until the harness adapter lands.

use bridge_core::event::BridgeEvent;
use bridge_core::mcp::McpServerDefinition;
use bridge_core::metrics::MetricsSnapshot;
use bridge_core::{AgentDefinition, AgentSummary, BridgeError, RuntimeConfig};
use std::sync::Arc;
use storage::{StorageBackend, StorageHandle};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;
use webhooks::{EventBus, PermissionManager};

use crate::agent_map::AgentMap;
use crate::agent_state::AgentState;

/// Central supervisor for all agents.
pub struct AgentSupervisor {
    pub(super) agent_map: AgentMap,
    pub(super) cancel: CancellationToken,
    pub(super) event_bus: Option<Arc<EventBus>>,
    pub(super) permission_manager: Arc<PermissionManager>,
    pub(super) storage: Option<StorageHandle>,
    pub(super) storage_backend: Option<Arc<dyn StorageBackend>>,
}

impl AgentSupervisor {
    /// Create a new supervisor.
    pub fn new(cancel: CancellationToken) -> Self {
        Self {
            agent_map: AgentMap::new(),
            cancel,
            event_bus: None,
            permission_manager: Arc::new(PermissionManager::new()),
            storage: None,
            storage_backend: None,
        }
    }

    pub fn with_event_bus(mut self, event_bus: Option<Arc<EventBus>>) -> Self {
        self.event_bus = event_bus;
        self
    }

    pub fn with_storage(mut self, storage: Option<StorageHandle>) -> Self {
        self.storage = storage;
        self
    }

    pub fn with_storage_backend(mut self, backend: Option<Arc<dyn StorageBackend>>) -> Self {
        self.storage_backend = backend;
        self
    }

    /// Kept for source compatibility with the previous bootstrap. The harness
    /// has its own concurrency model — this is now a no-op.
    pub fn with_capacity_limits(self, _config: &RuntimeConfig) -> Self {
        self
    }

    /// Shared permission manager exposed via `AppState`.
    pub fn permission_manager(&self) -> Arc<PermissionManager> {
        self.permission_manager.clone()
    }

    pub fn get_agent(&self, agent_id: &str) -> Option<Arc<AgentState>> {
        self.agent_map.get(agent_id)
    }

    pub async fn list_agents(&self) -> Vec<AgentSummary> {
        self.agent_map.list().await
    }

    pub fn list_agent_states(&self) -> Vec<Arc<AgentState>> {
        self.agent_map.list_states()
    }

    pub fn agent_count(&self) -> usize {
        self.agent_map.len()
    }

    /// Persist a batch of agent definitions. The harness adapter will read
    /// them when it starts up.
    pub async fn load_agents(&self, defs: Vec<AgentDefinition>) -> Result<(), BridgeError> {
        for def in defs {
            self.upsert_definition(def).await;
        }
        Ok(())
    }

    /// Apply a control-plane diff. Adds/updates/removes are applied to the
    /// in-memory map and forwarded to storage if configured. No runtime agent
    /// is built — the harness adapter owns that.
    pub async fn apply_diff(
        &self,
        added: Vec<AgentDefinition>,
        updated: Vec<AgentDefinition>,
        removed: Vec<String>,
    ) -> Result<(), BridgeError> {
        for def in added.into_iter().chain(updated.into_iter()) {
            self.upsert_definition(def).await;
        }
        for id in removed {
            self.agent_map.remove(&id);
            if let Some(storage) = &self.storage {
                storage.delete_agent(id);
            }
        }
        Ok(())
    }

    /// Rotate an agent's LLM API key in the stored definition. Real propagation
    /// to the harness will happen when the adapter is wired.
    pub async fn update_agent_api_key(
        &self,
        agent_id: &str,
        api_key: String,
    ) -> Result<(), BridgeError> {
        let agent = self
            .agent_map
            .get(agent_id)
            .ok_or_else(|| BridgeError::AgentNotFound(agent_id.to_string()))?;
        {
            let mut def = agent.definition.write().await;
            def.provider.api_key = api_key;
        }
        if let Some(storage) = &self.storage {
            let def = agent.definition.read().await.clone();
            storage.save_agent(def);
        }
        Ok(())
    }

    /// Stub. Returns `HarnessUnavailable` until the ACP adapter lands.
    pub async fn create_conversation(
        &self,
        agent_id: &str,
        _api_key_override: Option<String>,
        _provider_override: Option<bridge_core::ProviderConfig>,
        _per_conversation_mcp_servers: Option<Vec<McpServerDefinition>>,
    ) -> Result<(String, mpsc::Receiver<BridgeEvent>), BridgeError> {
        if self.agent_map.get(agent_id).is_none() {
            return Err(BridgeError::AgentNotFound(agent_id.to_string()));
        }
        Err(BridgeError::HarnessUnavailable)
    }

    /// Stub.
    pub async fn send_message(
        &self,
        _agent_id: &str,
        _conv_id: &str,
        _content: String,
        _system_reminder: Option<String>,
    ) -> Result<(), BridgeError> {
        Err(BridgeError::HarnessUnavailable)
    }

    /// Stub. The new harness owns conversation lifecycle.
    pub fn end_conversation(&self, _agent_id: &str, _conv_id: &str) -> Result<(), BridgeError> {
        Err(BridgeError::HarnessUnavailable)
    }

    /// Stub.
    pub async fn abort_conversation(
        &self,
        _agent_id: &str,
        _conv_id: &str,
    ) -> Result<(), BridgeError> {
        Err(BridgeError::HarnessUnavailable)
    }

    /// Stub. Returns no SSE receivers — the harness will handle hydration.
    pub async fn hydrate_conversations(
        &self,
        _agent_id: &str,
        _records: Vec<bridge_core::conversation::ConversationRecord>,
    ) -> Vec<(String, mpsc::Receiver<BridgeEvent>)> {
        Vec::new()
    }

    /// Stub. Returns an empty snapshot vec.
    pub async fn collect_metrics(&self) -> Vec<MetricsSnapshot> {
        Vec::new()
    }

    /// Cancel the supervisor. Idempotent.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        info!("supervisor shutdown");
    }

    async fn upsert_definition(&self, def: AgentDefinition) {
        let id = def.id.clone();
        if let Some(storage) = &self.storage {
            storage.save_agent(def.clone());
        }
        if let Some(existing) = self.agent_map.get(&id) {
            *existing.definition.write().await = def;
        } else {
            self.agent_map.insert(id, Arc::new(AgentState::new(def)));
        }
    }
}
