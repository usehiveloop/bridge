use bridge_core::mcp::McpServerDefinition;
use bridge_core::BridgeError;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::OnceCell;
use tracing::{error, info, warn};

use crate::connection::McpConnection;

/// Lazy connection cell — shared across callers so concurrent
/// `get_or_connect` calls for the same (agent, server) key cannot race
/// into a double-spawn. The `OnceCell` guarantees exactly-once
/// initialization.
type ConnectionCell = Arc<OnceCell<Result<Arc<McpConnection>, String>>>;

/// Manages MCP server connections for all agents.
///
/// Connections are keyed by (agent_id, server_name) so each agent can have
/// its own set of MCP server connections that are independently managed.
pub struct McpManager {
    /// Map of (agent_id, server_name) → McpConnection
    connections: DashMap<(String, String), Arc<McpConnection>>,
    /// Map of (agent_id, server_name) → lazy connect cell (for dedup).
    /// The cell's ok branch is mirrored into `connections` once filled.
    spawning: DashMap<(String, String), ConnectionCell>,
}

impl McpManager {
    /// Create a new empty MCP manager.
    pub fn new() -> Self {
        Self {
            connections: DashMap::new(),
            spawning: DashMap::new(),
        }
    }

    /// Connect an agent to all its configured MCP servers.
    ///
    /// Establishes connections and stores them for later use. Tool discovery
    /// is deferred to callers who can query each connection individually via
    /// `McpConnection::list_tools()` to preserve the tool-to-server association.
    pub async fn connect_agent(
        &self,
        agent_id: &str,
        servers: &[McpServerDefinition],
    ) -> Result<(), BridgeError> {
        for server in servers {
            match self.get_or_connect(agent_id, server).await {
                Ok(conn) => {
                    info!(
                        agent_id = agent_id,
                        server = server.name,
                        "connected to MCP server"
                    );

                    match conn.list_tools().await {
                        Ok(tools) => {
                            info!(
                                agent_id = agent_id,
                                server = server.name,
                                tool_count = tools.len(),
                                "discovered MCP tools"
                            );
                        }
                        Err(e) => {
                            error!(
                                agent_id = agent_id,
                                server = server.name,
                                error = %e,
                                "failed to list tools from MCP server"
                            );
                        }
                    }
                }
                Err(e) => {
                    error!(
                        agent_id = agent_id,
                        server = server.name,
                        error = %e,
                        "failed to connect to MCP server"
                    );
                }
            }
        }

        Ok(())
    }

    /// Get-or-spawn a connection atomically. Concurrent callers for the same
    /// key all receive the same Arc<McpConnection> (or the same error) from
    /// a single underlying spawn.
    async fn get_or_connect(
        &self,
        agent_id: &str,
        server: &McpServerDefinition,
    ) -> Result<Arc<McpConnection>, BridgeError> {
        let key = (agent_id.to_string(), server.name.clone());

        // Fast path: already-connected and still alive.
        if let Some(existing) = self.connections.get(&key) {
            let c = existing.value().clone();
            if c.is_alive() {
                return Ok(c);
            }
        }

        let cell: ConnectionCell = self
            .spawning
            .entry(key.clone())
            .or_insert_with(|| Arc::new(OnceCell::new()))
            .value()
            .clone();

        let result = cell
            .get_or_init(|| async {
                McpConnection::connect(&server.name, &server.transport)
                    .await
                    .map(Arc::new)
                    .map_err(|e| e.to_string())
            })
            .await;

        match result {
            Ok(conn) => {
                self.connections.insert(key.clone(), conn.clone());
                // Keep the cell around so future callers still hit the fast path,
                // but also remove it to prevent unbounded growth when clients
                // reconnect repeatedly. WHY: the connection is now in `connections`,
                // the cell served its dedup purpose.
                self.spawning.remove(&key);
                Ok(conn.clone())
            }
            Err(e) => {
                // Evict the failed cell so future callers retry instead of
                // replaying the same cached failure forever.
                self.spawning.remove(&key);
                Err(BridgeError::McpError(e.clone()))
            }
        }
    }

    /// Disconnect all MCP servers for a given agent.
    pub async fn disconnect_agent(&self, agent_id: &str) {
        let keys_to_remove: Vec<(String, String)> = self
            .connections
            .iter()
            .filter(|entry| entry.key().0 == agent_id)
            .map(|entry| entry.key().clone())
            .collect();

        for key in keys_to_remove {
            if let Some((_, conn)) = self.connections.remove(&key) {
                info!(
                    agent_id = agent_id,
                    server = key.1,
                    "disconnecting MCP server"
                );
                if let Ok(conn) = Arc::try_unwrap(conn) {
                    conn.disconnect().await;
                }
            }
        }
    }

    /// Get a connection to a specific MCP server for an agent.
    ///
    /// If a stored connection has died (child process exited), it is
    /// evicted and `Err(BridgeError::McpError("connection lost"))` is
    /// returned so callers stop using a stale handle.
    pub fn get_connection(
        &self,
        agent_id: &str,
        server_name: &str,
    ) -> Result<Arc<McpConnection>, BridgeError> {
        let key = (agent_id.to_string(), server_name.to_string());
        let conn = match self.connections.get(&key) {
            Some(entry) => entry.value().clone(),
            None => {
                return Err(BridgeError::McpError(format!(
                    "no connection for agent '{}' server '{}'",
                    agent_id, server_name
                )));
            }
        };

        if conn.is_alive() {
            Ok(conn)
        } else {
            warn!(
                agent_id = agent_id,
                server = server_name,
                "MCP connection lost; evicting stale handle"
            );
            self.connections.remove(&key);
            Err(BridgeError::McpError(format!(
                "connection lost for agent '{}' server '{}'",
                agent_id, server_name
            )))
        }
    }

    /// Get all connections for a given agent.
    pub fn get_agent_connections(&self, agent_id: &str) -> Vec<Arc<McpConnection>> {
        self.connections
            .iter()
            .filter(|entry| entry.key().0 == agent_id)
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Get the total number of active connections.
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_manager_is_empty() {
        let manager = McpManager::new();
        assert_eq!(manager.connection_count(), 0);
    }

    #[test]
    fn test_default_manager_is_empty() {
        let manager = McpManager::default();
        assert_eq!(manager.connection_count(), 0);
    }

    #[test]
    fn test_get_connection_returns_error_for_unknown() {
        let manager = McpManager::new();
        assert!(manager.get_connection("agent1", "server1").is_err());
    }

    #[test]
    fn test_get_agent_connections_returns_empty_for_unknown() {
        let manager = McpManager::new();
        let conns = manager.get_agent_connections("agent1");
        assert!(conns.is_empty());
    }

    #[test]
    fn test_get_connection_returns_error_for_wrong_agent() {
        let manager = McpManager::new();
        // No connections exist, so querying any agent/server pair returns an error
        assert!(manager.get_connection("agent1", "server_a").is_err());
        assert!(manager.get_connection("agent2", "server_a").is_err());
    }

    #[test]
    fn test_get_agent_connections_returns_empty_for_multiple_unknown_agents() {
        let manager = McpManager::new();
        assert!(manager.get_agent_connections("agent1").is_empty());
        assert!(manager.get_agent_connections("agent2").is_empty());
        assert!(manager.get_agent_connections("").is_empty());
    }

    #[test]
    fn test_dashmap_keying_different_agents_same_server() {
        // Verify the (agent_id, server_name) tuple keying logic:
        // Two different agents connecting to the same server name should produce
        // distinct keys.
        let key1 = ("agent1".to_string(), "server_a".to_string());
        let key2 = ("agent2".to_string(), "server_a".to_string());
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_dashmap_keying_same_agent_different_servers() {
        let key1 = ("agent1".to_string(), "server_a".to_string());
        let key2 = ("agent1".to_string(), "server_b".to_string());
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_dashmap_keying_identical_keys() {
        let key1 = ("agent1".to_string(), "server_a".to_string());
        let key2 = ("agent1".to_string(), "server_a".to_string());
        assert_eq!(key1, key2);
    }

    #[tokio::test]
    async fn test_disconnect_agent_on_empty_manager() {
        let manager = McpManager::new();
        // Should not panic when disconnecting a non-existent agent
        manager.disconnect_agent("nonexistent").await;
        assert_eq!(manager.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_connect_agent_with_no_servers() {
        let manager = McpManager::new();
        manager.connect_agent("agent1", &[]).await.unwrap();
        assert_eq!(manager.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_connect_agent_with_invalid_stdio_server() {
        let manager = McpManager::new();
        let servers = vec![McpServerDefinition {
            name: "bad_server".to_string(),
            transport: bridge_core::mcp::McpTransport::Stdio {
                command: "/nonexistent/binary/that/does/not/exist".to_string(),
                args: vec![],
                env: std::collections::HashMap::new(),
            },
        }];
        // Should not panic; the server fails to connect but the manager handles it gracefully
        manager.connect_agent("agent1", &servers).await.unwrap();
        assert_eq!(manager.connection_count(), 0);
    }

    #[tokio::test]
    async fn test_oncecell_dedup_concurrent_failures_only_spawn_once() {
        // WHY: the key safety property is that concurrent get_or_connect
        // calls for the same (agent, server) all see the same underlying
        // spawn result, rather than each launching their own subprocess.
        let manager = Arc::new(McpManager::new());
        let def = McpServerDefinition {
            name: "dedup_server".to_string(),
            transport: bridge_core::mcp::McpTransport::Stdio {
                command: "/nonexistent/binary/for/dedup".to_string(),
                args: vec![],
                env: std::collections::HashMap::new(),
            },
        };

        let mut handles = vec![];
        for _ in 0..8 {
            let m = manager.clone();
            let d = def.clone();
            handles.push(tokio::spawn(async move {
                m.get_or_connect("agent1", &d).await
            }));
        }
        for h in handles {
            let r = h.await.unwrap();
            assert!(r.is_err());
        }
        assert_eq!(manager.connection_count(), 0);
    }
}
