use anyhow::Context;
use bridge_core::RuntimeConfig;
use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use runtime::AgentSupervisor;
use std::sync::Arc;
use storage::{StorageBackend, StorageHandle};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::info;
use webhooks::EventBus;

use crate::logging::{init_logging, shutdown_signal};

pub(crate) async fn run_server() -> anyhow::Result<()> {
    // Load configuration from config.toml and environment variables
    let config: RuntimeConfig = Figment::from(Serialized::defaults(RuntimeConfig::default()))
        .merge(Toml::file("config.toml"))
        .merge(Env::prefixed("BRIDGE_"))
        .extract()
        .context("failed to load configuration")?;

    init_logging(&config);

    info!("bridge starting");

    let cancel = CancellationToken::new();

    let (storage_backend, storage_handle): (
        Option<Arc<dyn StorageBackend>>,
        Option<StorageHandle>,
    ) = match storage::init_storage()
        .await
        .context("failed to initialize storage")?
    {
        Some((backend, handle)) => (Some(backend), Some(handle)),
        None => (None, None),
    };

    if storage_backend.is_some() {
        info!("storage persistence enabled");
    } else {
        info!("storage persistence disabled");
    }

    // Create the unified event bus with optional webhook HTTP delivery.
    let webhook_url = config.webhook_url.clone().unwrap_or_default();
    let webhook_secret = config.control_plane_api_key.clone();

    let webhook_tx = if config.webhook_url.is_some() {
        let webhook_config = config.webhook_config.clone().unwrap_or_default();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let client = reqwest::Client::new();
        let url = webhook_url.clone();
        let secret = webhook_secret.clone();
        tokio::spawn(webhooks::run_delivery(
            rx,
            client,
            cancel.clone(),
            webhook_config,
            url,
            secret,
            storage_handle.clone(),
        ));
        info!(url = %webhook_url, "webhook delivery started");
        Some(tx)
    } else {
        None
    };

    let event_bus = Arc::new(EventBus::new(
        webhook_tx,
        storage_handle.clone(),
        webhook_url,
        webhook_secret,
    ));

    if config.websocket_enabled {
        info!("WebSocket event stream enabled on /ws/events");
    }

    // Build the (currently stubbed) supervisor. Once the harness adapter
    // lands, this is where it gets wired in.
    let supervisor = Arc::new(
        AgentSupervisor::new(cancel.clone())
            .with_capacity_limits(&config)
            .with_event_bus(Some(event_bus.clone()))
            .with_storage_backend(storage_backend.clone())
            .with_storage(storage_handle.clone()),
    );

    let app_state = api::AppState::new(
        supervisor.clone(),
        config.control_plane_api_key.clone(),
        storage_backend.clone(),
        cancel.clone(),
        event_bus.clone(),
    );

    if let Some(backend) = &storage_backend {
        restore_from_storage(backend, &supervisor, &event_bus).await?;
    }

    let app = api::build_router(app_state);

    let listener = TcpListener::bind(&config.listen_addr)
        .await
        .context("failed to bind TCP listener")?;
    info!(addr = config.listen_addr, "listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(cancel.clone()))
        .await
        .context("server error")?;

    info!("shutting down...");
    cancel.cancel();
    supervisor.shutdown().await;
    if let Some(handle) = storage_handle {
        handle.drain().await;
    }

    info!("bridge stopped");

    Ok(())
}

async fn restore_from_storage(
    backend: &Arc<dyn StorageBackend>,
    supervisor: &Arc<AgentSupervisor>,
    event_bus: &Arc<EventBus>,
) -> anyhow::Result<()> {
    let stored_agents = backend
        .load_all_agents()
        .await
        .context("failed to load stored agents")?;

    if !stored_agents.is_empty() {
        let agent_count = stored_agents.len();
        supervisor
            .load_agents(stored_agents)
            .await
            .context("failed to restore stored agents")?;
        info!(count = agent_count, "restored agents from storage");
    }

    // Conversation hydration is owned by the harness adapter; the stub
    // supervisor returns no SSE receivers. Pending events still get
    // replayed so webhook outbox catches up.
    let pending_events = backend
        .load_pending_events()
        .await
        .context("failed to load pending events")?;

    if !pending_events.is_empty() {
        let count = pending_events.len();
        for event in pending_events {
            event_bus.emit_replayed(event);
        }
        info!(count = count, "replayed pending events from storage");
    }

    Ok(())
}
