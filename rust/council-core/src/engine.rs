use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use futures::future::join_all;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::adapter::{AgentAdapter, CouncilError, CouncilResult};
use crate::metrics::{Metrics, MetricsReport};
use crate::model::{
    AgentId, AgentState, Author, Decision, Intent, Room, RoomEvent, empty_agent_states,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AgentDisposition {
    Pass,
    RequestFloor,
    SyncOnly,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BarrierTrace {
    pub event_id: u64,
    pub dispositions: BTreeMap<AgentId, AgentDisposition>,
    /// Each requester's short internal reason, when the model gave one.
    #[serde(default)]
    pub reasons: BTreeMap<AgentId, String>,
    pub floor_granted: Option<AgentId>,
}

/// Observation-only stream of what the council is doing mid-cycle,
/// for UIs. Not part of the protocol.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Progress {
    BarrierStarted {
        event_id: u64,
    },
    Decided {
        agent: AgentId,
        disposition: AgentDisposition,
    },
    Speaking {
        agent: AgentId,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StopReason {
    Quiescent,
    AiStreakLimit,
    Cancelled,
}

impl fmt::Display for StopReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Quiescent => formatter.write_str("QUIESCENT"),
            Self::AiStreakLimit => formatter.write_str("AI_STREAK_LIMIT"),
            Self::Cancelled => formatter.write_str("CANCELLED"),
        }
    }
}

/// Resolves when a cancellation has been requested; never resolves when no
/// token was supplied. A permit stored by `notify_one` before the cycle
/// starts also counts, so a stop pressed early is not lost.
async fn cancelled(cancel: Option<&Arc<Notify>>) {
    match cancel {
        Some(notify) => notify.notified().await,
        None => std::future::pending().await,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CycleOutcome {
    pub appended_events: Vec<RoomEvent>,
    pub barriers: Vec<BarrierTrace>,
    pub stop_reason: StopReason,
}

pub struct Council {
    room: Room,
    roster: Vec<AgentId>,
    states: BTreeMap<AgentId, AgentState>,
    adapters: BTreeMap<AgentId, Arc<dyn AgentAdapter>>,
    floor_cursor: usize,
    max_ai_streak: u64,
    metrics: Metrics,
    event_sink: Option<tokio::sync::mpsc::UnboundedSender<RoomEvent>>,
    progress_sink: Option<tokio::sync::mpsc::UnboundedSender<Progress>>,
}

enum ProcessingResult {
    Evaluated(CouncilResult<Intent>),
    SyncOnly,
}

impl Council {
    pub fn new(adapters: Vec<Arc<dyn AgentAdapter>>, max_ai_streak: u64) -> CouncilResult<Self> {
        let mut by_id = BTreeMap::new();
        for adapter in adapters {
            let id = adapter.id();
            if by_id.insert(id, adapter).is_some() {
                return Err(CouncilError::new(format!("duplicate adapter for {id}")));
            }
        }
        if by_id.len() < 2 {
            return Err(CouncilError::new(
                "a council needs at least two AI participants",
            ));
        }
        let roster: Vec<AgentId> = AgentId::ORDER
            .into_iter()
            .filter(|agent| by_id.contains_key(agent))
            .collect();
        Ok(Self {
            room: Room::default(),
            states: empty_agent_states(&roster),
            roster,
            adapters: by_id,
            floor_cursor: 0,
            max_ai_streak,
            metrics: Metrics::default(),
            event_sink: None,
            progress_sink: None,
        })
    }

    /// Streams every committed RoomEvent to observers (e.g. a UI) as it
    /// happens. Observation only: the sink is not part of the protocol.
    pub fn set_event_sink(&mut self, sink: tokio::sync::mpsc::UnboundedSender<RoomEvent>) {
        self.event_sink = Some(sink);
    }

    pub fn set_progress_sink(&mut self, sink: tokio::sync::mpsc::UnboundedSender<Progress>) {
        self.progress_sink = Some(sink);
    }

    fn progress(&self, update: Progress) {
        if let Some(sink) = &self.progress_sink {
            let _ = sink.send(update);
        }
    }

    /// Seeds a fresh council with an existing conversation so a saved room
    /// can be continued. Only valid before any event is committed. Agent
    /// states stay empty; the next barrier re-evaluates everyone anyway.
    pub fn seed_room(&mut self, events: Vec<RoomEvent>) -> CouncilResult<()> {
        if !self.room.event_log.is_empty() {
            return Err(CouncilError::new("seed_room requires an empty room"));
        }
        for (index, event) in events.iter().enumerate() {
            if event.id != index as u64 + 1 {
                return Err(CouncilError::new("seeded events must have sequential ids"));
            }
        }
        for event in &events {
            if let Some(sink) = &self.event_sink {
                let _ = sink.send(event.clone());
            }
        }
        self.room.event_log = events;
        Ok(())
    }

    pub fn room(&self) -> &Room {
        &self.room
    }

    pub fn roster(&self) -> &[AgentId] {
        &self.roster
    }

    /// Roster with each seat's model label, in arbitration order.
    pub fn seats(&self) -> Vec<(AgentId, Option<String>)> {
        self.roster
            .iter()
            .map(|agent| (*agent, self.adapters[agent].model_label()))
            .collect()
    }

    pub fn agent_states(&self) -> &BTreeMap<AgentId, AgentState> {
        &self.states
    }

    pub fn metrics_report(&self) -> MetricsReport {
        self.metrics.report()
    }

    pub fn rate_naturalness(
        &mut self,
        score: u8,
        note: Option<String>,
    ) -> Result<(), &'static str> {
        self.metrics.rate_naturalness(score, note)
    }

    pub async fn submit_human(
        &mut self,
        content: impl Into<String>,
    ) -> CouncilResult<CycleOutcome> {
        self.submit_human_directed(content, None).await
    }

    /// Human speech, optionally directing the first reply at one agent.
    /// Direction is the human-priority rule, not content arbitration: the
    /// full listening barrier still runs, and later grants in the same
    /// cycle go back to normal round-robin.
    pub async fn submit_human_directed(
        &mut self,
        content: impl Into<String>,
        directed: Option<AgentId>,
    ) -> CouncilResult<CycleOutcome> {
        self.submit_human_cancellable(content, directed, None).await
    }

    /// Like `submit_human_directed`, but the cycle stops with
    /// `StopReason::Cancelled` as soon as `cancel` is notified. Cancellation
    /// is a clean stop: committed events stay, the round-robin cursor only
    /// moves when a speech actually commits, and the partial cycle is still
    /// returned so traces and metrics stay consistent with the room.
    pub async fn submit_human_cancellable(
        &mut self,
        content: impl Into<String>,
        directed: Option<AgentId>,
        cancel: Option<Arc<Notify>>,
    ) -> CouncilResult<CycleOutcome> {
        let content = content.into();
        if content.trim().is_empty() {
            return Err(CouncilError::new("human event cannot be empty"));
        }
        if let Some(target) = directed
            && !self.roster.contains(&target)
        {
            return Err(CouncilError::new(format!("{target} is not in this room")));
        }

        let human = self.commit_event(Author::You, content);
        let mut appended_events = vec![human.clone()];
        let mut barriers = Vec::new();
        let mut current = human;
        let mut ai_streak = 0;
        let mut directed = directed;

        let stop_reason = loop {
            let barrier = tokio::select! {
                biased;
                _ = cancelled(cancel.as_ref()) => None,
                result = self.process_event(&current) => Some(result),
            };
            let Some(barrier) = barrier else {
                break StopReason::Cancelled;
            };
            let mut trace = barrier?;
            let requesters = self.valid_requesters(current.id);

            if requesters.is_empty() && directed.is_none() {
                barriers.push(trace);
                break StopReason::Quiescent;
            }
            if ai_streak >= self.max_ai_streak {
                barriers.push(trace);
                break StopReason::AiStreakLimit;
            }

            // A directed grant bypasses round-robin once without moving the
            // cursor; everything after it is ordinary arbitration. The cursor
            // only advances once the granted speech actually commits.
            let (speaker, next_cursor) = match directed.take() {
                Some(target) => (target, self.floor_cursor),
                None => self.next_requester(&requesters),
            };
            trace.floor_granted = Some(speaker);
            barriers.push(trace);

            let intent = match self.states[&speaker].pending_intent.clone() {
                Some(intent) if intent.basis_event_id == current.id => intent,
                Some(_) => return Err(CouncilError::new("refusing to use stale intent")),
                None => Intent::request_floor(current.id, "directed by the human"),
            };
            let snapshot = self.room.snapshot();
            self.progress(Progress::Speaking { agent: speaker });
            let speech = tokio::select! {
                biased;
                _ = cancelled(cancel.as_ref()) => None,
                result = self.adapters[&speaker].speak(&snapshot, &intent) => Some(result),
            };
            let Some(speech) = speech else {
                break StopReason::Cancelled;
            };
            let speech = speech?;
            if speech.trim().is_empty() {
                return Err(CouncilError::new(format!(
                    "{speaker} produced an empty speech"
                )));
            }
            current = self.commit_event(Author::Agent(speaker), speech);
            self.floor_cursor = next_cursor;
            appended_events.push(current.clone());
            ai_streak += 1;
        };

        self.metrics.record_ai_streak(ai_streak);
        Ok(CycleOutcome {
            appended_events,
            barriers,
            stop_reason,
        })
    }

    fn commit_event(&mut self, author: Author, content: String) -> RoomEvent {
        for state in self.states.values_mut() {
            state.pending_intent = None;
        }
        let event = RoomEvent {
            id: self.room.latest_event_id().unwrap_or(0) + 1,
            author,
            content,
        };
        self.room.event_log.push(event.clone());
        if let Some(sink) = &self.event_sink {
            let _ = sink.send(event.clone());
        }
        event
    }

    async fn process_event(&mut self, event: &RoomEvent) -> CouncilResult<BarrierTrace> {
        let snapshot = self.room.snapshot();
        if snapshot.latest_event_id() != Some(event.id) {
            return Err(CouncilError::new(
                "listening barrier can only process the latest Room Event",
            ));
        }

        self.progress(Progress::BarrierStarted { event_id: event.id });
        let progress_sink = self.progress_sink.clone();
        let jobs = self.roster.iter().copied().map(|agent| {
            let adapter = Arc::clone(&self.adapters[&agent]);
            let event = event.clone();
            let snapshot = snapshot.clone();
            let progress_sink = progress_sink.clone();
            async move {
                let result = if event.author == Author::Agent(agent) {
                    ProcessingResult::SyncOnly
                } else {
                    ProcessingResult::Evaluated(adapter.evaluate(&snapshot, &event).await)
                };
                if let Some(sink) = &progress_sink {
                    let disposition = match &result {
                        ProcessingResult::SyncOnly => Some(AgentDisposition::SyncOnly),
                        ProcessingResult::Evaluated(Ok(intent)) => Some(match intent.decision {
                            Decision::Pass => AgentDisposition::Pass,
                            Decision::RequestFloor => AgentDisposition::RequestFloor,
                        }),
                        ProcessingResult::Evaluated(Err(_)) => None,
                    };
                    if let Some(disposition) = disposition {
                        let _ = sink.send(Progress::Decided { agent, disposition });
                    }
                }
                (agent, result)
            }
        });

        let mut dispositions = BTreeMap::new();
        let mut reasons = BTreeMap::new();
        let mut completed_decisions = Vec::new();
        let mut first_error = None;

        for (agent, processing) in join_all(jobs).await {
            let state = self
                .states
                .get_mut(&agent)
                .expect("all roster agents have state");
            match processing {
                ProcessingResult::SyncOnly => {
                    state.last_heard_event = Some(event.id);
                    state.pending_intent = None;
                    state.sync_only_count += 1;
                    dispositions.insert(agent, AgentDisposition::SyncOnly);
                }
                ProcessingResult::Evaluated(Ok(intent)) => {
                    if intent.basis_event_id != event.id {
                        state.error_count += 1;
                        first_error.get_or_insert_with(|| {
                            CouncilError::provider(agent, "intent has wrong basis_event_id")
                        });
                        continue;
                    }
                    state.last_heard_event = Some(event.id);
                    state.evaluation_count += 1;
                    completed_decisions.push(intent.decision);
                    if let Some(reason) = &intent.reason {
                        reasons.insert(agent, reason.clone());
                    }
                    dispositions.insert(
                        agent,
                        match intent.decision {
                            Decision::Pass => AgentDisposition::Pass,
                            Decision::RequestFloor => AgentDisposition::RequestFloor,
                        },
                    );
                    state.pending_intent = Some(intent);
                }
                ProcessingResult::Evaluated(Err(error)) => {
                    state.error_count += 1;
                    first_error.get_or_insert(error);
                }
            }
        }

        self.metrics.record_barrier(&completed_decisions);
        if let Some(error) = first_error {
            return Err(error);
        }
        if self
            .states
            .values()
            .any(|state| state.last_heard_event != Some(event.id))
        {
            return Err(CouncilError::new(format!(
                "listening barrier incomplete for event #{}",
                event.id
            )));
        }

        Ok(BarrierTrace {
            event_id: event.id,
            dispositions,
            reasons,
            floor_granted: None,
        })
    }

    fn valid_requesters(&self, event_id: u64) -> Vec<AgentId> {
        self.roster
            .iter()
            .copied()
            .filter(|agent| {
                self.states[agent]
                    .pending_intent
                    .as_ref()
                    .is_some_and(|intent| {
                        intent.basis_event_id == event_id
                            && intent.decision == Decision::RequestFloor
                    })
            })
            .collect()
    }

    /// Round-robin pick plus the cursor value to adopt once that speaker's
    /// speech commits. Callers must not move the cursor before the commit.
    fn next_requester(&self, requesters: &[AgentId]) -> (AgentId, usize) {
        for offset in 0..self.roster.len() {
            let index = (self.floor_cursor + offset) % self.roster.len();
            let candidate = self.roster[index];
            if requesters.contains(&candidate) {
                return (candidate, (index + 1) % self.roster.len());
            }
        }
        unreachable!("next_requester is called with at least one roster agent")
    }
}
