use crate::engine::{BarrierTrace, CycleOutcome};
use crate::metrics::MetricsReport;
use crate::model::RoomSnapshot;

pub fn barrier_line(barrier: &BarrierTrace) -> String {
    let details = barrier
        .dispositions
        .iter()
        .map(|(agent, disposition)| format!("{agent}={disposition:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let floor = barrier
        .floor_granted
        .map(|agent| agent.to_string())
        .unwrap_or_else(|| "none".to_owned());
    format!("event #{}: {details} · floor={floor}", barrier.event_id)
}

/// Renders one finished (or in-progress) session as reviewable Markdown.
/// The file is a convenience export for the human quality review in
/// EXPERIMENT.md §6; `Room.event_log` stays the only source of truth.
pub fn render_session_markdown(
    provider_label: &str,
    room: &RoomSnapshot,
    cycles: &[CycleOutcome],
    report: &MetricsReport,
) -> String {
    let mut out = String::new();
    out.push_str("# Council session transcript\n\n");
    out.push_str(&format!("- provider: {provider_label}\n"));
    out.push_str(&format!("- events: {}\n", room.events.len()));
    out.push_str(&format!("- cycles: {}\n", cycles.len()));

    out.push_str("\n## Events\n\n");
    for event in &room.events {
        out.push_str(&format!(
            "**#{} {}**\n{}\n\n",
            event.id, event.author, event.content
        ));
    }

    out.push_str("## Control trace\n\n```text\n");
    for (index, cycle) in cycles.iter().enumerate() {
        for barrier in &cycle.barriers {
            out.push_str(&barrier_line(barrier));
            out.push('\n');
            for (agent, reason) in &barrier.reasons {
                out.push_str(&format!("  {agent} wanted the floor: {reason}\n"));
            }
        }
        out.push_str(&format!(
            "cycle {}: stop={}\n",
            index + 1,
            cycle.stop_reason
        ));
    }
    out.push_str("```\n");

    out.push_str("\n## Metrics\n\n```json\n");
    out.push_str(
        &serde_json::to_string_pretty(report).unwrap_or_else(|_| "unrenderable".to_owned()),
    );
    out.push_str("\n```\n");
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::render_session_markdown;
    use crate::engine::{AgentDisposition, BarrierTrace, CycleOutcome, StopReason};
    use crate::metrics::Metrics;
    use crate::model::{AgentId, Author, Decision, RoomEvent, RoomSnapshot};

    #[test]
    fn renders_events_control_trace_and_metrics() {
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
        metrics.record_ai_streak(1);

        let markdown = render_session_markdown("mock", &room, &cycles, &metrics.report());

        assert!(markdown.contains("**#1 You**\n질문"));
        assert!(markdown.contains("**#2 GPT**\n답변"));
        assert!(markdown.contains("event #1: GPT=RequestFloor, Claude=Pass · floor=GPT"));
        assert!(markdown.contains("GPT wanted the floor: 새 관점"));
        assert!(markdown.contains("cycle 1: stop=QUIESCENT"));
        assert!(markdown.contains("\"pass_rate\": 0.5"));
    }
}
