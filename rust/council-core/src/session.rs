//! The JSON sidecar written next to each Markdown transcript.
//!
//! The sidecar is a review copy, not a store: `Room.event_log` stays the only
//! source of truth (EXPERIMENT.md §2, §3). It exists so a past room can be
//! listed, read, re-seeded, and — via [`crate::review`] — scored and compared
//! against the other topics in the quality review.

use serde::{Deserialize, Serialize};

use crate::engine::CycleOutcome;
use crate::metrics::MetricsReport;
use crate::model::{AgentId, RoomSnapshot};
use crate::transcript::barrier_line;

#[derive(Clone, Serialize, Deserialize)]
pub struct UiEvent {
    pub id: u64,
    pub author: String,
    pub content: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct UiBarrier {
    pub line: String,
    #[serde(default)]
    pub reasons: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct UiCycle {
    pub barriers: Vec<UiBarrier>,
    pub stop: String,
    /// Id of the last event this cycle appended; the page inserts the
    /// control trace right after that event.
    #[serde(default)]
    pub last_event_id: u64,
}

impl UiCycle {
    /// One finished cycle as the page shows it: the control trace lines, why
    /// anyone asked for the floor, and where to slot the trace in the thread.
    pub fn from_outcome(outcome: &CycleOutcome) -> Self {
        UiCycle {
            barriers: outcome
                .barriers
                .iter()
                .map(|barrier| UiBarrier {
                    line: barrier_line(barrier),
                    reasons: barrier
                        .reasons
                        .iter()
                        .map(|(agent, reason)| format!("{agent}: {reason}"))
                        .collect(),
                })
                .collect(),
            stop: outcome.stop_reason.to_string(),
            last_event_id: outcome
                .appended_events
                .last()
                .map(|event| event.id)
                .unwrap_or(0),
        }
    }
}

/// What a human reviewer decided about a session after reading it.
///
/// Defaulted so every sidecar written before the review board still loads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewAnnotation {
    /// Held out of the quality totals — a session that went wrong in a way that
    /// says nothing about floor arbitration, such as an AI answering in the
    /// wrong persona.
    #[serde(default)]
    pub excluded: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Sidecar JSON written next to each Markdown transcript so a room can be
/// listed, viewed, resumed, and reviewed later.
#[derive(Serialize, Deserialize)]
pub struct SessionRecord {
    pub saved_unix: u64,
    pub provider: String,
    pub roster: Vec<String>,
    pub events: Vec<UiEvent>,
    pub cycles: Vec<UiCycle>,
    pub metrics: serde_json::Value,
    #[serde(default)]
    pub review: ReviewAnnotation,
}

impl SessionRecord {
    /// Builds the sidecar for a finished (or in-progress) session from the same
    /// inputs the Markdown transcript is rendered from, so the CLI and the web
    /// UI write the same shape and both end up on the review board.
    pub fn from_session(
        saved_unix: u64,
        provider_label: &str,
        roster: &[String],
        room: &RoomSnapshot,
        cycles: &[CycleOutcome],
        metrics: &MetricsReport,
    ) -> Self {
        SessionRecord {
            saved_unix,
            provider: provider_label.to_owned(),
            roster: roster.to_vec(),
            events: room
                .events
                .iter()
                .map(|event| UiEvent {
                    id: event.id,
                    author: event.author.to_string(),
                    content: event.content.clone(),
                })
                .collect(),
            cycles: cycles.iter().map(UiCycle::from_outcome).collect(),
            metrics: serde_json::to_value(metrics).unwrap_or(serde_json::Value::Null),
            // Freshly saved, so not reviewed yet; the review board annotates
            // finished sidecars.
            review: ReviewAnnotation::default(),
        }
    }

    /// Reads a saved Markdown transcript back into a record, so sessions written
    /// before the sidecar existed — and every CLI session on disk today — can
    /// still be reviewed. `roster` is recovered from the control trace, which
    /// names every agent that was evaluated.
    pub fn from_markdown(markdown: &str, saved_unix: u64) -> Option<Self> {
        if !markdown.starts_with("# Council session transcript") {
            return None;
        }
        let provider = markdown
            .lines()
            .find_map(|line| line.strip_prefix("- provider: "))?
            .trim()
            .to_owned();

        let events = parse_events(between(markdown, "## Events\n\n", "\n## Control trace")?);
        let trace = between(markdown, "## Control trace\n\n```text\n", "```\n")?;
        let cycles = parse_cycles(trace);
        let metrics = serde_json::from_str(between(markdown, "## Metrics\n\n```json\n", "\n```")?)
            .unwrap_or(serde_json::Value::Null);

        Some(SessionRecord {
            saved_unix,
            provider,
            roster: roster_from_trace(&cycles),
            events,
            cycles,
            metrics,
            review: ReviewAnnotation::default(),
        })
    }
}

/// The slice between two markers, or `None` when the transcript does not have
/// that section.
fn between<'a>(text: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let rest = &text[text.find(start)? + start.len()..];
    Some(&rest[..rest.find(end)?])
}

/// `**#3 Claude**` — the line the renderer puts above every utterance.
fn parse_event_header(line: &str) -> Option<(u64, String)> {
    let header = line.strip_prefix("**#")?.strip_suffix("**")?;
    let (id, author) = header.split_once(' ')?;
    Some((id.parse().ok()?, author.to_owned()))
}

fn parse_events(section: &str) -> Vec<UiEvent> {
    let mut events: Vec<UiEvent> = Vec::new();
    for line in section.lines() {
        if let Some((id, author)) = parse_event_header(line) {
            events.push(UiEvent {
                id,
                author,
                content: String::new(),
            });
            continue;
        }
        if let Some(event) = events.last_mut() {
            event.content.push_str(line);
            event.content.push('\n');
        }
    }
    for event in &mut events {
        // The renderer separates utterances with a blank line; that separator
        // is not part of what anyone said.
        while event.content.ends_with('\n') {
            event.content.pop();
        }
    }
    events
}

fn parse_cycles(trace: &str) -> Vec<UiCycle> {
    let mut cycles = Vec::new();
    let mut barriers: Vec<UiBarrier> = Vec::new();
    for line in trace.lines() {
        if let Some(rest) = line.strip_prefix("cycle ") {
            let stop = rest
                .split_once(": stop=")
                .map(|(_, stop)| stop.to_owned())
                .unwrap_or_default();
            // The Markdown does not carry the cycle's last appended event id;
            // the highest event the cycle evaluated is the same place the page
            // wants the trace shown.
            let last_event_id = barriers
                .iter()
                .filter_map(|barrier| barrier_event_id(&barrier.line))
                .max()
                .unwrap_or(0);
            cycles.push(UiCycle {
                barriers: std::mem::take(&mut barriers),
                stop,
                last_event_id,
            });
        } else if line.starts_with("event #") {
            barriers.push(UiBarrier {
                line: line.to_owned(),
                reasons: Vec::new(),
            });
        } else if let (Some((agent, reason)), Some(barrier)) = (
            line.trim().split_once(" wanted the floor: "),
            barriers.last_mut(),
        ) {
            barrier.reasons.push(format!("{agent}: {reason}"));
        }
    }
    cycles
}

fn barrier_event_id(line: &str) -> Option<u64> {
    between(line, "event #", ":")?.parse().ok()
}

/// Every agent named in the control trace, in the global fixed order.
fn roster_from_trace(cycles: &[UiCycle]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for cycle in cycles {
        for barrier in &cycle.barriers {
            let Some(body) = between(&barrier.line, ": ", " · floor=") else {
                continue;
            };
            for pair in body.split(", ") {
                let Some((agent, _)) = pair.split_once('=') else {
                    continue;
                };
                if !seen.iter().any(|known| known == agent) {
                    seen.push(agent.to_owned());
                }
            }
        }
    }
    AgentId::ORDER
        .iter()
        .map(ToString::to_string)
        .filter(|name| seen.iter().any(|known| known == name))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::engine::{AgentDisposition, BarrierTrace, StopReason};
    use crate::metrics::Metrics;
    use crate::model::{AgentId, Author, Decision, RoomEvent};

    fn fixture() -> (RoomSnapshot, Vec<CycleOutcome>, Metrics) {
        let room = RoomSnapshot {
            events: vec![
                RoomEvent {
                    id: 1,
                    author: Author::You,
                    content: "질문".to_owned(),
                },
                RoomEvent {
                    id: 2,
                    author: Author::Agent(AgentId::Gpt),
                    content: "답변".to_owned(),
                },
            ],
        };
        let cycles = vec![CycleOutcome {
            appended_events: room.events.clone(),
            barriers: vec![BarrierTrace {
                event_id: 1,
                dispositions: BTreeMap::from([
                    (AgentId::Gpt, AgentDisposition::RequestFloor),
                    (AgentId::Claude, AgentDisposition::Pass),
                ]),
                reasons: BTreeMap::from([(AgentId::Gpt, "새 관점".to_owned())]),
                floor_granted: Some(AgentId::Gpt),
            }],
            stop_reason: StopReason::Quiescent,
        }];
        let mut metrics = Metrics::default();
        metrics.record_barrier(&[Decision::RequestFloor, Decision::Pass]);
        (room, cycles, metrics)
    }

    #[test]
    fn a_record_carries_the_events_the_control_trace_and_the_metrics() {
        let (room, cycles, metrics) = fixture();

        let record = SessionRecord::from_session(
            99,
            "subscription CLIs",
            &["GPT".to_owned(), "Claude".to_owned()],
            &room,
            &cycles,
            &metrics.report(),
        );

        assert_eq!(record.saved_unix, 99);
        assert_eq!(record.provider, "subscription CLIs");
        assert_eq!(record.events.len(), 2);
        assert_eq!(record.events[1].author, "GPT");
        assert_eq!(
            record.cycles[0].barriers[0].line,
            "event #1: GPT=RequestFloor, Claude=Pass · floor=GPT"
        );
        assert_eq!(record.cycles[0].barriers[0].reasons, vec!["GPT: 새 관점"]);
        assert_eq!(record.cycles[0].stop, "QUIESCENT");
        assert_eq!(record.cycles[0].last_event_id, 2);
        assert_eq!(record.metrics["pass_rate"], serde_json::json!(0.5));
        assert!(!record.review.excluded);
    }
    #[test]
    fn a_saved_markdown_transcript_reads_back_into_the_same_record() {
        let (room, cycles, metrics) = fixture();
        let markdown = crate::transcript::render_session_markdown(
            "subscription CLIs",
            &room,
            &cycles,
            &metrics.report(),
        );

        let parsed = SessionRecord::from_markdown(&markdown, 42).expect("parsed");
        let direct = SessionRecord::from_session(
            42,
            "subscription CLIs",
            &["GPT".to_owned(), "Claude".to_owned()],
            &room,
            &cycles,
            &metrics.report(),
        );

        assert_eq!(parsed.saved_unix, 42);
        assert_eq!(parsed.provider, direct.provider);
        assert_eq!(parsed.events.len(), direct.events.len());
        assert_eq!(parsed.events[0].author, "You");
        assert_eq!(parsed.events[0].content, "질문");
        assert_eq!(parsed.events[1].author, "GPT");
        assert_eq!(parsed.metrics, direct.metrics);
        assert_eq!(parsed.cycles.len(), 1);
        assert_eq!(
            parsed.cycles[0].barriers[0].line,
            direct.cycles[0].barriers[0].line
        );
        assert_eq!(parsed.cycles[0].barriers[0].reasons, vec!["GPT: 새 관점"]);
        assert_eq!(parsed.cycles[0].stop, "QUIESCENT");
        // Recovered from the trace, in the global fixed order.
        assert_eq!(parsed.roster, vec!["GPT", "Claude"]);
    }

    #[test]
    fn an_utterance_spanning_blank_lines_survives_the_round_trip() {
        let long = "첫 문단.\n\n둘째 문단은 빈 줄 뒤에 온다.";
        let room = RoomSnapshot {
            events: vec![RoomEvent {
                id: 1,
                author: Author::You,
                content: long.to_owned(),
            }],
        };
        let markdown = crate::transcript::render_session_markdown(
            "mock",
            &room,
            &[],
            &Metrics::default().report(),
        );

        let parsed = SessionRecord::from_markdown(&markdown, 1).expect("parsed");

        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.events[0].content, long);
    }

    #[test]
    fn text_that_is_not_a_transcript_is_refused() {
        assert!(SessionRecord::from_markdown("# 그냥 메모\n\n내용", 1).is_none());
    }
}
