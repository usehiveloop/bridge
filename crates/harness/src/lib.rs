//! Bridge harness layer.
//!
//! Adapts external coding-agent CLIs (currently Claude Code; OpenCode later)
//! to Bridge's supervisor surface using the Agent Client Protocol (ACP) over
//! stdio. Notifications stream back as `BridgeEvent`s on per-conversation
//! SSE channels.

pub mod claude;
pub mod events;
pub mod skills;

pub use claude::{spawn_claude_harness, ClaudeHarness, ClaudeHarnessOptions, ConversationContext};
