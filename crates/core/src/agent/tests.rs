use super::*;
use crate::provider::ProviderType;

const SIMPLE_AGENT: &str = r#"{
    "id": "agent_simple",
    "name": "Simple Agent",
    "system_prompt": "You are a helpful assistant.",
    "provider": {
        "provider_type": "open_ai",
        "model": "gpt-4o",
        "api_key": "test-key",
        "base_url": "https://api.openai.com/v1"
    }
}"#;

#[test]
fn parse_simple_agent_inline_fixture() {
    let agent: AgentDefinition =
        serde_json::from_str(SIMPLE_AGENT).expect("simple agent JSON should deserialize");

    assert_eq!(agent.id, "agent_simple");
    assert_eq!(agent.name, "Simple Agent");
    assert_eq!(agent.system_prompt, "You are a helpful assistant.");
    assert_eq!(agent.provider.provider_type, ProviderType::OpenAI);
    assert_eq!(agent.provider.model, "gpt-4o");
    assert!(agent.tools.is_empty());
    assert!(agent.mcp_servers.is_empty());
    assert!(agent.skills.is_empty());
    assert!(agent.webhook_url.is_none());
}

#[test]
fn legacy_payload_with_deprecated_fields_still_parses() {
    // Customers carrying old AgentDefinition payloads with immortal/verifier
    // /history_strip/integrations/etc. must continue to deserialize cleanly
    // post-harness-rip; bridge ignores those fields at runtime.
    let json = r#"{
        "id": "legacy",
        "name": "Legacy",
        "system_prompt": "hello",
        "provider": {
            "provider_type": "anthropic",
            "model": "claude-haiku-4-5",
            "api_key": "k"
        },
        "tools": [
            {"name": "calc", "description": "x", "parameters_schema": {"type": "object"}}
        ],
        "skills": [],
        "integrations": [],
        "config": {
            "max_tokens": 4096,
            "immortal": {"token_budget": 100000},
            "history_strip": {"enabled": true},
            "tool_requirements": []
        },
        "subagents": [],
        "permissions": {}
    }"#;

    let agent: AgentDefinition =
        serde_json::from_str(json).expect("legacy payload must still deserialize");
    assert_eq!(agent.id, "legacy");
    assert_eq!(agent.config.max_tokens, Some(4096));
}

#[test]
fn simple_agent_roundtrip() {
    let agent: AgentDefinition = serde_json::from_str(SIMPLE_AGENT).unwrap();
    let serialized = serde_json::to_string(&agent).unwrap();
    let roundtripped: AgentDefinition = serde_json::from_str(&serialized).unwrap();
    assert_eq!(agent, roundtripped);
}
