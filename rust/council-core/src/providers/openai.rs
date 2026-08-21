use async_trait::async_trait;
use reqwest::Client;
use serde_json::{Value, json};

use crate::adapter::{AgentAdapter, CouncilError, CouncilResult};
use crate::model::{AgentId, Intent, RoomEvent, RoomSnapshot};
use crate::prompts::{
    evaluation_input, evaluation_instructions, speaking_input, speaking_instructions,
};

use super::parse_decision;

pub struct OpenAiAdapter {
    client: Client,
    api_key: String,
    model: String,
    endpoint: String,
}

impl OpenAiAdapter {
    pub fn from_env() -> CouncilResult<Self> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| CouncilError::new("OPENAI_API_KEY is required for --provider live"))?;
        let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-5.4-mini".to_owned());
        let base_url = std::env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned());
        Ok(Self {
            client: Client::new(),
            api_key,
            model,
            endpoint: format!("{}/responses", base_url.trim_end_matches('/')),
        })
    }

    async fn create_response(&self, body: Value) -> CouncilResult<Value> {
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|error| CouncilError::provider(AgentId::Gpt, error))?;
        let status = response.status();
        let value: Value = response
            .json()
            .await
            .map_err(|error| CouncilError::provider(AgentId::Gpt, error))?;
        if !status.is_success() {
            return Err(CouncilError::provider(
                AgentId::Gpt,
                format!("HTTP {status}: {}", api_error_message(&value)),
            ));
        }
        Ok(value)
    }
}

#[async_trait]
impl AgentAdapter for OpenAiAdapter {
    fn id(&self) -> AgentId {
        AgentId::Gpt
    }

    async fn evaluate(&self, room: &RoomSnapshot, event: &RoomEvent) -> CouncilResult<Intent> {
        let body = json!({
            "model": self.model,
            "store": false,
            "instructions": evaluation_instructions(self.id()),
            "input": evaluation_input(room),
            "max_output_tokens": 160,
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": "council_intent",
                    "strict": true,
                    "schema": {
                        "type": "object",
                        "properties": {
                            "decision": {
                                "type": "string",
                                "enum": ["PASS", "REQUEST_FLOOR"]
                            },
                            "reason": {"type": "string"}
                        },
                        "required": ["decision", "reason"],
                        "additionalProperties": false
                    }
                }
            }
        });
        let response = self.create_response(body).await?;
        parse_decision(&extract_output_text(&response)?, event.id)
    }

    async fn speak(&self, room: &RoomSnapshot, _intent: &Intent) -> CouncilResult<String> {
        let body = json!({
            "model": self.model,
            "store": false,
            "instructions": speaking_instructions(self.id()),
            "input": speaking_input(room),
            "max_output_tokens": 300
        });
        let response = self.create_response(body).await?;
        let text = extract_output_text(&response)?;
        let speech = text.trim();
        if speech.is_empty() {
            return Err(CouncilError::provider(self.id(), "empty speech response"));
        }
        Ok(speech.to_owned())
    }
}

fn extract_output_text(response: &Value) -> CouncilResult<String> {
    let texts = response
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("output_text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>();
    if texts.is_empty() {
        return Err(CouncilError::provider(
            AgentId::Gpt,
            "response contained no output_text",
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
