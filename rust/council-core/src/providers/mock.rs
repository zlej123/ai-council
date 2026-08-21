use async_trait::async_trait;

use crate::adapter::{AgentAdapter, CouncilResult};
use crate::model::{AgentId, Author, Intent, RoomEvent, RoomSnapshot};

pub struct MockAdapter {
    id: AgentId,
}

impl MockAdapter {
    pub const fn new(id: AgentId) -> Self {
        Self { id }
    }
}

#[async_trait]
impl AgentAdapter for MockAdapter {
    fn id(&self) -> AgentId {
        self.id
    }

    async fn evaluate(&self, _room: &RoomSnapshot, event: &RoomEvent) -> CouncilResult<Intent> {
        let intent = match (self.id, event.author) {
            (_, Author::You) => Intent::request_floor(event.id, "mock human-turn contribution"),
            (AgentId::Claude, Author::Agent(AgentId::Gpt)) => {
                Intent::request_floor(event.id, "mock follow-up contribution")
            }
            _ => Intent::pass(event.id),
        };
        Ok(intent)
    }

    async fn speak(&self, room: &RoomSnapshot, _intent: &Intent) -> CouncilResult<String> {
        let latest = room
            .events
            .last()
            .map(|event| event.content.as_str())
            .unwrap_or_default();
        let speech = match self.id {
            AgentId::Gpt => format!(
                "구조적으로 보면 ‘{latest}’에는 가설과 검증 기준을 먼저 분리해 보는 게 유용해요."
            ),
            AgentId::Claude => {
                "한 가지 덧붙이면, 그 기준이 실제 사용자 행동을 관찰하는지도 확인해야 해요. 기능이 작동하는 것과 대화가 자연스러운 것은 다르니까요.".to_owned()
            }
        };
        Ok(speech)
    }
}
