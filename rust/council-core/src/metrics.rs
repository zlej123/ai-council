use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::Decision;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NaturalnessRating {
    pub score: u8,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Metrics {
    decision_count: u64,
    pass_count: u64,
    dual_evaluation_barriers: u64,
    simultaneous_request_barriers: u64,
    ai_streaks: Vec<u64>,
    ratings: Vec<NaturalnessRating>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MetricsReport {
    pub decisions: u64,
    pub pass_rate: Option<f64>,
    pub dual_evaluation_barriers: u64,
    pub simultaneous_request_rate: Option<f64>,
    pub ai_streak_count: u64,
    pub mean_ai_streak: Option<f64>,
    pub max_ai_streak: Option<u64>,
    pub ai_streak_histogram: BTreeMap<u64, u64>,
    pub naturalness_average: Option<f64>,
    pub naturalness_ratings: Vec<NaturalnessRating>,
}

impl Metrics {
    pub fn record_barrier(&mut self, decisions: &[Decision]) {
        self.decision_count += decisions.len() as u64;
        self.pass_count += decisions
            .iter()
            .filter(|decision| **decision == Decision::Pass)
            .count() as u64;

        if decisions.len() == 2 {
            self.dual_evaluation_barriers += 1;
            if decisions
                .iter()
                .all(|decision| *decision == Decision::RequestFloor)
            {
                self.simultaneous_request_barriers += 1;
            }
        }
    }

    pub fn record_ai_streak(&mut self, length: u64) {
        self.ai_streaks.push(length);
    }

    pub fn rate_naturalness(
        &mut self,
        score: u8,
        note: Option<String>,
    ) -> Result<(), &'static str> {
        if !(1..=5).contains(&score) {
            return Err("naturalness score must be between 1 and 5");
        }
        self.ratings.push(NaturalnessRating { score, note });
        Ok(())
    }

    pub fn report(&self) -> MetricsReport {
        let pass_rate = ratio(self.pass_count, self.decision_count);
        let simultaneous_request_rate = ratio(
            self.simultaneous_request_barriers,
            self.dual_evaluation_barriers,
        );
        let mean_ai_streak = if self.ai_streaks.is_empty() {
            None
        } else {
            Some(self.ai_streaks.iter().sum::<u64>() as f64 / self.ai_streaks.len() as f64)
        };
        let max_ai_streak = self.ai_streaks.iter().copied().max();
        let mut ai_streak_histogram = BTreeMap::new();
        for length in &self.ai_streaks {
            *ai_streak_histogram.entry(*length).or_insert(0) += 1;
        }
        let naturalness_average = if self.ratings.is_empty() {
            None
        } else {
            Some(
                self.ratings
                    .iter()
                    .map(|rating| u64::from(rating.score))
                    .sum::<u64>() as f64
                    / self.ratings.len() as f64,
            )
        };

        MetricsReport {
            decisions: self.decision_count,
            pass_rate,
            dual_evaluation_barriers: self.dual_evaluation_barriers,
            simultaneous_request_rate,
            ai_streak_count: self.ai_streaks.len() as u64,
            mean_ai_streak,
            max_ai_streak,
            ai_streak_histogram,
            naturalness_average,
            naturalness_ratings: self.ratings.clone(),
        }
    }
}

fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator != 0).then_some(numerator as f64 / denominator as f64)
}
