use a2a::AgentCard;
use serde_json::json;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Agent {
    pub id: String,
    #[serde(default)]
    pub live: bool,
    #[serde(default)]
    pub google_account: bool,
    pub name: String,
    pub slots: Vec<crate::scheduling::Slot>,
}

/// Only local/test constructors use these fixtures; production starts empty.
pub fn fixtures() -> Vec<Agent> {
    [
        ("alice", "Alice", [("09:00", "10:00"), ("14:00", "15:00")]),
        ("bob", "Bob", [("09:30", "10:30"), ("15:00", "16:00")]),
    ]
    .into_iter()
    .map(|(id, name, ranges)| Agent {
        id: id.into(),
        live: false,
        google_account: false,
        name: name.into(),
        slots: ranges
            .into_iter()
            .map(|(start, end)| crate::scheduling::Slot {
                start: format!("2030-01-15T{start}:00Z").parse().unwrap(),
                end: format!("2030-01-15T{end}:00Z").parse().unwrap(),
            })
            .collect(),
    })
    .collect()
}

impl Agent {
    pub fn identifier(&self) -> String {
        format!("urn:aithos:calendar:agent:{}", self.id)
    }
    pub fn availability(&self) -> Vec<crate::scheduling::Slot> {
        self.slots.clone()
    }

    pub fn card(&self, base_url: &str) -> AgentCard {
        if self.google_account {
            return serde_json::from_value(json!({
                "name":self.name, "description":"Account-linked Google Calendar agent. Availability and booking require a short-lived operation capability issued by this service.",
                "version":"0.6.0", "supportedInterfaces":[{"url":format!("{base_url}/a2a"),"protocolBinding":"JSONRPC","protocolVersion":"1.0","tenant":self.id}],
                "capabilities":{}, "defaultInputModes":["text/plain","application/json"], "defaultOutputModes":["text/plain","application/json"],
                "securitySchemes":{"calendarOperation":{"httpAuthSecurityScheme":{"scheme":"Bearer","description":"Short-lived capability bound to the caller, recipient and exact operation; issued internally after browser authorization."}}},
                "skills":[{"id":"greeting","name":"Greeting","description":"Returns a public greeting.","tags":["greeting"]},
                {"id":"get_availability","name":"Calendar availability","description":"Reads free intervals on the connected primary calendar during weekdays 09:00–18:00, within a 30-day window.","tags":["calendar","availability"],"inputModes":["application/json"],"securityRequirements":[{"schemes":{"calendarOperation":{"list":[]}}}]},
                {"id":"commit_booking","name":"Confirm a meeting","description":"Creates or reconciles a confirmed, server-stored booking using a deterministic event ID.","tags":["calendar","booking"],"inputModes":["application/json"],"securityRequirements":[{"schemes":{"calendarOperation":{"list":[]}}}]}]
            })).expect("account card matches SDK schema");
        }
        if self.live {
            return serde_json::from_value(json!({
                "name":self.name, "description":"Reads public Google booking-page availability and finds a common offered host slot through A2A. Does not book appointments.",
                "version":"0.4.0", "supportedInterfaces":[{"url":format!("{base_url}/a2a"),"protocolBinding":"JSONRPC","protocolVersion":"1.0","tenant":self.id}],
                "capabilities":{}, "defaultInputModes":["text/plain","application/json"], "defaultOutputModes":["text/plain","application/json"],
                "skills":[
                    {"id":"greeting","name":"Greeting","description":"Returns a deterministic greeting.","tags":["greeting"]},
                    {"id":"get_availability","name":"Public availability","description":"Reads offered slots from this agent's public Google booking page, within a bounded window of up to 30 days.","tags":["calendar","availability"],"inputModes":["application/json"]},
                    {"id":"find_common_slot","name":"Find a common host slot","description":"Discovers a peer, requests real availability over A2A, and selects a complete host appointment covered by the peer's advertised intervals. Uses the host appointment duration. Does not reserve.","tags":["calendar","scheduling"],"inputModes":["application/json"]}
                ]
            })).expect("live card matches SDK schema");
        }
        serde_json::from_value(json!({
            "name": self.name,
            "description": format!("{}'s Calendar test agent. Shares mock availability and finds common slots; does not book appointments.", self.name),
            "version": "0.3.0",
            "supportedInterfaces": [{
                "url": format!("{base_url}/a2a"),
                "protocolBinding": "JSONRPC",
                "protocolVersion": "1.0",
                "tenant": self.id
            }],
            "capabilities": {},
            "defaultInputModes": ["text/plain", "application/json"],
            "defaultOutputModes": ["text/plain", "application/json"],
            "skills": [{
                "id": "greeting", "name": "Mock greeting",
                "description": "Returns a deterministic greeting from the selected agent.",
                "tags": ["mock", "greeting"], "examples": ["Hello"]
            }, {
                "id": "get_availability", "name": "Mock availability",
                "description": "Returns this agent's configured mock availability in UTC.",
                "tags": ["mock", "availability"], "inputModes": ["application/json"]
            }, {
                "id": "find_common_slot", "name": "Find a mock common slot",
                "description": "Discovers a peer in the configured catalog and asks it for availability over A2A.",
                "tags": ["mock", "scheduling"], "inputModes": ["application/json"]
            }]
        })).expect("static agent fixture must match SDK schema")
    }
}
