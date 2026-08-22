use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use council_core::{
    AgentAdapter, AgentId, Author, Council, CouncilError, CouncilResult, Decision, Intent,
    RoomEvent, RoomSnapshot, StopReason,
};
use tokio::sync::Barrier;
use tokio::time::{Duration, timeout};

struct TestAdapter {
    id: AgentId,
    human_decision: Decision,
    other_decision: Decision,
    human_barrier: Option<Arc<Barrier>>,
    fail_event: Option<u64>,
    evaluated_events: Mutex<Vec<u64>>,
}

impl TestAdapter {
    fn new(id: AgentId, human_decision: Decision, other_decision: Decision) -> Self {
        Self {
            id,
            human_decision,
            other_decision,
            human_barrier: None,
            fail_event: None,
            evaluated_events: Mutex::new(Vec::new()),
        }
    }

    fn with_human_barrier(mut self, barrier: Arc<Barrier>) -> Self {
        self.human_barrier = Some(barrier);
        self
    }

    fn failing_on(mut self, event_id: u64) -> Self {
        self.fail_event = Some(event_id);
        self
    }

    fn evaluated_events(&self) -> Vec<u64> {
        self.evaluated_events.lock().unwrap().clone()
    }
}

#[async_trait]
impl AgentAdapter for TestAdapter {
    fn id(&self) -> AgentId {
        self.id
    }

    async fn evaluate(&self, _room: &RoomSnapshot, event: &RoomEvent) -> CouncilResult<Intent> {
        self.evaluated_events.lock().unwrap().push(event.id);
        if event.author == Author::You
            && let Some(barrier) = &self.human_barrier
        {
            barrier.wait().await;
        }
        if self.fail_event == Some(event.id) {
            return Err(CouncilError::provider(self.id, "deliberate test failure"));
        }
        let decision = if event.author == Author::You {
            self.human_decision
        } else {
            self.other_decision
        };
        Ok(Intent {
            basis_event_id: event.id,
            decision,
            reason: Some("test decision".to_owned()),
        })
    }

    async fn speak(&self, _room: &RoomSnapshot, _intent: &Intent) -> CouncilResult<String> {
        Ok(format!("{} test contribution", self.id))
    }
}

fn council(gpt: Arc<TestAdapter>, claude: Arc<TestAdapter>, max_streak: u64) -> Council {
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![gpt, claude];
    Council::new(adapters, max_streak).unwrap()
}

#[tokio::test]
async fn both_agents_cross_the_human_event_barrier_concurrently() {
    let barrier = Arc::new(Barrier::new(2));
    let gpt = Arc::new(
        TestAdapter::new(AgentId::Gpt, Decision::Pass, Decision::Pass)
            .with_human_barrier(Arc::clone(&barrier)),
    );
    let claude = Arc::new(
        TestAdapter::new(AgentId::Claude, Decision::Pass, Decision::Pass)
            .with_human_barrier(barrier),
    );
    let mut council = council(Arc::clone(&gpt), Arc::clone(&claude), 3);

    let outcome = timeout(Duration::from_secs(1), council.submit_human("hello"))
        .await
        .expect("parallel evaluation should let both adapters reach the barrier")
        .unwrap();

    assert_eq!(outcome.stop_reason, StopReason::Quiescent);
    assert_eq!(gpt.evaluated_events(), vec![1]);
    assert_eq!(claude.evaluated_events(), vec![1]);
    assert!(
        council
            .agent_states()
            .values()
            .all(|state| state.last_heard_event == Some(1))
    );
}

#[tokio::test]
async fn a_new_speech_invalidates_the_loser_and_forces_re_evaluation() {
    let gpt = Arc::new(TestAdapter::new(
        AgentId::Gpt,
        Decision::RequestFloor,
        Decision::Pass,
    ));
    let claude = Arc::new(TestAdapter::new(
        AgentId::Claude,
        Decision::RequestFloor,
        Decision::Pass,
    ));
    let mut council = council(Arc::clone(&gpt), Arc::clone(&claude), 3);

    let outcome = council.submit_human("topic").await.unwrap();

    assert_eq!(
        outcome
            .appended_events
            .iter()
            .map(|event| event.author)
            .collect::<Vec<_>>(),
        vec![Author::You, Author::Agent(AgentId::Gpt)]
    );
    assert_eq!(claude.evaluated_events(), vec![1, 2]);
    assert_eq!(gpt.evaluated_events(), vec![1]);
    let claude_intent = council.agent_states()[&AgentId::Claude]
        .pending_intent
        .as_ref()
        .unwrap();
    assert_eq!(claude_intent.basis_event_id, 2);
    assert_eq!(claude_intent.decision, Decision::Pass);
    assert_eq!(
        council.agent_states()[&AgentId::Gpt].last_heard_event,
        Some(2)
    );
}

#[tokio::test]
async fn simultaneous_requests_are_granted_round_robin_without_meaning_scores() {
    let gpt = Arc::new(TestAdapter::new(
        AgentId::Gpt,
        Decision::RequestFloor,
        Decision::Pass,
    ));
    let claude = Arc::new(TestAdapter::new(
        AgentId::Claude,
        Decision::RequestFloor,
        Decision::Pass,
    ));
    let mut council = council(gpt, claude, 3);

    let first = council.submit_human("first").await.unwrap();
    let second = council.submit_human("second").await.unwrap();

    assert_eq!(first.barriers[0].floor_granted, Some(AgentId::Gpt));
    assert_eq!(second.barriers[0].floor_granted, Some(AgentId::Claude));
    assert_eq!(
        council.metrics_report().simultaneous_request_rate,
        Some(1.0)
    );
}

#[tokio::test]
async fn four_agent_roster_grants_round_robin_in_fixed_global_order() {
    let make = |id| Arc::new(TestAdapter::new(id, Decision::RequestFloor, Decision::Pass));
    let adapters: Vec<Arc<dyn AgentAdapter>> = vec![
        make(AgentId::Grok),
        make(AgentId::Gpt),
        make(AgentId::Gemini),
        make(AgentId::Claude),
    ];
    let mut council = Council::new(adapters, 3).unwrap();

    let first = council.submit_human("one").await.unwrap();
    let second = council.submit_human("two").await.unwrap();
    let third = council.submit_human("three").await.unwrap();

    // Insertion order was shuffled; arbitration still follows the global order.
    assert_eq!(first.barriers[0].floor_granted, Some(AgentId::Gpt));
    assert_eq!(second.barriers[0].floor_granted, Some(AgentId::Claude));
    assert_eq!(third.barriers[0].floor_granted, Some(AgentId::Gemini));
    assert!(
        council
            .agent_states()
            .values()
            .all(|state| state.last_heard_event == Some(6))
    );
    // 3 human barriers were 4-way contentions; the 3 AI-speech barriers had
    // three evaluators each and no contention.
    assert_eq!(council.metrics_report().multi_evaluation_barriers, 6);
    assert_eq!(
        council.metrics_report().simultaneous_request_rate,
        Some(0.5)
    );
}

#[tokio::test]
async fn directed_human_speech_grants_the_target_even_if_it_passed() {
    let gpt = Arc::new(TestAdapter::new(
        AgentId::Gpt,
        Decision::RequestFloor,
        Decision::Pass,
    ));
    // Claude passes on human events, so only direction can make it speak.
    let claude = Arc::new(TestAdapter::new(
        AgentId::Claude,
        Decision::Pass,
        Decision::Pass,
    ));
    let mut council = council(gpt, claude, 3);

    let outcome = council
        .submit_human_directed("클로드, 네 생각은?", Some(AgentId::Claude))
        .await
        .unwrap();

    assert_eq!(outcome.barriers[0].floor_granted, Some(AgentId::Claude));
    assert_eq!(
        outcome.appended_events[1].author,
        Author::Agent(AgentId::Claude)
    );
    // The directed grant must not advance the round-robin cursor: the next
    // ordinary contention still starts at GPT.
    let next = council.submit_human("다음 주제").await.unwrap();
    assert_eq!(next.barriers[0].floor_granted, Some(AgentId::Gpt));
}

#[tokio::test]
async fn seeded_room_continues_with_sequential_ids() {
    let gpt = Arc::new(TestAdapter::new(
        AgentId::Gpt,
        Decision::RequestFloor,
        Decision::Pass,
    ));
    let claude = Arc::new(TestAdapter::new(
        AgentId::Claude,
        Decision::Pass,
        Decision::Pass,
    ));
    let mut council = council(gpt, claude, 3);

    council
        .seed_room(vec![
            RoomEvent {
                id: 1,
                author: Author::You,
                content: "이전 대화".to_owned(),
            },
            RoomEvent {
                id: 2,
                author: Author::Agent(AgentId::Gpt),
                content: "이전 답변".to_owned(),
            },
        ])
        .unwrap();

    let outcome = council.submit_human("이어서 가자").await.unwrap();
    assert_eq!(outcome.appended_events[0].id, 3);
    assert_eq!(council.room().event_log.len(), 4);
    assert!(council.seed_room(Vec::new()).is_err());
}

#[tokio::test]
async fn provider_failure_fails_closed_before_any_floor_grant() {
    let gpt =
        Arc::new(TestAdapter::new(AgentId::Gpt, Decision::Pass, Decision::Pass).failing_on(1));
    let claude = Arc::new(TestAdapter::new(
        AgentId::Claude,
        Decision::RequestFloor,
        Decision::Pass,
    ));
    let mut council = council(gpt, claude, 3);

    let error = council.submit_human("topic").await.unwrap_err();

    assert!(error.to_string().contains("deliberate test failure"));
    assert_eq!(council.room().event_log.len(), 1);
    assert_eq!(council.agent_states()[&AgentId::Gpt].last_heard_event, None);
    assert_eq!(
        council.agent_states()[&AgentId::Claude].last_heard_event,
        Some(1)
    );
}

#[tokio::test]
async fn streak_limit_still_processes_the_final_committed_event() {
    let gpt = Arc::new(TestAdapter::new(
        AgentId::Gpt,
        Decision::RequestFloor,
        Decision::RequestFloor,
    ));
    let claude = Arc::new(TestAdapter::new(
        AgentId::Claude,
        Decision::Pass,
        Decision::RequestFloor,
    ));
    let mut council = council(gpt, claude, 3);

    let outcome = council.submit_human("keep going").await.unwrap();

    assert_eq!(outcome.stop_reason, StopReason::AiStreakLimit);
    assert_eq!(outcome.appended_events.len(), 4);
    assert!(
        council
            .agent_states()
            .values()
            .all(|state| state.last_heard_event == Some(4))
    );
    assert_eq!(council.metrics_report().max_ai_streak, Some(3));
    assert_eq!(council.metrics_report().pass_rate, Some(0.2));

    council
        .rate_naturalness(4, Some("felt like a group".to_owned()))
        .unwrap();
    assert_eq!(council.metrics_report().naturalness_average, Some(4.0));
}
