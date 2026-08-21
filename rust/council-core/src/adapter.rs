use std::error::Error;
use std::fmt;

use async_trait::async_trait;

use crate::model::{AgentId, Intent, RoomEvent, RoomSnapshot};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CouncilError {
    message: String,
}

impl CouncilError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn provider(agent: AgentId, message: impl fmt::Display) -> Self {
        Self::new(format!("{agent} provider error: {message}"))
    }
}

impl fmt::Display for CouncilError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CouncilError {}

impl From<serde_json::Error> for CouncilError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(format!("JSON error: {error}"))
    }
}

pub type CouncilResult<T> = Result<T, CouncilError>;

#[async_trait]
pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> AgentId;

    async fn evaluate(&self, room: &RoomSnapshot, event: &RoomEvent) -> CouncilResult<Intent>;

    async fn speak(&self, room: &RoomSnapshot, intent: &Intent) -> CouncilResult<String>;
}
