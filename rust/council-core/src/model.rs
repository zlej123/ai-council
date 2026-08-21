use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum AgentId {
    Gpt,
    Claude,
}

impl AgentId {
    pub const ORDER: [Self; 2] = [Self::Gpt, Self::Claude];

    pub const fn index(self) -> usize {
        match self {
            Self::Gpt => 0,
            Self::Claude => 1,
        }
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gpt => write!(formatter, "GPT"),
            Self::Claude => write!(formatter, "Claude"),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "agent")]
pub enum Author {
    You,
    Agent(AgentId),
}

impl fmt::Display for Author {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::You => write!(formatter, "You"),
            Self::Agent(agent) => write!(formatter, "{agent}"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RoomEvent {
    pub id: u64,
    pub author: Author,
    pub content: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Room {
    pub event_log: Vec<RoomEvent>,
}

impl Room {
    pub fn latest_event_id(&self) -> Option<u64> {
        self.event_log.last().map(|event| event.id)
    }

    pub fn snapshot(&self) -> RoomSnapshot {
        RoomSnapshot {
            events: self.event_log.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RoomSnapshot {
    pub events: Vec<RoomEvent>,
}

impl RoomSnapshot {
    pub fn latest_event_id(&self) -> Option<u64> {
        self.events.last().map(|event| event.id)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Decision {
    Pass,
    RequestFloor,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Intent {
    pub basis_event_id: u64,
    pub decision: Decision,
    pub reason: Option<String>,
}

impl Intent {
    pub fn pass(basis_event_id: u64) -> Self {
        Self {
            basis_event_id,
            decision: Decision::Pass,
            reason: None,
        }
    }

    pub fn request_floor(basis_event_id: u64, reason: impl Into<String>) -> Self {
        Self {
            basis_event_id,
            decision: Decision::RequestFloor,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentState {
    pub last_heard_event: Option<u64>,
    pub pending_intent: Option<Intent>,
    pub evaluation_count: u64,
    pub sync_only_count: u64,
    pub error_count: u64,
}

pub fn empty_agent_states() -> BTreeMap<AgentId, AgentState> {
    AgentId::ORDER
        .into_iter()
        .map(|agent| (agent, AgentState::default()))
        .collect()
}
