use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};

use crate::adapter::{AgentAdapter, CouncilError, CouncilResult};
use crate::model::{AgentId, Intent, RoomEvent, RoomSnapshot};
use crate::prompts::{evaluation_input, evaluation_instructions, speaking_input};

use super::parse_decision;

pub struct AnthropicAdapter {
    environment: super::SeatEnvironment,
    client: Client,
    api_key: String,
    model: String,
    endpoint: String,
}

impl AnthropicAdapter {
    pub fn from_env() -> CouncilResult<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| CouncilError::new("ANTHROPIC_API_KEY is required for --provider live"))?;
        let model =
            std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-sonnet-4-6".to_owned());
        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com/v1".to_owned());
        Ok(Self {
            environment: super::SeatEnvironment::default(),
            client: Client::new(),
            api_key,
            model,
            endpoint: format!("{}/messages", base_url.trim_end_matches('/')),
        })
    }

    async fn create_message(&self, body: Value) -> CouncilResult<Value> {
        let response = self
            .client
            .post(&self.endpoint)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .map_err(|error| CouncilError::provider(AgentId::Claude, error))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(|error| CouncilError::provider(AgentId::Claude, error))?;
        if !status.is_success() {
            return Err(CouncilError::provider(
                AgentId::Claude,
                format!("HTTP {status}: {}", api_error_message(&value)),
            ));
        }
        Ok(value)
    }
}

impl AnthropicAdapter {
    pub fn with_environment(mut self, environment: super::SeatEnvironment) -> Self {
        self.environment = environment;
        self
    }
}

#[async_trait]
impl AgentAdapter for AnthropicAdapter {
    fn model_label(&self) -> Option<String> {
        Some(self.model.clone())
    }

    fn id(&self) -> AgentId {
        AgentId::Claude
    }

    async fn evaluate(&self, room: &RoomSnapshot, event: &RoomEvent) -> CouncilResult<Intent> {
        let body = json!({
            "model": self.model,
            "max_tokens": 160,
            "system": evaluation_instructions(self.id()),
            "messages": [{"role": "user", "content": evaluation_input(room)}]
        });
        let response = self.create_message(body).await?;
        parse_decision(&extract_text(&response)?, event.id)
    }

    async fn speak(&self, room: &RoomSnapshot, _intent: &Intent) -> CouncilResult<String> {
        let body = json!({
            "model": self.model,
            "max_tokens": 300,
            "system": super::speak_instructions(self.id(), &self.environment, super::ToolGrant::None),
            "messages": [{"role": "user", "content": speaking_input(room)}]
        });
        let response = self.create_message(body).await?;
        let text = extract_text(&response)?;
        let speech = text.trim();
        if speech.is_empty() {
            return Err(CouncilError::provider(self.id(), "empty speech response"));
        }
        Ok(speech.to_owned())
    }
}

fn extract_text(response: &Value) -> CouncilResult<String> {
    let texts = response
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>();
    if texts.is_empty() {
        return Err(CouncilError::provider(
            AgentId::Claude,
            "response contained no text block",
        ));
    }
    Ok(texts.join("\n"))
}

fn api_error_message(value: &Value) -> String {
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("unknown API error")
        .to_owned()
}
