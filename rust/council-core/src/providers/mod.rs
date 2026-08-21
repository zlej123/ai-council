mod anthropic;
mod mock;
mod openai;
mod subscription;

pub use anthropic::AnthropicAdapter;
pub use mock::MockAdapter;
pub use openai::OpenAiAdapter;
pub use subscription::{AntigravityCliAdapter, ClaudeCliAdapter, CodexCliAdapter, GrokCliAdapter};

use serde::Deserialize;

use crate::adapter::{CouncilError, CouncilResult};
use crate::model::{Decision, Intent};

#[derive(Debug, Deserialize)]
struct DecisionPayload {
    decision: Decision,
    #[serde(default)]
    reason: Option<String>,
}

fn parse_decision(text: &str, event_id: u64) -> CouncilResult<Intent> {
    let trimmed = text.trim();
    let json = if trimmed.starts_with("```") {
        trimmed
            .strip_prefix("```json")
            .or_else(|| trimmed.strip_prefix("```"))
            .and_then(|value| value.strip_suffix("```"))
            .map(str::trim)
            .ok_or_else(|| CouncilError::new("malformed fenced decision JSON"))?
    } else {
        trimmed
    };
    let payload: DecisionPayload = serde_json::from_str(json)?;
    Ok(Intent {
        basis_event_id: event_id,
        decision: payload.decision,
        reason: payload.reason.filter(|reason| !reason.trim().is_empty()),
    })
}
