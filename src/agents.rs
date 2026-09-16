use a2a::{A2AError, AgentCard};
use ai_catalog::AiCatalog;
use serde_json::json;

pub struct Agent {
    pub id: &'static str,
    pub name: &'static str,
}

pub const AGENTS: [Agent; 2] = [
    Agent {
        id: "alice",
        name: "Alice",
    },
    Agent {
        id: "bob",
        name: "Bob",
    },
];

pub fn find(tenant: Option<&str>) -> Result<&'static Agent, A2AError> {
    AGENTS
        .iter()
        .find(|agent| Some(agent.id) == tenant)
        .ok_or_else(|| A2AError::invalid_params("Unknown or missing tenant"))
}

impl Agent {
    pub fn card(&self, base_url: &str) -> AgentCard {
        serde_json::from_value(json!({
            "name": self.name,
            "description": format!("{}'s Calendar test agent. Returns a greeting; does not book appointments.", self.name),
            "version": "0.1.0",
            "supportedInterfaces": [{
                "url": format!("{base_url}/a2a"),
                "protocolBinding": "JSONRPC",
                "protocolVersion": "1.0",
                "tenant": self.id
            }],
            "capabilities": {"streaming": false, "pushNotifications": false, "extendedAgentCard": false},
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["text/plain"],
            "skills": [{
                "id": "greeting", "name": "Mock greeting",
                "description": "Returns a deterministic greeting from the selected agent.",
                "tags": ["mock", "greeting"], "examples": ["Hello"]
            }]
        })).expect("static agent fixture must match SDK schema")
    }
}

pub fn catalog(base_url: &str) -> AiCatalog {
    serde_json::from_value(json!({
        "specVersion": "1.0",
        "host": {"displayName": "Calendar A2A test agents"},
        "entries": AGENTS.iter().map(|agent| json!({
            "identifier": format!("urn:aithos:calendar:agent:{}", agent.id),
            "displayName": agent.name,
            "type": "application/a2a-agent-card+json",
            "url": format!("{base_url}/agents/{}/agent-card.json", agent.id),
            "description": "Public mock agent for testing A2A routing. No calendar access.",
            "tags": ["calendar", "mock"]
        })).collect::<Vec<_>>()
    }))
    .expect("static catalog fixture must match catalog schema")
}
