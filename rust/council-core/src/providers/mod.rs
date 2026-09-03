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
use crate::prompts::SeatTools;

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

/// What a seat's turns are allowed to touch, shared by every seat in the room.
/// `artifacts: None` means a tool-less (v1-style) room; `Some` turns the v2
/// speaking-turn tools on, with writes confined to that folder.
#[derive(Clone, Debug, Default)]
pub struct SeatEnvironment {
    pub language: Option<String>,
    pub workspace: Option<std::path::PathBuf>,
    pub artifacts: Option<std::path::PathBuf>,
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

/// The one place that decides what a seat may do on a speaking turn. Both
/// the prompt (`speak_instructions`) and each adapter's CLI arguments read
/// this, so the model is told exactly what the child process can do — the
/// per-seat table in EXPERIMENT.md §7 is this function in prose.
///
/// Only the subscription CLIs run tools. The grant is what each CLI can
/// CONFINE, not what it could be talked into:
/// - Codex: web, read (unconfined — the sandbox only gates writes), write and
///   run inside the artifacts folder (workspace-write, cwd = artifacts).
/// - Claude: web and read inside a `--restricted` boundary. Write only when
///   there is no workspace, because that boundary is one combined read+write
///   set: with a workspace present, a Write tool could reach it. No run —
///   Bash is not a file tool and `--restricted` would not confine it.
/// - Gemini (Antigravity): web only. Nothing mechanical confines its file
///   tools, so none are granted.
/// - Grok: everything, under a kernel sandbox that marks the workspace
///   read-only and the artifacts folder writable.
pub fn seat_tools(kind: ProviderKind, agent: AgentId, environment: &SeatEnvironment) -> SeatTools {
    if environment.artifacts.is_none() || kind != ProviderKind::Subscription {
        return SeatTools::NONE;
    }
    match agent {
        AgentId::Gpt => SeatTools {
            web: true,
            read: true,
            write: true,
            run: true,
        },
        AgentId::Claude => SeatTools {
            web: true,
            read: true,
            write: environment.workspace.is_none(),
            run: false,
        },
        AgentId::Gemini => SeatTools {
            web: true,
            ..SeatTools::NONE
        },
        AgentId::Grok => SeatTools {
            web: true,
            read: true,
            write: true,
            run: true,
        },
    }
}

/// The speaking-turn system prompt for a seat in this environment: language
/// directive plus a tool context that matches what the runtime grants.
pub(crate) fn speak_instructions(
    agent: AgentId,
    environment: &SeatEnvironment,
    tools: SeatTools,
) -> String {
    let context = environment.artifacts.as_ref().and_then(|artifacts| {
        crate::prompts::tool_context(
            tools,
            environment
                .workspace
                .as_ref()
                .and_then(|path| path.to_str()),
            &artifacts.to_string_lossy(),
        )
    });
    crate::prompts::speaking_instructions_with(
        agent,
        environment.language.as_deref(),
        context.as_deref(),
    )
}

pub fn build_adapters(
    kind: ProviderKind,
    agents: &[AgentId],
    environment: &SeatEnvironment,
) -> CouncilResult<Vec<Arc<dyn AgentAdapter>>> {
    let specs: Vec<AgentSpec> = agents.iter().copied().map(AgentSpec::defaults).collect();
    build_adapters_with(kind, &specs, None, environment)
}

pub fn build_adapters_with(
    kind: ProviderKind,
    specs: &[AgentSpec],
    usage_sink: Option<&tokio::sync::mpsc::UnboundedSender<UsageSample>>,
    environment: &SeatEnvironment,
) -> CouncilResult<Vec<Arc<dyn AgentAdapter>>> {
    let roster: Vec<AgentId> = specs.iter().map(|spec| spec.agent).collect();
    let mut adapters: Vec<Arc<dyn AgentAdapter>> = Vec::new();
    for spec in specs {
        let model = spec.model.clone();
        let effort = spec.effort.clone();
        let seat_env = environment.clone();
        let sink = usage_sink.cloned();
        let adapter: Arc<dyn AgentAdapter> =
            match (kind, spec.agent) {
                (ProviderKind::Mock, agent) => Arc::new(MockAdapter::new(agent, &roster)),
                (ProviderKind::Subscription, AgentId::Gpt) => Arc::new(
                    CodexCliAdapter::with_config(model, effort, seat_env.clone(), sink)?,
                ),
                (ProviderKind::Subscription, AgentId::Claude) => Arc::new(
                    ClaudeCliAdapter::with_config(model, effort, seat_env.clone(), sink)?,
                ),
                (ProviderKind::Subscription, AgentId::Gemini) => Arc::new(
                    AntigravityCliAdapter::with_config(model, effort, seat_env.clone(), sink)?,
                ),
                (ProviderKind::Subscription, AgentId::Grok) => Arc::new(
                    GrokCliAdapter::with_config(model, effort, seat_env.clone(), sink)?,
                ),
                (ProviderKind::Live, AgentId::Gpt) => {
                    Arc::new(OpenAiAdapter::from_env()?.with_environment(seat_env))
                }
                (ProviderKind::Live, AgentId::Claude) => {
                    Arc::new(AnthropicAdapter::from_env()?.with_environment(seat_env))
                }
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
