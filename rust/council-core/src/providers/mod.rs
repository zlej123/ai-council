mod anthropic;
mod mock;
mod openai;
mod subscription;

pub use anthropic::AnthropicAdapter;
pub use mock::MockAdapter;
pub use openai::OpenAiAdapter;
pub use subscription::{AntigravityCliAdapter, ClaudeCliAdapter, CodexCliAdapter, GrokCliAdapter};

use std::sync::Arc;

use serde::Deserialize;

use crate::adapter::{AgentAdapter, CouncilError, CouncilResult};
use crate::model::{AgentId, Decision, Intent};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderKind {
    Mock,
    Subscription,
    Live,
}

impl ProviderKind {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "mock" => Some(Self::Mock),
            "subscription" => Some(Self::Subscription),
            "live" => Some(Self::Live),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Mock => "mock",
            Self::Subscription => "subscription CLIs",
            Self::Live => "live APIs",
        }
    }
}

pub fn build_adapters(
    kind: ProviderKind,
    agents: &[AgentId],
) -> CouncilResult<Vec<Arc<dyn AgentAdapter>>> {
    let mut adapters: Vec<Arc<dyn AgentAdapter>> = Vec::new();
    for agent in agents {
        let adapter: Arc<dyn AgentAdapter> = match (kind, agent) {
            (ProviderKind::Mock, _) => Arc::new(MockAdapter::new(*agent)),
            (ProviderKind::Subscription, AgentId::Gpt) => {
                Arc::new(CodexCliAdapter::subscription()?)
            }
            (ProviderKind::Subscription, AgentId::Claude) => {
                Arc::new(ClaudeCliAdapter::subscription()?)
            }
            (ProviderKind::Subscription, AgentId::Gemini) => {
                Arc::new(AntigravityCliAdapter::subscription()?)
            }
            (ProviderKind::Subscription, AgentId::Grok) => {
                Arc::new(GrokCliAdapter::subscription()?)
            }
            (ProviderKind::Live, AgentId::Gpt) => Arc::new(OpenAiAdapter::from_env()?),
            (ProviderKind::Live, AgentId::Claude) => Arc::new(AnthropicAdapter::from_env()?),
            (ProviderKind::Live, other) => {
                return Err(CouncilError::new(format!(
                    "live (API) mode has no adapter for {other}; use subscription mode"
                )));
            }
        };
        adapters.push(adapter);
    }
    Ok(adapters)
}

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
