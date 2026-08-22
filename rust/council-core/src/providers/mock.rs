use async_trait::async_trait;

use crate::adapter::{AgentAdapter, CouncilResult};
use crate::model::{AgentId, Author, Intent, RoomEvent, RoomSnapshot};

pub struct MockAdapter {
    id: AgentId,
    roster: Vec<AgentId>,
}

impl MockAdapter {
    /// `roster` is the session's full AI roster so the deterministic
    /// follow-up rule works for any subset of the fixed order.
    pub fn new(id: AgentId, roster: &[AgentId]) -> Self {
        let mut roster: Vec<AgentId> = roster.to_vec();
        roster.sort();
        Self { id, roster }
    }

    /// The roster member one place after `author`, if any.
    fn follows(&self, author: AgentId) -> Option<AgentId> {
        let position = self.roster.iter().position(|agent| *agent == author)?;
        self.roster.get(position + 1).copied()
    }
}

#[async_trait]
impl AgentAdapter for MockAdapter {
    fn model_label(&self) -> Option<String> {
        Some("mock".to_owned())
    }

    fn id(&self) -> AgentId {
        self.id
    }

    async fn evaluate(&self, room: &RoomSnapshot, event: &RoomEvent) -> CouncilResult<Intent> {
        // Deterministic demo: everyone wants the human's opening, and exactly
        // the next roster member follows up the FIRST AI speech of the turn,
        // so every roster produces one contention, one follow-up, then
        // quiescence.
        let ai_speeches_this_turn = room
            .events
            .iter()
            .rev()
            .take_while(|event| event.author != Author::You)
            .count();
        let intent = match event.author {
            Author::You => Intent::request_floor(event.id, "mock human-turn contribution"),
            Author::Agent(author)
                if ai_speeches_this_turn == 1 && self.follows(author) == Some(self.id) =>
            {
                Intent::request_floor(event.id, "mock follow-up contribution")
            }
            Author::Agent(_) => Intent::pass(event.id),
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
            AgentId::Gemini => {
                "근거 자료 쪽에서 보면, 비슷한 다중 에이전트 실험들이 이미 있으니 그 결과와 비교해볼 수 있어요.".to_owned()
            }
            AgentId::Grok => {
                "반대로 생각하면, AI끼리의 합의가 오히려 편향을 강화할 수도 있다는 점은 짚고 가야 해요.".to_owned()
            }
        };
        Ok(speech)
    }
}
