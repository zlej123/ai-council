//! Combines the saved session sidecars into the one view EXPERIMENT.md §6 asks
//! for: at least ten different topics read together, with the naturalness
//! average, PASS rate, simultaneous REQUEST rate, and streak distribution laid
//! side by side.
//!
//! This is a review layer over transcripts, not part of the protocol. Nothing
//! here touches the room, the barrier, or floor arbitration.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::metrics::{MetricsReport, NaturalnessRating};
use crate::session::{ReviewAnnotation, SessionRecord};

/// One saved session, reduced to what the review table shows.
#[derive(Clone, Debug, Serialize)]
pub struct SessionSummary {
    pub file: String,
    pub saved_unix: u64,
    pub provider: String,
    pub roster: Vec<String>,
    /// The human utterance that opened the room — the topic under review.
    pub topic: String,
    pub events: usize,
    pub metrics: MetricsReport,
    pub review: ReviewAnnotation,
}

/// The four §6 numbers over every session still included in the review.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReviewAggregate {
    pub total_sessions: usize,
    pub included_sessions: usize,
    pub rated_sessions: usize,
    pub decisions: u64,
    pub pass_rate: Option<f64>,
    pub multi_evaluation_barriers: u64,
    pub simultaneous_request_rate: Option<f64>,
    pub ai_streak_count: u64,
    pub mean_ai_streak: Option<f64>,
    pub max_ai_streak: Option<u64>,
    pub ai_streak_histogram: BTreeMap<u64, u64>,
    pub naturalness_average: Option<f64>,
}

/// Reduces one sidecar to a review row. Returns `None` when the stored metrics
/// cannot be read, so one unreadable file never sinks the whole board.
pub fn summarize(file: &str, record: &SessionRecord) -> Option<SessionSummary> {
    let metrics: MetricsReport = serde_json::from_value(record.metrics.clone()).ok()?;
    let topic = record
        .events
        .iter()
        .find(|event| event.author == "You")
        .or_else(|| record.events.first())
        .map(|event| event.content.chars().take(120).collect())
        .unwrap_or_default();
    Some(SessionSummary {
        file: file.to_owned(),
        saved_unix: record.saved_unix,
        provider: record.provider.clone(),
        roster: record.roster.clone(),
        topic,
        events: record.events.len(),
        metrics,
        review: record.review.clone(),
    })
}

/// Recovers a stored rate's numerator. `MetricsReport` keeps the rate and its
/// denominator but not the count, and both counts are small integers, so
/// multiplying back and rounding is exact over any session we can produce.
fn count_behind(rate: Option<f64>, denominator: u64) -> u64 {
    rate.map(|rate| (rate * denominator as f64).round() as u64)
        .unwrap_or(0)
}

/// Combines the included summaries. Rates are recombined from their counts —
/// averaging the per-session rates would weight a two-decision session as
/// heavily as a forty-decision one.
pub fn aggregate(summaries: &[SessionSummary]) -> ReviewAggregate {
    let included = summaries.iter().filter(|summary| !summary.review.excluded);

    let mut decisions = 0;
    let mut passes = 0;
    let mut barriers = 0;
    let mut simultaneous = 0;
    let mut histogram: BTreeMap<u64, u64> = BTreeMap::new();
    let mut scores: Vec<f64> = Vec::new();
    let mut rated_sessions = 0;
    let mut included_sessions = 0;

    for summary in included {
        let metrics = &summary.metrics;
        included_sessions += 1;
        decisions += metrics.decisions;
        passes += count_behind(metrics.pass_rate, metrics.decisions);
        barriers += metrics.multi_evaluation_barriers;
        simultaneous += count_behind(
            metrics.simultaneous_request_rate,
            metrics.multi_evaluation_barriers,
        );
        for (length, count) in &metrics.ai_streak_histogram {
            *histogram.entry(*length).or_default() += count;
        }
        if !metrics.naturalness_ratings.is_empty() {
            rated_sessions += 1;
        }
        scores.extend(
            metrics
                .naturalness_ratings
                .iter()
                .map(|rating| f64::from(rating.score)),
        );
    }

    let streak_count: u64 = histogram.values().sum();
    let streak_total: u64 = histogram.iter().map(|(length, n)| length * n).sum();

    ReviewAggregate {
        total_sessions: summaries.len(),
        included_sessions,
        rated_sessions,
        decisions,
        pass_rate: (decisions > 0).then(|| passes as f64 / decisions as f64),
        multi_evaluation_barriers: barriers,
        simultaneous_request_rate: (barriers > 0).then(|| simultaneous as f64 / barriers as f64),
        ai_streak_count: streak_count,
        mean_ai_streak: (streak_count > 0).then(|| streak_total as f64 / streak_count as f64),
        max_ai_streak: histogram.keys().next_back().copied(),
        ai_streak_histogram: histogram,
        naturalness_average: (!scores.is_empty())
            .then(|| scores.iter().sum::<f64>() / scores.len() as f64),
    }
}

/// Records a human's naturalness score against an already-saved session and
/// recomputes the stored average. Ratings land in the same list the live
/// `/rate` writes to, so a topic has one rating history wherever it was scored.
pub fn push_rating(
    report: &mut MetricsReport,
    score: u8,
    note: Option<String>,
) -> Result<(), &'static str> {
    if !(1..=5).contains(&score) {
        return Err("naturalness score must be between 1 and 5");
    }
    report
        .naturalness_ratings
        .push(NaturalnessRating { score, note });
    let total: u64 = report
        .naturalness_ratings
        .iter()
        .map(|rating| u64::from(rating.score))
        .sum();
    report.naturalness_average = Some(total as f64 / report.naturalness_ratings.len() as f64);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(
        decisions: u64,
        pass_rate: f64,
        multi_barriers: u64,
        simultaneous_rate: f64,
        streaks: &[(u64, u64)],
        ratings: &[u8],
    ) -> MetricsReport {
        let histogram: BTreeMap<u64, u64> = streaks.iter().copied().collect();
        let count: u64 = histogram.values().sum();
        let total: u64 = histogram.iter().map(|(len, n)| len * n).sum();
        let naturalness_ratings: Vec<NaturalnessRating> = ratings
            .iter()
            .map(|score| NaturalnessRating {
                score: *score,
                note: None,
            })
            .collect();
        let naturalness_average = if naturalness_ratings.is_empty() {
            None
        } else {
            Some(
                naturalness_ratings
                    .iter()
                    .map(|r| f64::from(r.score))
                    .sum::<f64>()
                    / naturalness_ratings.len() as f64,
            )
        };
        MetricsReport {
            decisions,
            pass_rate: (decisions > 0).then_some(pass_rate),
            multi_evaluation_barriers: multi_barriers,
            simultaneous_request_rate: (multi_barriers > 0).then_some(simultaneous_rate),
            ai_streak_count: count,
            mean_ai_streak: (count > 0).then(|| total as f64 / count as f64),
            max_ai_streak: histogram.keys().next_back().copied(),
            ai_streak_histogram: histogram,
            naturalness_average,
            naturalness_ratings,
        }
    }

    fn summary(file: &str, metrics: MetricsReport, excluded: bool) -> SessionSummary {
        SessionSummary {
            file: file.to_owned(),
            saved_unix: 1,
            provider: "subscription CLIs".to_owned(),
            roster: vec!["GPT".to_owned(), "Claude".to_owned()],
            topic: "주제".to_owned(),
            events: 4,
            metrics,
            review: ReviewAnnotation {
                excluded,
                reason: None,
            },
        }
    }

    #[test]
    fn pass_rate_is_weighted_by_decisions_not_averaged_across_sessions() {
        // 2 of 10, then 2 of 2. Averaging the rates would say 60%; the council
        // actually passed 4 of 12 judgements.
        let sessions = [
            summary("a.json", report(10, 0.2, 0, 0.0, &[], &[]), false),
            summary("b.json", report(2, 1.0, 0, 0.0, &[], &[]), false),
        ];

        let total = aggregate(&sessions);

        assert_eq!(total.decisions, 12);
        assert_eq!(total.pass_rate, Some(4.0 / 12.0));
    }

    #[test]
    fn simultaneous_request_rate_is_weighted_by_its_own_barriers() {
        // 1 of 4, then 3 of 3: 4 of 7 barriers had two or more requesters.
        let sessions = [
            summary("a.json", report(8, 0.5, 4, 0.25, &[], &[]), false),
            summary("b.json", report(6, 0.5, 3, 1.0, &[], &[]), false),
        ];

        let total = aggregate(&sessions);

        assert_eq!(total.multi_evaluation_barriers, 7);
        assert_eq!(total.simultaneous_request_rate, Some(4.0 / 7.0));
    }

    #[test]
    fn excluded_sessions_are_left_out_of_the_totals() {
        let sessions = [
            summary("good.json", report(10, 0.2, 0, 0.0, &[], &[4]), false),
            summary("spoiled.json", report(90, 0.9, 0, 0.0, &[], &[1]), true),
        ];

        let total = aggregate(&sessions);

        assert_eq!(total.total_sessions, 2);
        assert_eq!(total.included_sessions, 1);
        assert_eq!(total.decisions, 10);
        assert_eq!(total.pass_rate, Some(0.2));
        assert_eq!(total.naturalness_average, Some(4.0));
    }

    #[test]
    fn streak_histograms_merge_and_mean_and_max_come_from_the_merge() {
        let sessions = [
            summary("a.json", report(4, 0.0, 0, 0.0, &[(1, 2)], &[]), false),
            summary(
                "b.json",
                report(4, 0.0, 0, 0.0, &[(1, 1), (3, 1)], &[]),
                false,
            ),
        ];

        let total = aggregate(&sessions);

        assert_eq!(total.ai_streak_histogram, BTreeMap::from([(1, 3), (3, 1)]));
        assert_eq!(total.ai_streak_count, 4);
        assert_eq!(total.mean_ai_streak, Some(6.0 / 4.0));
        assert_eq!(total.max_ai_streak, Some(3));
    }

    #[test]
    fn naturalness_averages_over_every_rating_not_over_session_averages() {
        // One session rated three times, another rated once. The single 1 must
        // not weigh as much as the other session's whole run of scores.
        let sessions = [
            summary("a.json", report(4, 0.0, 0, 0.0, &[], &[5, 5, 5]), false),
            summary("b.json", report(4, 0.0, 0, 0.0, &[], &[1]), false),
        ];

        let total = aggregate(&sessions);

        assert_eq!(total.naturalness_average, Some(16.0 / 4.0));
        assert_eq!(total.rated_sessions, 2);
    }

    #[test]
    fn an_empty_review_reports_no_rates_rather_than_zero() {
        let total = aggregate(&[]);

        assert_eq!(total.included_sessions, 0);
        assert_eq!(total.pass_rate, None);
        assert_eq!(total.simultaneous_request_rate, None);
        assert_eq!(total.mean_ai_streak, None);
        assert_eq!(total.naturalness_average, None);
    }

    #[test]
    fn the_topic_is_the_first_human_utterance_not_the_first_event() {
        let record: SessionRecord = serde_json::from_value(serde_json::json!({
            "saved_unix": 7,
            "provider": "subscription CLIs",
            "roster": ["Claude", "Grok"],
            "events": [
                { "id": 1, "author": "Claude", "content": "이어가기로 시드된 발언" },
                { "id": 2, "author": "You", "content": "사람이 실제로 물은 주제" }
            ],
            "cycles": [],
            "metrics": report(2, 0.5, 1, 1.0, &[(1, 1)], &[3])
        }))
        .expect("record");

        let summary = summarize("web-session-7.json", &record).expect("summary");

        assert_eq!(summary.topic, "사람이 실제로 물은 주제");
        assert_eq!(summary.events, 2);
        assert_eq!(summary.roster, vec!["Claude", "Grok"]);
        assert!(!summary.review.excluded);
    }

    #[test]
    fn a_sidecar_with_unreadable_metrics_is_skipped_rather_than_defaulted() {
        let record: SessionRecord = serde_json::from_value(serde_json::json!({
            "saved_unix": 7,
            "provider": "mock",
            "roster": ["GPT"],
            "events": [{ "id": 1, "author": "You", "content": "주제" }],
            "cycles": [],
            "metrics": { "decisions": "열두 번" }
        }))
        .expect("record");

        assert!(summarize("broken.json", &record).is_none());
    }
    #[test]
    fn rating_a_past_session_appends_and_recomputes_the_average() {
        let mut metrics = report(4, 0.0, 0, 0.0, &[], &[5, 3]);

        push_rating(&mut metrics, 4, Some("읽어보니 자연스러웠다".to_owned())).expect("accepted");

        assert_eq!(metrics.naturalness_ratings.len(), 3);
        assert_eq!(metrics.naturalness_average, Some(12.0 / 3.0));
        assert_eq!(
            metrics.naturalness_ratings[2].note.as_deref(),
            Some("읽어보니 자연스러웠다")
        );
    }

    #[test]
    fn a_score_outside_one_to_five_is_refused_and_changes_nothing() {
        let mut metrics = report(4, 0.0, 0, 0.0, &[], &[]);

        assert!(push_rating(&mut metrics, 0, None).is_err());
        assert!(push_rating(&mut metrics, 6, None).is_err());

        assert!(metrics.naturalness_ratings.is_empty());
        assert_eq!(metrics.naturalness_average, None);
    }
}
