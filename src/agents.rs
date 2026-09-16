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
    pub fn identifier(&self) -> String {
        format!("urn:aithos:calendar:agent:{}", self.id)
    }

    pub fn availability(&self) -> Vec<crate::scheduling::Slot> {
        let ranges = match self.id {
            "alice" => [("09:00", "10:00"), ("14:00", "15:00")],
            "bob" => [("09:30", "10:30"), ("15:00", "16:00")],
            _ => unreachable!("known fixture agent"),
        };
        ranges
            .into_iter()
            .map(|(start, end)| crate::scheduling::Slot {
                start: format!("2030-01-15T{start}:00Z")
                    .parse()
                    .expect("fixture timestamp"),
                end: format!("2030-01-15T{end}:00Z")
                    .parse()
                    .expect("fixture timestamp"),
            })
            .collect()
    }

    pub fn card(&self, base_url: &str) -> AgentCard {
        serde_json::from_value(json!({
            "name": self.name,
            "description": format!("{}'s Calendar test agent. Shares mock availability and finds common slots; does not book appointments.", self.name),
            "version": "0.2.0",
            "supportedInterfaces": [{
                "url": format!("{base_url}/a2a"),
                "protocolBinding": "JSONRPC",
                "protocolVersion": "1.0",
                "tenant": self.id
            }],
            "capabilities": {"streaming": false, "pushNotifications": false, "extendedAgentCard": false},
            "defaultInputModes": ["text/plain", "application/json"],
            "defaultOutputModes": ["text/plain", "application/json"],
            "skills": [{
                "id": "greeting", "name": "Mock greeting",
                "description": "Returns a deterministic greeting from the selected agent.",
                "tags": ["mock", "greeting"], "examples": ["Hello"]
            }, {
                "id": "get_availability", "name": "Mock availability",
                "description": "Returns fixed UTC availability for January 15, 2030.",
                "tags": ["mock", "availability"], "inputModes": ["application/json"]
            }, {
                "id": "find_common_slot", "name": "Find a mock common slot",
                "description": "Discovers a peer in the configured catalog and asks it for availability over A2A.",
                "tags": ["mock", "scheduling"], "inputModes": ["application/json"]
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
