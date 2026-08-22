mod anthropic;
mod mock;
mod openai;
mod subscription;

pub use anthropic::AnthropicAdapter;
pub use mock::MockAdapter;
pub use openai::OpenAiAdapter;
pub use subscription::{
    AntigravityCliAdapter, ClaudeCliAdapter, CodexCliAdapter, GrokCliAdapter, check_subscription,
    cli_binary,
};

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::adapter::{AgentAdapter, CouncilError, CouncilResult};
use crate::model::{AgentId, Decision, Intent};

/// Tokens and cost one CLI call reported, as observed from its JSON output.
/// This is session-local observation — the CLIs do not expose remaining
/// subscription quota headlessly.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct UsageSample {
    pub agent: AgentId,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

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

/// One seat request: which agent joins, optionally pinned to a model/effort.
/// `None` falls back to the seat's *_SUBSCRIPTION_MODEL / _EFFORT env vars.
#[derive(Clone, Debug)]
pub struct AgentSpec {
    pub agent: AgentId,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl AgentSpec {
    pub fn defaults(agent: AgentId) -> Self {
        Self {
            agent,
            model: None,
            effort: None,
        }
    }
}

pub fn build_adapters(
    kind: ProviderKind,
    agents: &[AgentId],
) -> CouncilResult<Vec<Arc<dyn AgentAdapter>>> {
    let specs: Vec<AgentSpec> = agents.iter().copied().map(AgentSpec::defaults).collect();
    build_adapters_with(kind, &specs, None)
}

pub fn build_adapters_with(
    kind: ProviderKind,
    specs: &[AgentSpec],
    usage_sink: Option<&tokio::sync::mpsc::UnboundedSender<UsageSample>>,
) -> CouncilResult<Vec<Arc<dyn AgentAdapter>>> {
    let roster: Vec<AgentId> = specs.iter().map(|spec| spec.agent).collect();
    let mut adapters: Vec<Arc<dyn AgentAdapter>> = Vec::new();
    for spec in specs {
        let model = spec.model.clone();
        let effort = spec.effort.clone();
        let sink = usage_sink.cloned();
        let adapter: Arc<dyn AgentAdapter> = match (kind, spec.agent) {
            (ProviderKind::Mock, agent) => Arc::new(MockAdapter::new(agent, &roster)),
            (ProviderKind::Subscription, AgentId::Gpt) => {
                Arc::new(CodexCliAdapter::with_config(model, effort, sink)?)
            }
            (ProviderKind::Subscription, AgentId::Claude) => {
                Arc::new(ClaudeCliAdapter::with_config(model, effort, sink)?)
            }
            (ProviderKind::Subscription, AgentId::Gemini) => {
                Arc::new(AntigravityCliAdapter::with_config(model, effort, sink)?)
            }
            (ProviderKind::Subscription, AgentId::Grok) => {
                Arc::new(GrokCliAdapter::with_config(model, effort, sink)?)
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
